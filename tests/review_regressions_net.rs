//! Regression tests for the network side: hostile NZB text never reaches the
//! server as extra commands, hostile articles can't grow a file past its size
//! or buffer without bound, a provider allowing fewer connections than
//! configured doesn't cost any articles, and a `STAT` the server refuses is
//! "unknown", not "missing".

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::engine::{Engine, JobRequest, Outcome, Preflight};
use dl_nzb::nntp::{ArticleOutcome, AsyncNntpConnection};
use dl_nzb::Nzb;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

// --- A scripted NNTP server ---------------------------------------------------

/// Like the mock in `common`, plus: every command line is logged, `STAT` can
/// get a fixed reply, at most `max_open` sessions are served at once (the
/// rest are greeted with `502`), and the server can be taken down (open
/// sessions dropped, new ones refused) and brought back.
struct Server {
    articles: Vec<MockArticle>,
    missing: Vec<String>,
    body_delay: Duration,
    stat_reply: Option<&'static str>,
    max_open: usize,
    lines: Mutex<Vec<String>>,
    down: AtomicBool,
    open: AtomicUsize,
    refused: AtomicUsize,
    bodies: AtomicUsize,
}

impl Server {
    fn new(articles: Vec<MockArticle>) -> Self {
        Self {
            articles,
            missing: Vec::new(),
            body_delay: Duration::ZERO,
            stat_reply: None,
            max_open: usize::MAX,
            lines: Mutex::new(Vec::new()),
            down: AtomicBool::new(false),
            open: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
            bodies: AtomicUsize::new(0),
        }
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    async fn spawn(self: &Arc<Self>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = self.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let taken = server.open.fetch_add(1, Ordering::SeqCst);
                if server.down.load(Ordering::SeqCst) || taken >= server.max_open {
                    server.open.fetch_sub(1, Ordering::SeqCst);
                    server.refused.fetch_add(1, Ordering::SeqCst);
                    let _ = sock.write_all(b"502 Too many connections\r\n").await;
                    continue;
                }
                let server = server.clone();
                tokio::spawn(async move {
                    let _ = server.session(sock).await;
                    server.open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        port
    }

    async fn session(&self, sock: TcpStream) -> std::io::Result<()> {
        let (rd, mut wr) = sock.into_split();
        let mut reader = BufReader::new(rd);
        wr.write_all(b"200 Welcome\r\n").await?;
        let mut buf = String::new();
        loop {
            buf.clear();
            if reader.read_line(&mut buf).await? == 0 || self.down.load(Ordering::SeqCst) {
                return Ok(());
            }
            let line = buf.trim_end_matches(['\r', '\n']).to_string();
            self.lines.lock().unwrap().push(line.clone());
            let mut parts = line.split_whitespace();
            let cmd = parts.next().unwrap_or("").to_uppercase();
            let id = parts
                .next()
                .unwrap_or("")
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            let article = self.articles.iter().find(|a| a.message_id == id);
            let missing = article.is_none() || self.missing.contains(&id);
            match cmd.as_str() {
                "AUTHINFO" if id.eq_ignore_ascii_case("USER") => {
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
                "STAT" => {
                    let reply = match self.stat_reply {
                        Some(reply) => format!("{reply}\r\n"),
                        None if missing => "430 No such article\r\n".to_string(),
                        None => "223 0 article\r\n".to_string(),
                    };
                    wr.write_all(reply.as_bytes()).await?;
                }
                "BODY" => {
                    self.bodies.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(self.body_delay).await;
                    if self.down.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    match article.filter(|_| !missing) {
                        Some(a) => {
                            wr.write_all(b"222 0 body follows\r\n").await?;
                            wr.write_all(&a.body).await?;
                            wr.write_all(b".\r\n").await?;
                        }
                        None => wr.write_all(b"430 No such article\r\n").await?,
                    }
                }
                _ => wr.write_all(b"500 Unknown command\r\n").await?,
            }
        }
    }
}

fn request(nzb: &Path, out: &Path, preflight: Preflight) -> JobRequest {
    JobRequest {
        preflight,
        ..JobRequest::new(nzb, out)
    }
}

/// A raw listener for one connection: greets, accepts any login, then hands
/// the socket to `script` with every later command line.
async fn one_connection<F, Fut>(script: F) -> u16
where
    F: FnOnce(BufReader<tokio::net::tcp::OwnedReadHalf>, tokio::net::tcp::OwnedWriteHalf) -> Fut
        + Send
        + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (rd, mut wr) = sock.into_split();
        let mut rd = BufReader::new(rd);
        wr.write_all(b"200 hi\r\n").await.unwrap();
        let mut line = String::new();
        rd.read_line(&mut line).await.unwrap(); // AUTHINFO USER
        wr.write_all(b"281 ok\r\n").await.unwrap();
        script(rd, wr).await;
    });
    port
}

// --- NNTP command injection ---------------------------------------------------

/// The `<segment>` text from the security review: a message id that ends the
/// `BODY` line early and smuggles a whole `POST` after it.
const SMUGGLING_ID: &str = "x@y&gt;&#13;&#10;POST&#13;&#10;From: victim&#13;&#10;Newsgroups: alt.test&#13;&#10;Subject: injected&#13;&#10;&#13;&#10;spam&#13;&#10;.&#13;&#10;BODY &lt;z@y";
const SMUGGLING_GROUP: &str = "alt.test&#13;&#10;XGROUPINJ";

/// Command lines the server got that the client never meant to send.
fn injected(lines: &[String]) -> Vec<String> {
    const SENT: [&str; 6] = [
        "AUTHINFO ",
        "GROUP alt.binaries.test",
        "BODY <",
        "STAT <",
        "DATE",
        "QUIT",
    ];
    lines
        .iter()
        .filter(|l| !SENT.iter().any(|s| l.starts_with(s)))
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_message_id_with_line_breaks_is_never_sent() {
    let xml = format!(
        r#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><file poster="p" date="1700000000" subject="&quot;a.bin&quot;"><groups><group>alt.binaries.test</group></groups><segments><segment bytes="10" number="1">{SMUGGLING_ID}</segment><segment bytes="10" number="2">ok@y</segment></segments></file></nzb>"#
    );
    let nzb: Nzb = xml.parse().unwrap();
    let evil = nzb.files()[0].segments[0].message_id.clone();
    assert!(evil.contains("\r\nPOST\r\n"), "{evil:?}");

    let (tx, rx) = tokio::sync::oneshot::channel();
    let port = one_connection(|mut rd, _wr| async move {
        let mut got = Vec::new();
        let mut line = String::new();
        while rd.read_line(&mut line).await.unwrap_or(0) > 0 {
            got.push(line.trim_end().to_string());
            if line.starts_with("BODY <ok@y>") {
                break;
            }
            line.clear();
        }
        let _ = tx.send(got);
    })
    .await;

    let mut conn = AsyncNntpConnection::connect(&usenet(port), None)
        .await
        .unwrap();
    assert!(conn.send_body(&evil).await.is_err());
    // Refused before anything was written: the connection is still usable.
    assert!(!conn.is_poisoned());
    conn.send_body("ok@y").await.unwrap();
    conn.flush().await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, vec!["BODY <ok@y>".to_string()]);
}

#[tokio::test]
async fn an_nzb_whose_every_message_id_is_malformed_is_rejected() {
    let xml = format!(
        r#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><file poster="p" date="1700000000" subject="&quot;a.bin&quot;"><groups><group>alt.binaries.test</group></groups><segments><segment bytes="10" number="1">{SMUGGLING_ID}</segment><segment bytes="10" number="2">two words@y</segment></segments></file></nzb>"#
    );
    let err = xml.parse::<Nzb>().unwrap_err();
    assert_eq!(err.kind(), dl_nzb::ErrorKind::Nzb, "{err}");
}

/// End to end, with and without the `STAT` scan: the bad article counts as
/// missing, the file listing only a bad group is skipped, nothing injected
/// reaches the server, and the good articles still download.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_nzb_text_never_reaches_the_server() {
    for preflight in [Preflight::Always, Preflight::Never] {
        let (mut articles, a_ids, a_sizes, _) = make_file_articles("a.bin", "a", 3, 1000);
        let (b_articles, _, _, b_full) = make_file_articles("b.bin", "b", 1, 1000);
        articles.extend(b_articles);
        let server = Arc::new(Server::new(articles));
        let port = server.spawn().await;

        let temp = tempfile::tempdir().unwrap();
        let mut xml = String::from(
            r#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">"#,
        );
        // a.bin (queued first: most articles) lists only the hostile group.
        xml.push_str(r#"<file poster="p" date="1700000000" subject="&quot;a.bin&quot;">"#);
        xml.push_str(&format!(
            "<groups><group>{SMUGGLING_GROUP}</group></groups><segments>"
        ));
        for (i, (id, size)) in a_ids.iter().zip(&a_sizes).enumerate() {
            xml.push_str(&format!(
                r#"<segment bytes="{size}" number="{}">{id}</segment>"#,
                i + 1
            ));
        }
        xml.push_str("</segments></file>");
        // b.bin: one good article and the hostile one.
        xml.push_str(r#"<file poster="p" date="1700000000" subject="&quot;b.bin&quot;">"#);
        xml.push_str("<groups><group>alt.binaries.test</group></groups><segments>");
        xml.push_str(r#"<segment bytes="1064" number="1">b1@t</segment>"#);
        xml.push_str(&format!(
            r#"<segment bytes="1064" number="2">{SMUGGLING_ID}</segment>"#
        ));
        xml.push_str("</segments></file></nzb>");
        let nzb = temp.path().join("hostile.nzb");
        std::fs::write(&nzb, xml).unwrap();

        let out = temp.path().join("Hostile");
        let engine = Engine::new(config(port, temp.path(), 1)).unwrap();
        let rec = Arc::new(Recorder::default());
        let job = engine.start(
            JobRequest {
                on_unrepairable: dl_nzb::engine::OnUnrepairable::Continue,
                ..request(&nzb, &out, preflight)
            },
            rec.clone(),
        );
        let summary = wait(&job, 30).await;

        let lines = server.lines();
        assert!(
            injected(&lines).is_empty(),
            "{preflight:?}: injected {:?}",
            injected(&lines)
        );
        assert_eq!(
            std::fs::read(out.join("b.bin.partial"))
                .or_else(|_| std::fs::read(out.join("b.bin")))
                .unwrap_or_default()
                .get(..1000),
            Some(&b_full[..]),
            "{preflight:?}: the good article of b.bin"
        );
        // a.bin's 3 (no usable group) and b.bin's hostile one.
        assert_eq!(summary.articles_failed, 4, "{preflight:?}: {summary:?}");
        assert_eq!(summary.outcome, Outcome::Failed, "{preflight:?}");
        assert!(
            rec.warnings().iter().any(|w| w.contains("a.bin")),
            "{preflight:?}: {:?}",
            rec.warnings()
        );
    }
}

// --- Article placement ---------------------------------------------------------

/// An article whose `=ypart begin=` lies 16 GiB into a 100-byte file is a bad
/// article, not a reason to grow the file to 16 GiB.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_part_placed_past_the_end_of_its_file_is_rejected() {
    let plain = b"0123456789";
    let begin: u64 = (1u64 << 34) + 1;
    let body = build_part("x.bin", 1, 1, begin, begin + 9, plain);
    let server = Arc::new(Server::new(vec![MockArticle {
        message_id: "x@t".into(),
        body,
    }]));
    let port = server.spawn().await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "x", &[("x.bin", &["x@t".to_string()], &[100])]);
    let out = temp.path().join("X");
    let engine = Engine::new(config(port, temp.path(), 1)).unwrap();
    let summary = wait(
        &engine.start(
            request(&nzb, &out, Preflight::Never),
            Arc::new(Recorder::default()),
        ),
        30,
    )
    .await;

    for name in ["x.bin", "x.bin.partial"] {
        if let Ok(meta) = std::fs::metadata(out.join(name)) {
            assert!(meta.len() <= 100, "{name} grew to {} bytes", meta.len());
        }
    }
    assert_eq!(summary.articles_failed, 1, "{summary:?}");
    assert_ne!(summary.outcome, Outcome::Completed);
}

// --- Bounded reads -------------------------------------------------------------

/// A greeting that never ends must fail the connection at the line limit,
/// not buffer until the login timeout.
#[tokio::test]
async fn an_endless_response_line_is_cut_off() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = sock.write_all(b"200 ").await;
        let _ = sock.write_all(&vec![b'a'; 1 << 20]).await;
        tokio::time::sleep(Duration::from_secs(120)).await;
    });
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        AsyncNntpConnection::connect(&usenet(port), None),
    )
    .await
    .expect("still reading the endless line");
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// An article body line that never ends must fail the article at the body
/// limit (poisoning the connection), not buffer until the read timeout.
#[tokio::test]
async fn an_endless_body_line_is_cut_off() {
    let port = one_connection(|mut rd, mut wr| async move {
        let mut line = String::new();
        rd.read_line(&mut line).await.unwrap(); // BODY
        let _ = wr.write_all(b"222 0 body follows\r\n").await;
        let chunk = vec![b'a'; 1 << 20];
        for _ in 0..40 {
            if wr.write_all(&chunk).await.is_err() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_secs(180)).await;
    })
    .await;
    let mut conn = AsyncNntpConnection::connect(&usenet(port), None)
        .await
        .unwrap();
    conn.send_body("long@t").await.unwrap();
    conn.flush().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), conn.read_body_outcome("long@t"))
        .await
        .expect("still reading the endless line");
    assert!(matches!(outcome, ArticleOutcome::Transient), "{outcome:?}");
    assert!(conn.is_poisoned());
}

