//! Regressions from the second verification pass: a stop that lands right
//! after PAR2 deleted its files, articles placed by an NZB whose sizes are
//! wrong, the availability scan under a connection limit, the reason given
//! when the PAR2 files are gone, a body exactly at the size limit, and a job
//! request printed for a log.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::engine::{
    Engine, JobEvent, JobHandle, JobObserver, JobPhase, JobRequest, Outcome, Preflight, Verdict,
};
use dl_nzb::nntp::{ArticleOutcome, AsyncNntpConnection};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

// --- A scripted NNTP server ---------------------------------------------------

/// Like the mock in `common`, plus: at most `max_open` sessions are served at
/// once (the rest are greeted with `502`), and `STAT` for the ids in
/// `stat_unknown` gets a reply that says nothing about the article.
struct Server {
    articles: Vec<MockArticle>,
    missing: Vec<String>,
    stat_unknown: Vec<String>,
    max_open: usize,
    open: AtomicUsize,
    refused: AtomicUsize,
    bodies: AtomicUsize,
}

impl Server {
    fn new(articles: Vec<MockArticle>) -> Self {
        Self {
            articles,
            missing: Vec::new(),
            stat_unknown: Vec::new(),
            max_open: usize::MAX,
            open: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
            bodies: AtomicUsize::new(0),
        }
    }

    async fn spawn(self: &Arc<Self>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = self.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let taken = server.open.fetch_add(1, Ordering::SeqCst);
                if taken >= server.max_open {
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
            if reader.read_line(&mut buf).await? == 0 {
                return Ok(());
            }
            let line = buf.trim_end_matches(['\r', '\n']).to_string();
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
                    let reply = if self.stat_unknown.contains(&id) {
                        "500 What?\r\n"
                    } else if missing {
                        "430 No such article\r\n"
                    } else {
                        "223 0 article\r\n"
                    };
                    wr.write_all(reply.as_bytes()).await?;
                }
                "BODY" => {
                    self.bodies.fetch_add(1, Ordering::SeqCst);
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

/// Records every event, and stops the job the first time `stop_when` holds.
struct StopWhen {
    stop_when: Box<dyn Fn(&JobEvent) -> bool + Send + Sync>,
    job: OnceLock<JobHandle>,
    events: Recorder,
}

impl StopWhen {
    fn new(stop_when: impl Fn(&JobEvent) -> bool + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            stop_when: Box::new(stop_when),
            job: OnceLock::new(),
            events: Recorder::default(),
        })
    }

    async fn run(self: &Arc<Self>, engine: &Engine, request: JobRequest) -> dl_nzb::JobSummary {
        let job = engine.start(request, self.clone());
        let _ = self.job.set(job.clone());
        wait(&job, 60).await
    }
}

impl JobObserver for StopWhen {
    fn on_event(&self, event: JobEvent) {
        if (self.stop_when)(&event) {
            // The handle is set right after `start` returns.
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.job.get().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.job.get().expect("job handle").stop();
        }
        self.events.on_event(event);
    }
}

// --- A stop right after PAR2 deleted its files -------------------------------------

/// Two archives and a full PAR2 set for them, posted one article per file.
struct Release {
    temp: tempfile::TempDir,
    nzb: PathBuf,
    articles: Vec<(String, Vec<u8>)>,
    ids: Vec<(String, String)>,
}

impl Release {
    fn new(files: &[(&str, Vec<u8>)]) -> Self {
        let source = tempfile::tempdir().unwrap();
        let mut sources = Vec::new();
        for (name, bytes) in files {
            let path = source.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            sources.push(path);
        }
        let par2 = par2_rs::Par2Creator::new(sources)
            .unwrap()
            .with_block_size(2048)
            .unwrap()
            .with_redundancy(100.0)
            .unwrap()
            .create()
            .unwrap();
        let mut posted: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(n, b)| (n.to_string(), b.clone()))
            .collect();
        for path in &par2 {
            posted.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(path).unwrap(),
            ));
        }
        let mut articles = Vec::new();
        let mut ids = Vec::new();
        let mut listed: Vec<(String, Vec<String>, Vec<u64>)> = Vec::new();
        for (i, (name, bytes)) in posted.iter().enumerate() {
            let id = format!("f{i}@t");
            articles.push((id.clone(), article_without_crc(name, bytes)));
            ids.push((name.clone(), id.clone()));
            listed.push((name.clone(), vec![id], vec![bytes.len() as u64 + 64]));
        }
        let temp = tempfile::tempdir().unwrap();
        let refs: Vec<(&str, &[String], &[u64])> = listed
            .iter()
            .map(|(n, i, s)| (n.as_str(), i.as_slice(), s.as_slice()))
            .collect();
        let nzb = write_nzb(temp.path(), "job", &refs);
        Self {
            temp,
            nzb,
            articles,
            ids,
        }
    }

    fn id(&self, posted: &str) -> String {
        self.ids
            .iter()
            .find(|(n, _)| n == posted)
            .map(|(_, id)| id.clone())
            .unwrap()
    }

    async fn serve(&self, missing: Vec<String>) -> u16 {
        let state = MockServerState {
            articles: self
                .articles
                .iter()
                .map(|(id, body)| MockArticle {
                    message_id: id.clone(),
                    body: body.clone(),
                })
                .collect(),
            missing_ids: missing,
            ..Default::default()
        };
        spawn_server(Arc::new(state)).await
    }

    fn folder(&self) -> PathBuf {
        self.temp.path().join("Job")
    }
}

