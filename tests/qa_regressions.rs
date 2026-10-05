//! Regressions from live QA of the apps: a pause stops all network use within
//! about a second (without spending retries, and with the resume record true
//! to what is on disk), one slow connection doesn't hold up the start, a
//! connection the server resets is a lost connection rather than a TLS
//! failure, said in plain words, and renaming uses the job's title rather
//! than its de-duplicated folder.

mod common;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::config::UsenetConfig;
use dl_nzb::engine::{Engine, ErrorKind, JobEvent, JobRequest, Outcome, Preflight};
use dl_nzb::DlNzbError;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};

// --- A slow, observable NNTP server --------------------------------------------

/// An NNTP server whose article bodies trickle out (a slow link), which can
/// hold back one connection's greeting, and which keeps count of what it
/// sends and of the connections open.
struct SlowServer {
    articles: HashMap<String, Vec<u8>>,
    /// Body bytes per write, and the wait after each.
    chunk: usize,
    chunk_delay: Duration,
    /// The connection accepted `n`th (from 0) waits this long to greet.
    slow_greeting: Option<(usize, Duration)>,
    accepted: AtomicUsize,
    open: AtomicUsize,
    /// Bodies being sent right now.
    streaming: AtomicUsize,
    body_bytes: AtomicU64,
    requests: Mutex<HashMap<String, usize>>,
    /// Articles whose whole body was sent.
    served: Mutex<HashSet<String>>,
    first_body: Mutex<Option<Instant>>,
}

impl SlowServer {
    fn new(articles: Vec<MockArticle>) -> Self {
        Self {
            articles: articles
                .into_iter()
                .map(|a| (a.message_id, a.body))
                .collect(),
            chunk: usize::MAX,
            chunk_delay: Duration::ZERO,
            slow_greeting: None,
            accepted: AtomicUsize::new(0),
            open: AtomicUsize::new(0),
            streaming: AtomicUsize::new(0),
            body_bytes: AtomicU64::new(0),
            requests: Mutex::default(),
            served: Mutex::default(),
            first_body: Mutex::default(),
        }
    }

    /// Bodies go out `chunk` bytes every `delay`.
    fn trickle(mut self, chunk: usize, delay: Duration) -> Self {
        self.chunk = chunk;
        self.chunk_delay = delay;
        self
    }

    fn requests(&self, id: &str) -> usize {
        self.requests.lock().unwrap().get(id).copied().unwrap_or(0)
    }

    fn served(&self) -> HashSet<String> {
        self.served.lock().unwrap().clone()
    }

    async fn spawn(self: &Arc<Self>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = self.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let n = server.accepted.fetch_add(1, SeqCst);
                let server = server.clone();
                tokio::spawn(async move {
                    server.open.fetch_add(1, SeqCst);
                    let _ = server.session(sock, n).await;
                    server.open.fetch_sub(1, SeqCst);
                });
            }
        });
        port
    }

    async fn session(&self, sock: TcpStream, n: usize) -> std::io::Result<()> {
        if let Some((slow, delay)) = self.slow_greeting {
            if slow == n {
                tokio::time::sleep(delay).await;
            }
        }
        let (rd, mut wr) = sock.into_split();
        let mut reader = BufReader::new(rd);
        wr.write_all(b"200 Welcome\r\n").await?;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            let mut parts = line.split_whitespace();
            let cmd = parts.next().unwrap_or("").to_ascii_uppercase();
            let arg = parts.next().unwrap_or("");
            match cmd.as_str() {
                "AUTHINFO" if arg.eq_ignore_ascii_case("USER") => {
                    wr.write_all(b"381 More authentication required\r\n")
                        .await?
                }
                "AUTHINFO" => wr.write_all(b"281 Authentication accepted\r\n").await?,
                "GROUP" => wr.write_all(b"211 1 1 1 group\r\n").await?,
                "DATE" => wr.write_all(b"111 20260101000000\r\n").await?,
                "QUIT" => {
                    wr.write_all(b"205 Bye\r\n").await?;
                    return Ok(());
                }
                "BODY" => {
                    let id = arg.trim_start_matches('<').trim_end_matches('>');
                    *self
                        .requests
                        .lock()
                        .unwrap()
                        .entry(id.to_string())
                        .or_default() += 1;
                    self.first_body
                        .lock()
                        .unwrap()
                        .get_or_insert_with(Instant::now);
                    let Some(body) = self.articles.get(id) else {
                        wr.write_all(b"430 No such article\r\n").await?;
                        continue;
                    };
                    wr.write_all(b"222 0 body follows\r\n").await?;
                    self.streaming.fetch_add(1, SeqCst);
                    let sent = self.stream(&mut wr, body).await;
                    self.streaming.fetch_sub(1, SeqCst);
                    sent?;
                    wr.write_all(b".\r\n").await?;
                    self.served.lock().unwrap().insert(id.to_string());
                }
                _ => wr.write_all(b"500 What?\r\n").await?,
            }
        }
    }

    async fn stream(&self, wr: &mut OwnedWriteHalf, body: &[u8]) -> std::io::Result<()> {
        for piece in body.chunks(self.chunk) {
            wr.write_all(piece).await?;
            self.body_bytes.fetch_add(piece.len() as u64, SeqCst);
            if !self.chunk_delay.is_zero() {
                tokio::time::sleep(self.chunk_delay).await;
            }
        }
        Ok(())
    }
}