// --- Fewer connections than configured ------------------------------------------

/// The provider takes 2 connections; 4 are configured. The two workers that
/// can't connect must not give up articles the other two are downloading.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_provider_allowing_fewer_connections_loses_no_articles() {
    let (articles, ids, sizes, full) = make_file_articles("show.bin", "r", 480, 4_000);
    let mut server = Server::new(articles);
    server.body_delay = Duration::from_millis(25);
    server.max_open = 2;
    let server = Arc::new(server);
    let port = server.spawn().await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "show", &[("show.bin", &ids, &sizes)]);
    let out = temp.path().join("Show");
    let mut cfg = config(port, temp.path(), 4);
    cfg.usenet.retry_delay = 50;
    let engine = Engine::new(cfg).unwrap();
    let summary = wait(
        &engine.start(
            request(&nzb, &out, Preflight::Never),
            Arc::new(Recorder::default()),
        ),
        120,
    )
    .await;

    assert!(
        server.refused.load(Ordering::SeqCst) > 0,
        "the limit never applied"
    );
    assert_eq!(summary.articles_failed, 0, "{summary:?}");
    assert_eq!(summary.outcome, Outcome::Completed, "{summary:?}");
    assert_eq!(summary.error_kind, None);
    assert_eq!(std::fs::read(out.join("show.bin")).unwrap(), full);
}