/// Stops the job from another thread `delay_us` after PAR2 starts repairing.
struct StopAfterRepairStarts {
    delay_us: u64,
    job: OnceLock<JobHandle>,
}

impl JobObserver for StopAfterRepairStarts {
    fn on_event(&self, event: JobEvent) {
        if event != JobEvent::Phase(JobPhase::Repairing) {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.job.get().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let job = self.job.get().expect("job handle").clone();
        let delay = Duration::from_micros(self.delay_us);
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            job.stop();
        });
    }
}

/// With `delete_par2_after_repair`, a stop can land after PAR2 repaired the
/// files and deleted its own, before the job saved that verdict. The next
/// start then has neither the PAR2 files nor a verdict, and must still finish
/// the job instead of calling the repaired data "missing". The stop is tried
/// at many delays so some land in that window.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_right_after_par2_deletes_its_files_still_resumes() {
    let mut stopped = 0;
    let mut in_window = 0;
    let mut stuck = Vec::new();
    for i in 0..150u64 {
        let release = Release::new(&[("A.rar", PLAIN_RAR.to_vec()), ("C.rar", PLAIN_RAR.to_vec())]);
        // A.rar's only article is missing: PAR2 must rebuild it.
        let port = release.serve(vec![release.id("A.rar")]).await;
        let out = release.folder();
        let mut cfg = config(port, release.temp.path(), 2);
        cfg.post_processing.auto_par2_repair = true;
        cfg.post_processing.auto_extract_rar = true;
        cfg.post_processing.delete_par2_after_repair = true;
        let engine = Engine::new(cfg).unwrap();
        let observer = Arc::new(StopAfterRepairStarts {
            delay_us: (i * 37) % 3000,
            job: OnceLock::new(),
        });
        let job = engine.start(
            request(&release.nzb, &out, Preflight::Never),
            observer.clone(),
        );
        let _ = observer.job.set(job.clone());
        let first = wait(&job, 30).await;
        if first.outcome != Outcome::Stopped {
            continue;
        }
        stopped += 1;
        let names = names_in(&out);
        if !names.iter().any(|n| n.ends_with(".par2")) && !out.join("VERSION").exists() {
            in_window += 1;
        }
        let second = wait(
            &engine.start(
                request(&release.nzb, &out, Preflight::Never),
                Arc::new(Recorder::default()),
            ),
            30,
        )
        .await;
        if second.outcome != Outcome::Completed {
            stuck.push(format!(
                "stop after {}us: {:?} {:?}, folder {names:?}",
                observer.delay_us, second.outcome, second.message
            ));
        }
    }
    eprintln!("stopped {stopped} runs, {in_window} right after PAR2 deleted its files");
    assert!(stuck.is_empty(), "{stuck:#?}");
}

// --- Article placement with unreliable NZB sizes -----------------------------------