fn config(port: u16, dir: &Path, connections: u16) -> dl_nzb::Config {
    let mut config = make_config("127.0.0.1", port, dir.into());
    config.usenet.connections = connections;
    config.tuning.max_concurrent_connections = connections as usize;
    config
}

fn request(nzb: &Path, out: &Path) -> JobRequest {
    JobRequest {
        preflight: Preflight::Never,
        ..JobRequest::new(nzb, out)
    }
}

/// Wait (up to 20 s) until `done`.
async fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A file of `count` articles of 96 kB whose bodies take about half a second
/// each on the wire. The NZB declares 1.1 MB per article, which keeps each
/// connection's window at 4 requests.
struct SlowFile {
    server: Arc<SlowServer>,
    ids: Vec<String>,
    full: Vec<u8>,
    temp: tempfile::TempDir,
    nzb: std::path::PathBuf,
}

const SEGMENT: usize = 96_000;

async fn slow_file(count: usize) -> SlowFile {
    let (articles, ids, _, full) = make_file_articles("film.bin", "p", count, SEGMENT);
    let sizes = vec![1_100_000u64; count];
    let server = Arc::new(SlowServer::new(articles).trickle(4096, Duration::from_millis(20)));
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "film", &[("film.bin", &ids, &sizes)]);
    SlowFile {
        server,
        ids,
        full,
        temp,
        nzb,
    }
}

// --- Pause ---------------------------------------------------------------------

/// On a slow link a pause used to let every connection drain its window of
/// requests (bytes kept arriving ~25 s after "Paused") and then kept the
/// sockets open in the pool. Now nothing more arrives a second after the
/// pause, every connection is closed, and resuming finishes the job with
/// every article right.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pause_stops_the_network_within_a_second() {
    let count = 12;
    let f = slow_file(count).await;
    let port = f.server.spawn().await;
    let out = f.temp.path().join("Film");
    let engine = Engine::new(config(port, f.temp.path(), 2)).unwrap();
    let recorder = Arc::new(Recorder::default());
    let job = engine.start(request(&f.nzb, &out), recorder.clone());

    // Paused from the start: no socket stays open (not even the one the job
    // checked the server with) and nothing is asked for.
    job.pause();
    recorder
        .wait_for(|e| matches!(e, JobEvent::Progress(p) if p.paused))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        f.server.open.load(SeqCst),
        0,
        "a connection open while paused"
    );
    assert!(f.ids.iter().all(|id| f.server.requests(id) == 0));
    job.resume();

    until("both connections sending bodies", || {
        f.server.streaming.load(SeqCst) >= 2
    })
    .await;
    job.pause();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (sent, open) = (f.server.body_bytes.load(SeqCst), f.server.open.load(SeqCst));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        f.server.body_bytes.load(SeqCst),
        sent,
        "body bytes still flowing more than a second after the pause"
    );
    assert_eq!(open, 0, "connections left open a second after the pause");
    assert!(f.server.served().len() < count, "paused too late to test");
    assert!(!job.is_finished());
    recorder
        .wait_for(|e| {
            matches!(e, JobEvent::Progress(p)
                if p.paused && p.speed_bps == 0.0 && p.eta_secs.is_none())
        })
        .await;

    job.resume();
    let summary = wait(&job, 60).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(summary.articles_failed, 0);
    assert_eq!(std::fs::read(out.join("film.bin")).unwrap(), f.full);
    for id in &f.ids {
        let asked = f.server.requests(id);
        assert!((1..=2).contains(&asked), "{id} requested {asked} times");
    }
}