/// The server goes away, the user pauses while the worker is still trying to
/// reconnect, and the server is back by the time they resume. The pause ends
/// the reconnect attempts at once (a paused job uses no network), and the
/// failed ones must not decide the outcome: the one article the server
/// really lacks is reported as missing, not as a connection error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_failures_while_paused_are_not_the_jobs_error() {
    let (articles, ids, sizes, _) = make_file_articles("ep.bin", "e", 40, 2_000);
    let mut server = Server::new(articles);
    server.body_delay = Duration::from_millis(20);
    server.missing = vec![ids[39].clone()];
    let server = Arc::new(server);
    let port = server.spawn().await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "ep", &[("ep.bin", &ids, &sizes)]);
    let out = temp.path().join("Ep");
    let mut cfg = config(port, temp.path(), 1);
    cfg.usenet.retry_delay = 50;
    let engine = Engine::new(cfg).unwrap();
    let job = engine.start(
        request(&nzb, &out, Preflight::Never),
        Arc::new(Recorder::default()),
    );

    until("a few articles", || {
        server.bodies.load(Ordering::SeqCst) >= 5
    })
    .await;
    server.down.store(true, Ordering::SeqCst);
    until("a refused reconnect", || {
        server.refused.load(Ordering::SeqCst) >= 1
    })
    .await;
    job.pause();
    // No more attempts while paused.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let refused = server.refused.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(server.refused.load(Ordering::SeqCst), refused);
    server.down.store(false, Ordering::SeqCst);
    job.resume();

    let summary = wait(&job, 60).await;
    assert_eq!(summary.articles_failed, 1, "{summary:?}");
    assert_eq!(summary.outcome, Outcome::Failed);
    assert_eq!(summary.error_kind, None, "{:?}", summary.message);
    assert!(
        summary
            .message
            .as_deref()
            .is_some_and(|m| m.contains("missing")),
        "{:?}",
        summary.message
    );
}