/// Download `nzb` into a fresh folder; the summary and the file's bytes
/// (finished or partial).
async fn download(
    server: Server,
    nzb: impl FnOnce(&Path) -> PathBuf,
    filename: &str,
) -> (dl_nzb::JobSummary, Vec<u8>) {
    let server = Arc::new(server);
    let port = server.spawn().await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = nzb(temp.path());
    let out = temp.path().join("Job");
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let summary = wait(
        &engine.start(
            request(&nzb, &out, Preflight::Never),
            Arc::new(Recorder::default()),
        ),
        30,
    )
    .await;
    let bytes = std::fs::read(out.join(filename))
        .or_else(|_| std::fs::read(out.join(format!("{filename}.partial"))))
        .unwrap_or_default();
    (summary, bytes)
}

/// Indexers list incomplete posts: this NZB lacks the entry for part 5 of a
/// 10-part file, so its sizes add up to less than the file. Every part it
/// does list must land where it belongs, the last one included.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m1_nzb_missing_a_segment_entry() {
    let (articles, ids, sizes, full) = make_file_articles("f.bin", "s", 10, 4_000);
    // Numbered as posted (1-4, 6-10), and numbered again from 1 (some tools do).
    for renumbered in [false, true] {
        let segments: Vec<(u32, u64, String)> = (0..10)
            .filter(|i| *i != 4)
            .enumerate()
            .map(|(listed, i)| {
                let number = if renumbered { listed + 1 } else { i + 1 };
                (number as u32, sizes[i], ids[i].clone())
            })
            .collect();
        let server = Server::new(
            articles
                .iter()
                .map(|a| MockArticle {
                    message_id: a.message_id.clone(),
                    body: a.body.clone(),
                })
                .collect(),
        );
        let (summary, got) =
            download(server, |dir| numbered_nzb(dir, "f.bin", &segments), "f.bin").await;
        assert_eq!(
            summary.articles_failed, 0,
            "renumbered={renumbered}: {summary:?}"
        );
        assert_eq!(got.len(), 40_000, "renumbered={renumbered}");
        assert_eq!(got[..16_000], full[..16_000], "renumbered={renumbered}");
        assert_eq!(got[20_000..], full[20_000..], "renumbered={renumbered}");
    }
}

/// An NZB that gives every segment `bytes="0"` (or too few bytes): the
/// articles themselves say how big the file is, within reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m1_nzb_with_zero_or_too_small_sizes() {
    // One part, sized 0 and 9 for its 10 bytes.
    let plain = b"0123456789";
    for nzb_bytes in [0u64, 9] {
        let server = Server::new(vec![MockArticle {
            message_id: "x@t".into(),
            body: one_part("x.bin", 1, 10, Some(10), plain),
        }]);
        let segments = vec![(1, nzb_bytes, "x@t".to_string())];
        let (summary, got) =
            download(server, |dir| numbered_nzb(dir, "x.bin", &segments), "x.bin").await;
        assert_eq!(summary.articles_failed, 0, "bytes={nzb_bytes}: {summary:?}");
        assert_eq!(summary.outcome, Outcome::Completed, "bytes={nzb_bytes}");
        assert_eq!(got, plain, "bytes={nzb_bytes}");
    }

    // Ten parts, every one sized 0, or half its real size.
    let (articles, ids, sizes, full) = make_file_articles("f.bin", "s", 10, 4_000);
    for scale in [0u64, 2] {
        let segments: Vec<(u32, u64, String)> = (0..10)
            .map(|i| {
                let bytes = if scale == 0 { 0 } else { sizes[i] / scale };
                (i as u32 + 1, bytes, ids[i].clone())
            })
            .collect();
        let server = Server::new(
            articles
                .iter()
                .map(|a| MockArticle {
                    message_id: a.message_id.clone(),
                    body: a.body.clone(),
                })
                .collect(),
        );
        let (summary, got) =
            download(server, |dir| numbered_nzb(dir, "f.bin", &segments), "f.bin").await;
        assert_eq!(summary.articles_failed, 0, "scale={scale}: {summary:?}");
        assert_eq!(got, full, "scale={scale}");
    }
}