/// The articles a pause abandons go back to the queue as they were: pausing
/// more often than an article's retry budget allows (5 connection-level
/// retries here) never gives it up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pausing_spends_no_retries() {
    let f = slow_file(3).await;
    let port = f.server.spawn().await;
    let out = f.temp.path().join("Film");
    let mut cfg = config(port, f.temp.path(), 1);
    cfg.usenet.retry_attempts = 1;
    let engine = Engine::new(cfg).unwrap();
    let job = engine.start(request(&f.nzb, &out), Arc::new(Recorder::default()));

    for _ in 0..7 {
        until("a body on the wire", || f.server.streaming.load(SeqCst) > 0).await;
        job.pause();
        until("the connection to close", || {
            f.server.open.load(SeqCst) == 0
        })
        .await;
        job.resume();
    }
    let summary = wait(&job, 60).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(summary.articles_failed, 0);
    assert_eq!(std::fs::read(out.join("film.bin")).unwrap(), f.full);
    assert!(f.server.requests(&f.ids[0]) >= 7);
}

/// The articles recorded in the resume sidecar (`done` ranges per file).
fn recorded_articles(dir: &Path) -> BTreeSet<usize> {
    let text = std::fs::read_to_string(dir.join(".dl-nzb-job.json")).unwrap();
    let doc: dl_nzb::serde_json::Value = dl_nzb::serde_json::from_str(&text).unwrap();
    let mut done = BTreeSet::new();
    for range in doc["files"][0]["done"].as_array().unwrap() {
        let first = range[0].as_u64().unwrap() as usize;
        let last = range[1].as_u64().unwrap() as usize;
        done.extend(first..=last);
    }
    done
}

/// A second after a pause the sidecar records exactly the articles in the
/// `.partial` file: none of the requests the pause abandoned, and every
/// article written just before it (the sidecar used to lag up to 2 s, which
/// a suspended app may never get). A resume fetches none of those again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pause_leaves_the_resume_record_true_to_the_disk() {
    let count = 12;
    let f = slow_file(count).await;
    let port = f.server.spawn().await;
    let out = f.temp.path().join("Film");
    let engine = Engine::new(config(port, f.temp.path(), 2)).unwrap();
    let job = engine.start(request(&f.nzb, &out), Arc::new(Recorder::default()));

    until("a few articles", || f.server.served().len() >= 3).await;
    job.pause();
    tokio::time::sleep(Duration::from_secs(1)).await;

    let recorded = recorded_articles(&out);
    let partial = std::fs::read(out.join("film.bin.partial")).unwrap();
    let on_disk: BTreeSet<usize> = (0..count)
        .filter(|k| {
            let region = &partial[k * SEGMENT..(k + 1) * SEGMENT];
            if region == &f.full[k * SEGMENT..(k + 1) * SEGMENT] {
                return true;
            }
            assert!(region.iter().all(|&b| b == 0), "article {k} is half there");
            false
        })
        .collect();
    assert!(!recorded.is_empty());
    assert_eq!(recorded, on_disk, "the sidecar disagrees with the file");
    assert!(on_disk.len() < count, "paused too late to test");
    let served = f.server.served();
    for k in &recorded {
        assert!(served.contains(&f.ids[*k]), "article {k} recorded unsent");
    }

    job.stop();
    let stopped = wait(&job, 10).await;
    assert!(stopped.resumable, "{stopped:?}");
    let summary = wait(
        &engine.start(request(&f.nzb, &out), Arc::new(Recorder::default())),
        60,
    )
    .await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(std::fs::read(out.join("film.bin")).unwrap(), f.full);
    for k in &recorded {
        assert_eq!(
            f.server.requests(&f.ids[*k]),
            1,
            "article {k} fetched again"
        );
    }
}

// --- Start ---------------------------------------------------------------------

/// Work starts with the first connection up: one connection whose greeting
/// takes 10 s used to hold the whole job at "connecting" until it came up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_slow_connection_does_not_hold_up_the_start() {
    let (articles, ids, sizes, full) = make_file_articles("doc.bin", "s", 8, 20_000);
    let mut server = SlowServer::new(articles);
    // Connection 0 is the job's own check; 1 is the first a worker opens.
    server.slow_greeting = Some((1, Duration::from_secs(10)));
    let server = Arc::new(server);
    let port = server.spawn().await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "doc", &[("doc.bin", &ids, &sizes)]);
    let out = temp.path().join("Doc");
    let engine = Engine::new(config(port, temp.path(), 4)).unwrap();

    let started = Instant::now();
    let job = engine.start(request(&nzb, &out), Arc::new(Recorder::default()));
    let summary = wait(&job, 30).await;
    let first = server
        .first_body
        .lock()
        .unwrap()
        .expect("no article asked for");
    assert!(
        first.duration_since(started) < Duration::from_secs(2),
        "first article after {:?}",
        first.duration_since(started)
    );
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the job waited {:?} for the slow connection",
        started.elapsed()
    );
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(std::fs::read(out.join("doc.bin")).unwrap(), full);
}