/// The server goes away mid-download and stays away. The job must end after
/// one round of reconnect attempts, not settle an article per round while its
/// bar creeps on, and must not go on to fetch recovery data it cannot reach:
/// it fails with the connection's error, resumably, and once the server is
/// back a resume fetches every article that failed that way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_server_ends_the_job_promptly_and_resumably() {
    let (mut articles, ids, sizes, full) = make_file_articles("film.bin", "f", 300, 2_000);
    let (index, index_ids, index_sizes, _) = make_file_articles("film.par2", "i", 1, 500);
    let (volume, volume_ids, volume_sizes, _) =
        make_file_articles("film.vol00+40.par2", "v", 40, 2_000);
    articles.extend(index);
    articles.extend(volume);
    let mut server = Server::new(articles);
    server.body_delay = Duration::from_millis(10);
    let server = Arc::new(server);
    let port = server.spawn().await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(
        temp.path(),
        "film",
        &[
            ("film.bin", &ids, &sizes),
            ("film.par2", &index_ids, &index_sizes),
            ("film.vol00+40.par2", &volume_ids, &volume_sizes),
        ],
    );
    let out = temp.path().join("Film");
    let mut cfg = config(port, temp.path(), 2);
    cfg.usenet.retry_delay = 50;
    // Post-processing has something to do (there are no archives to find).
    cfg.post_processing.auto_extract_rar = true;
    let engine = Engine::new(cfg).unwrap();
    let recorder = Arc::new(Recorder::default());
    let job = engine.start(request(&nzb, &out, Preflight::Never), recorder.clone());

    until("some articles", || {
        server.bodies.load(Ordering::SeqCst) >= 20
    })
    .await;
    server.down.store(true, Ordering::SeqCst);
    let lost = Instant::now();
    // One round of reconnect attempts with a 50 ms retry delay is about 2.5 s;
    // an article per round would take minutes.
    let summary = wait(&job, 20).await;
    assert!(
        lost.elapsed() < Duration::from_secs(12),
        "took {:?} to give up",
        lost.elapsed()
    );
    assert_eq!(summary.outcome, Outcome::Failed, "{summary:?}");
    assert!(summary.error_kind.is_some(), "{summary:?}");
    assert!(summary.resumable, "{summary:?}");
    assert!(
        !recorder
            .phases()
            .contains(&dl_nzb::engine::JobPhase::DownloadingRecovery),
        "fetched recovery data from a server that was gone: {:?}",
        recorder.phases()
    );
    // Nothing to verify or unpack in an unfinished download.
    assert_eq!(
        summary.par2.skipped_reason.as_deref(),
        Some("Post-processing did not run."),
        "{summary:?}"
    );

    server.down.store(false, Ordering::SeqCst);
    let summary = wait(
        &engine.start(
            request(&nzb, &out, Preflight::Never),
            Arc::new(Recorder::default()),
        ),
        60,
    )
    .await;
    assert_eq!(summary.articles_failed, 0, "{summary:?}");
    assert_eq!(summary.outcome, Outcome::Completed, "{summary:?}");
    assert_eq!(std::fs::read(out.join("film.bin")).unwrap(), full);
}

// --- STAT replies ---------------------------------------------------------------

/// A server that refuses `STAT` (here `500`, as one without the command
/// would) says nothing about whether articles exist: the scan is
/// inconclusive and every article is still downloaded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_stat_is_unknown_not_missing() {
    let (articles, ids, sizes, full) = make_file_articles("doc.bin", "d", 8, 3_000);
    let mut server = Server::new(articles);
    server.stat_reply = Some("500 What?");
    let server = Arc::new(server);
    let port = server.spawn().await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "doc", &[("doc.bin", &ids, &sizes)]);
    let out = temp.path().join("Doc");
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let summary = wait(
        &engine.start(
            request(&nzb, &out, Preflight::Always),
            Arc::new(Recorder::default()),
        ),
        30,
    )
    .await;

    let availability = summary.availability.clone().expect("the scan ran");
    assert_eq!(availability.articles_missing, 0, "{availability:?}");
    assert_eq!(availability.verdict, dl_nzb::engine::Verdict::Unknown);
    assert_eq!(summary.outcome, Outcome::Completed, "{summary:?}");
    assert_eq!(std::fs::read(out.join("doc.bin")).unwrap(), full);
}