/// Unreliable NZB sizes don't open the door to a part placed absurdly far
/// past the end of its file: with no size, a zero size or a huge `size=`
/// claim, such a part is still a bad article and the file stays small.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m1_a_crafted_offset_is_still_rejected_when_nzb_sizes_are_unreliable() {
    let plain = b"0123456789";
    let far: u64 = 1 << 34;
    let cases: Vec<(&str, Option<u64>, u64)> = vec![
        ("no size=, NZB 100 bytes", None, 100),
        ("no size=, NZB 0 bytes", None, 0),
        ("size= huge, NZB 100 bytes", Some(1 << 40), 100),
        ("size= huge, NZB 0 bytes", Some(1 << 40), 0),
    ];
    for (name, ybegin_size, nzb_bytes) in cases {
        let server = Server::new(vec![MockArticle {
            message_id: "x@t".into(),
            body: one_part("x.bin", far + 1, far + 10, ybegin_size, plain),
        }]);
        let segments = vec![(1, nzb_bytes, "x@t".to_string())];
        let (summary, got) =
            download(server, |dir| numbered_nzb(dir, "x.bin", &segments), "x.bin").await;
        assert_eq!(summary.articles_failed, 1, "{name}: {summary:?}");
        assert!(got.len() <= 1_000, "{name}: the file grew to {}", got.len());
    }
}

// --- The availability scan under a connection limit ------------------------------

/// The provider allows 1 of the 8 configured connections. One data article
/// is missing and there is plenty of recovery data: the scan must come to
/// the same verdict as without the limit, not count every `STAT` it couldn't
/// send as missing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h2_preflight_under_connection_limit() {
    for limit in [usize::MAX, 1] {
        let (mut articles, ids, sizes, _) = make_file_articles("film.bin", "f", 2000, 1_000);
        let (index, index_ids, index_sizes, _) = make_file_articles("film.par2", "i", 1, 500);
        let (volume, volume_ids, volume_sizes, _) =
            make_file_articles("film.vol00+40.par2", "v", 400, 1_000);
        articles.extend(index);
        articles.extend(volume);
        let mut server = Server::new(articles);
        server.missing = vec!["f3@t".to_string()];
        server.max_open = limit;
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
        let mut cfg = config(port, temp.path(), 8);
        cfg.usenet.retry_delay = 20;
        let engine = Engine::new(cfg).unwrap();
        // The verdict is all this is about: stop once it is in.
        let observer = StopWhen::new(|e| matches!(e, JobEvent::Availability(_)));
        let summary = observer
            .run(&engine, request(&nzb, &out, Preflight::Always))
            .await;
        let availability = summary.availability.clone().expect("the scan ran");
        assert_eq!(
            availability.verdict,
            Verdict::Repairable,
            "limit={limit}: {availability:?} {:?}",
            summary.message
        );
        assert_eq!(availability.articles_missing, 1, "limit={limit}");
        assert_ne!(summary.outcome, Outcome::Unrepairable, "limit={limit}");
        if limit == 1 {
            assert!(
                server.refused.load(Ordering::SeqCst) > 0,
                "the limit never applied"
            );
        }
    }
}

/// One data article is confirmed missing, and the server won't say whether
/// the recovery volume's articles are there. That's not enough to call the
/// job unrepairable: the verdict is unknown and the download goes ahead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scan_that_could_not_check_the_recovery_data_is_unknown_not_unrepairable() {
    let (mut articles, ids, sizes, _) = make_file_articles("film.bin", "f", 40, 1_000);
    let (index, index_ids, index_sizes, _) = make_file_articles("film.par2", "i", 1, 500);
    let (volume, volume_ids, volume_sizes, _) =
        make_file_articles("film.vol00+40.par2", "v", 40, 1_000);
    articles.extend(index);
    articles.extend(volume);
    let mut server = Server::new(articles);
    server.missing = vec!["f3@t".to_string()];
    server.stat_unknown = volume_ids.clone();
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
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let observer = StopWhen::new(|e| matches!(e, JobEvent::Availability(_)));
    let summary = observer
        .run(&engine, request(&nzb, &out, Preflight::Always))
        .await;
    let availability = summary.availability.clone().expect("the scan ran");
    assert_eq!(availability.verdict, Verdict::Unknown, "{availability:?}");
    assert_eq!(availability.articles_missing, 1);
    assert_eq!(summary.outcome, Outcome::Stopped, "{:?}", summary.message);
}

// --- The reason given when the PAR2 files are gone -----------------------------------