// --- Error kinds and messages ------------------------------------------------

/// A listener that runs `script` on every connection it accepts.
async fn listener_doing<F, Fut>(script: F) -> u16
where
    F: Fn(TcpStream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(script(sock));
        }
    });
    port
}

/// Close `sock` with a reset (RST) rather than an orderly FIN.
fn reset(sock: TcpStream) {
    socket2::SockRef::from(&sock)
        .set_linger(Some(Duration::ZERO))
        .unwrap();
    drop(sock);
}

fn server(port: u16, ssl: bool) -> UsenetConfig {
    let mut usenet = make_config("127.0.0.1", port, ".".into()).usenet;
    usenet.ssl = ssl;
    usenet
}

async fn check(port: u16, ssl: bool) -> DlNzbError {
    Engine::test_connection(&server(port, ssl))
        .await
        .expect_err("the connection must fail")
}

/// Plain sentences only: no library wording in parentheses.
fn assert_plain(message: &str) {
    assert!(
        !message.contains('(') && !message.contains(')') && message.ends_with('.'),
        "{message}"
    );
}

/// The server resets the connection as soon as it is up: a lost connection,
/// not a protocol or file error.
#[tokio::test]
async fn a_reset_connection_is_a_lost_connection() {
    let port = listener_doing(|sock| async move { reset(sock) }).await;
    let err = check(port, false).await;
    assert_eq!(err.kind(), ErrorKind::Connect, "{err}");
    assert_eq!(err.user_message(), "The connection to 127.0.0.1 was lost.");
}

/// The QA case: the server resets the connection in the middle of the TLS
/// handshake. That was "Couldn't Connect Securely ... (connection closed via
/// error)"; it is a lost connection.
#[tokio::test]
async fn a_reset_during_the_tls_handshake_is_not_a_tls_failure() {
    let port = listener_doing(|mut sock| async move {
        let mut hello = [0u8; 64];
        let _ = sock.read(&mut hello).await;
        reset(sock);
    })
    .await;
    let err = check(port, true).await;
    assert_eq!(err.kind(), ErrorKind::Connect, "{err}");
    assert_eq!(err.user_message(), "The connection to 127.0.0.1 was lost.");

    // A job against it fails the same way (the queue pauses on Connect).
    let temp = tempfile::tempdir().unwrap();
    let (_, ids, sizes, _) = make_file_articles("x.bin", "x", 1, 1_000);
    let nzb = write_nzb(temp.path(), "x", &[("x.bin", &ids, &sizes)]);
    let mut cfg = config(port, temp.path(), 2);
    cfg.usenet.ssl = true;
    let engine = Engine::new(cfg).unwrap();
    let summary = wait(
        &engine.start(
            request(&nzb, &temp.path().join("X")),
            Arc::new(Recorder::default()),
        ),
        30,
    )
    .await;
    assert_eq!(summary.outcome, Outcome::Failed);
    assert_eq!(summary.error_kind, Some(ErrorKind::Connect));
    assert_eq!(
        summary.message.as_deref(),
        Some("The connection to 127.0.0.1 was lost.")
    );
}

/// A handshake that fails on its own terms (here the server answers the
/// client's hello with a fatal `handshake_failure` alert, and keeps the
/// connection open) is still a TLS failure, in a plain sentence.
#[tokio::test]
async fn a_failed_tls_handshake_is_a_tls_failure_in_plain_words() {
    let port = listener_doing(|mut sock| async move {
        let mut hello = [0u8; 64];
        let _ = sock.read(&mut hello).await;
        // Alert record, TLS 1.2, 2 bytes: fatal (2), handshake_failure (40).
        let _ = sock
            .write_all(&[0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x28])
            .await;
        let mut sink = Vec::new();
        let _ = sock.read_to_end(&mut sink).await;
    })
    .await;
    let err = check(port, true).await;
    assert_eq!(err.kind(), ErrorKind::Tls, "{err}");
    assert_eq!(
        err.user_message(),
        "Could not make a secure connection to 127.0.0.1."
    );
}