/// A download with an article missing finished with PAR2 turned off; then
/// the PAR2 files were deleted and the job started again with PAR2 on. The
/// failure must name what really stands in the way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_failure_says_the_par2_files_are_gone_when_they_are() {
    let (mut articles, ids, sizes, _) = make_file_articles("film.bin", "f", 20, 1_000);
    let (index, index_ids, index_sizes, _) = make_file_articles("film.par2", "i", 1, 500);
    let (volume, volume_ids, volume_sizes, _) =
        make_file_articles("film.vol00+20.par2", "v", 20, 1_000);
    articles.extend(index);
    articles.extend(volume);
    let mut server = Server::new(articles);
    server.missing = vec!["f3@t".to_string()];
    let server = Arc::new(server);
    let port = server.spawn().await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(
        temp.path(),
        "film",
        &[
            ("film.bin", &ids, &sizes),
            ("film.par2", &index_ids, &index_sizes),
            ("film.vol00+20.par2", &volume_ids, &volume_sizes),
        ],
    );
    let out = temp.path().join("Film");

    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let first = wait(
        &engine.start(
            request(&nzb, &out, Preflight::Never),
            Arc::new(Recorder::default()),
        ),
        30,
    )
    .await;
    assert_eq!(first.outcome, Outcome::Failed, "{first:?}");
    let message = first.message.unwrap_or_default();
    assert!(message.contains("PAR2 repair is turned off"), "{message}");

    for name in names_in(&out) {
        if name.ends_with(".par2") {
            std::fs::remove_file(out.join(name)).unwrap();
        }
    }
    let mut cfg = config(port, temp.path(), 2);
    cfg.post_processing.auto_par2_repair = true;
    let engine = Engine::new(cfg).unwrap();
    let second = wait(
        &engine.start(
            request(&nzb, &out, Preflight::Never),
            Arc::new(Recorder::default()),
        ),
        30,
    )
    .await;
    assert_eq!(second.outcome, Outcome::Failed, "{second:?}");
    let message = second.message.unwrap_or_default();
    assert!(!message.contains("turned off"), "{message}");
    assert!(message.contains("PAR2 files"), "{message}");
    assert!(message.contains("gone"), "{message}");
}

// --- A body exactly at the size limit -------------------------------------------------

const MAX_BODY: usize = 32 * 1024 * 1024;

/// What a connection makes of a `BODY` response carrying `payload` (which
/// ends with the terminator, or doesn't).
async fn body_outcome(payload: Vec<u8>) -> (ArticleOutcome, bool) {
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
        line.clear();
        rd.read_line(&mut line).await.unwrap(); // BODY
        let _ = wr.write_all(b"222 0 body follows\r\n").await;
        let _ = wr.write_all(&payload).await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    let mut conn = AsyncNntpConnection::connect(&usenet(port), None)
        .await
        .unwrap();
    conn.send_body("big@t").await.unwrap();
    conn.flush().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(20), conn.read_body_outcome("big@t"))
        .await
        .expect("still reading the body");
    let poisoned = conn.is_poisoned();
    (outcome, poisoned)
}

/// A body of `size` bytes once read (lines of `a`), then the terminator.
fn body_of(size: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(size + size / (1 << 20) + 16);
    let mut produced = 0usize;
    while produced < size {
        // A line of n bytes and CRLF reads as n bytes and `\n`.
        let content = (size - produced - 1).min(1 << 20);
        payload.extend(std::iter::repeat_n(b'a', content));
        payload.extend_from_slice(b"\r\n");
        produced += content + 1;
    }
    payload.extend_from_slice(b".\r\n");
    payload
}

/// The limit is on the body: one of exactly that size is read whole (it is
/// no yEnc, so it fails to decode, on a connection still in sync), and only
/// a larger one is cut off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_of_exactly_the_size_limit_is_read() {
    let (outcome, poisoned) = body_outcome(body_of(MAX_BODY)).await;
    assert!(
        matches!(outcome, ArticleOutcome::DecodeFailed),
        "{outcome:?}"
    );
    assert!(!poisoned);

    let (outcome, poisoned) = body_outcome(body_of(MAX_BODY + 1)).await;
    assert!(matches!(outcome, ArticleOutcome::Transient), "{outcome:?}");
    assert!(poisoned);
}

// --- A job request printed for a log ----------------------------------------------------

#[test]
fn a_printed_job_request_hides_its_passwords() {
    let request = JobRequest {
        passwords: vec!["hunter2".to_string(), "s3cret".to_string()],
        ..JobRequest::new("/tmp/a.nzb", "/tmp/A")
    };
    for printed in [format!("{request:?}"), format!("{request:#?}")] {
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(!printed.contains("s3cret"), "{printed}");
        assert!(printed.contains("a.nzb"), "{printed}");
    }
}