/// "502 Too many connections" instead of a welcome: the server refused the
/// connection (it used to be an "unexpected response" with the reply in
/// parentheses).
#[tokio::test]
async fn a_refusing_greeting_is_a_refused_connection() {
    let port = listener_doing(|mut sock| async move {
        let _ = sock.write_all(b"502 Too many connections\r\n").await;
    })
    .await;
    let err = check(port, false).await;
    assert_eq!(err.kind(), ErrorKind::Connect, "{err}");
    assert_eq!(
        err.user_message(),
        format!("127.0.0.1 refused the connection on port {port}.")
    );

    // Nothing listening at all.
    let err = check(pick_free_port(), false).await;
    assert_eq!(err.kind(), ErrorKind::Connect, "{err}");
    assert_plain(&err.user_message());
}

// --- Renaming --------------------------------------------------------------------

/// An obfuscated main file, one article, in a job folder whose name the app
/// de-duplicated ("Name 2").
struct Obfuscated {
    temp: tempfile::TempDir,
    nzb: std::path::PathBuf,
    out: std::path::PathBuf,
    port: u16,
}

const SCRAMBLED: &str = "a1b2c3d4e5f6a7b8c9d0.mkv";

async fn obfuscated(missing_second: bool) -> Obfuscated {
    let (articles, ids, sizes, _) = make_file_articles(SCRAMBLED, "o", 2, 30_000);
    let state = Arc::new(MockServerState {
        articles,
        missing_ids: if missing_second {
            vec![ids[1].clone()]
        } else {
            Vec::new()
        },
        ..Default::default()
    });
    let port = spawn_server(state).await;
    let temp = tempfile::tempdir().unwrap();
    // The NZB is named "Name.nzb" and has no title of its own.
    let nzb = write_nzb(temp.path(), "Name", &[(SCRAMBLED, &ids, &sizes)]);
    let out = temp.path().join("Name 2");
    Obfuscated {
        temp,
        nzb,
        out,
        port,
    }
}

fn renaming(port: u16, dir: &Path, on: bool) -> dl_nzb::Config {
    let mut cfg = config(port, dir, 2);
    cfg.post_processing.deobfuscate_file_names = on;
    cfg
}

/// The QA case: "Download Again" put the job in "Name 2", and renaming called
/// the main file "Name 2.mkv". It is named after the request's title.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renaming_uses_the_title_not_the_deduplicated_folder() {
    let o = obfuscated(false).await;
    let engine = Engine::new(renaming(o.port, o.temp.path(), true)).unwrap();
    let request = JobRequest {
        title: Some("Big Buck Bunny".into()),
        ..request(&o.nzb, &o.out)
    };
    let summary = wait(&engine.start(request, Arc::new(Recorder::default())), 30).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert!(
        o.out.join("Big Buck Bunny.mkv").is_file(),
        "{:?}",
        summary.files
    );
    assert_eq!(summary.files_renamed, 1);
}

/// Without a title, the NZB's (its file name here), never the folder's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renaming_without_a_title_uses_the_nzbs() {
    let o = obfuscated(false).await;
    let engine = Engine::new(renaming(o.port, o.temp.path(), true)).unwrap();
    let summary = wait(
        &engine.start(request(&o.nzb, &o.out), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert!(o.out.join("Name.mkv").is_file(), "{:?}", summary.files);
    assert!(!o.out.join("Name 2.mkv").exists());
}

/// `reprocess` has no request: it uses the title `start()` recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reprocess_renames_after_the_recorded_title() {
    // An article is missing (and there is no PAR2), so the job fails and
    // keeps its sidecar; renaming is off for it.
    let o = obfuscated(true).await;
    let engine = Engine::new(renaming(o.port, o.temp.path(), false)).unwrap();
    let request = JobRequest {
        title: Some("Name".into()),
        ..request(&o.nzb, &o.out)
    };
    let first = wait(&engine.start(request, Arc::new(Recorder::default())), 30).await;
    assert_eq!(first.outcome, Outcome::Failed, "{:?}", first.message);
    assert!(o.out.join(SCRAMBLED).is_file());

    engine.update_config(renaming(o.port, o.temp.path(), true));
    let again = wait(
        &engine.reprocess(o.out.clone(), Vec::new(), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert!(o.out.join("Name.mkv").is_file(), "{:?}", again.files);
    assert!(!o.out.join("Name 2.mkv").exists());
}
