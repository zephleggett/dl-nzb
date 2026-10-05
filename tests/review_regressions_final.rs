//! Regressions from the final verification pass: a crafted part that raised
//! its own placement limit, renaming by a title that cleans up to nothing or
//! already ends in the extension, a server that resets every body, a stop
//! landing as post-processing ends, warnings with library wording, a refused
//! first connection, and `reprocess` on a folder with known holes.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::engine::{
    Engine, ErrorKind, JobEvent, JobHandle, JobObserver, JobPhase, JobRequest, Outcome,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const SIDECAR: &str = ".dl-nzb-job.json";

/// One or more plain sentences: no library wording in parentheses.
fn plain(message: &str) -> bool {
    !message.contains('(') && !message.contains(')') && message.ends_with('.')
}

/// A listener that hands each accepted connection to `session` with its
/// index, counting them.
async fn listen<F, Fut>(accepted: Arc<AtomicUsize>, session: F) -> u16
where
    F: Fn(TcpStream, usize) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let n = accepted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(session(sock, n));
        }
    });
    port
}

// --- Article placement ------------------------------------------------------------

/// The article being judged used to count toward its own limit: one part of
/// 4 MiB numbered into a ~200 KB NZB was allowed to end at 400 x 4 MiB, and
/// the file grew (sparsely) to 1.6 GB, reported Completed. The limit now
/// comes from the NZB alone (here 200 parts numbered 1 to 200: 200 x 4 MiB +
/// 4 MiB), and no article raises it. (An NZB numbering its parts up to 400
/// claims 400 parts, and would allow 400 x 4 MiB + 4 MiB: see
/// `review_regressions_round3`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crafted_part_cannot_raise_its_own_placement_limit() {
    let (count, seg) = (200usize, 1000usize);
    let full: Vec<u8> = (0..count * seg).map(|i| (i % 251) as u8).collect();
    let mut articles = Vec::new();
    let mut segments = Vec::new();
    for k in 1..count {
        let id = format!("s{k}@t");
        articles.push(MockArticle {
            message_id: id.clone(),
            body: one_part(
                "f.bin",
                (k * seg + 1) as u64,
                ((k + 1) * seg) as u64,
                None,
                &full[k * seg..(k + 1) * seg],
            ),
        });
        segments.push(((k + 1) as u32, seg as u64 + 64, id));
    }
    let len: u64 = 4 << 20;
    let parts = 400u64;
    articles.push(MockArticle {
        message_id: "evil@t".into(),
        body: one_part(
            "f.bin",
            parts * len + 1,
            parts * len + len,
            None,
            &vec![0x41; len as usize],
        ),
    });
    segments.insert(0, (1, seg as u64 + 64, "evil@t".to_string()));

    let port = spawn_server(Arc::new(MockServerState {
        articles,
        ..Default::default()
    }))
    .await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = numbered_nzb(temp.path(), "f.bin", &segments);
    let out = temp.path().join("Job");
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let summary = wait(
        &engine.start(request(&nzb, &out), Arc::new(Recorder::default())),
        60,
    )
    .await;

    let on_disk = ["f.bin", "f.bin.partial"]
        .iter()
        .filter_map(|name| std::fs::metadata(out.join(name)).ok())
        .map(|m| m.len())
        .max()
        .unwrap_or(0);
    let declared = (count * (seg + 64)) as u64;
    assert!(
        on_disk <= declared,
        "a ~200 KB file grew to {on_disk} bytes"
    );
    assert_eq!(summary.articles_failed, 1, "{summary:?}");
    assert_ne!(summary.outcome, Outcome::Completed);
    // Every honest part still landed where it belongs.
    let got = std::fs::read(out.join("f.bin")).unwrap();
    assert_eq!(got[seg..], full[seg..]);
}

// --- Renaming by title -------------------------------------------------------------

const SCRAMBLED: &str = "a1b2c3d4e5f6a7b8c9d0.mkv";

/// Download a scrambled `.mkv` from `Name.nzb` into the de-duplicated folder
/// "Name 2" with renaming on, titled `title`; the `.mkv` files it ends with.
async fn renamed_with(title: Option<&str>) -> Vec<String> {
    let (articles, ids, sizes, _) = make_file_articles(SCRAMBLED, "o", 2, 30_000);
    let port = spawn_server(Arc::new(MockServerState {
        articles,
        ..Default::default()
    }))
    .await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "Name", &[(SCRAMBLED, &ids, &sizes)]);
    let out = temp.path().join("Name 2");
    let mut cfg = config(port, temp.path(), 2);
    cfg.post_processing.deobfuscate_file_names = true;
    let engine = Engine::new(cfg).unwrap();
    let req = JobRequest {
        title: title.map(String::from),
        ..request(&nzb, &out)
    };
    let summary = wait(&engine.start(req, Arc::new(Recorder::default())), 30).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    names_in(&out)
        .into_iter()
        .filter(|n| n.to_ascii_lowercase().ends_with(".mkv"))
        .collect()
}

/// A title that cleans up to nothing names the file after the NZB, never
/// after the folder the app may have de-duplicated ("Name 2.mkv").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_title_that_cleans_up_to_nothing_falls_back_to_the_nzb_name() {
    for title in ["..", "...", " . ", "\u{202E}"] {
        assert_eq!(
            renamed_with(Some(title)).await,
            vec!["Name.mkv"],
            "title {title:?}"
        );
    }
}

/// A title that already ends in the file's extension isn't given it twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_title_ending_in_the_extension_is_not_doubled() {
    for (title, want) in [("Name.mkv", "Name.mkv"), ("Big Movie.MKV", "Big Movie.mkv")] {
        assert_eq!(
            renamed_with(Some(title)).await,
            vec![want],
            "title {title:?}"
        );
    }
}

/// Bidirectional and invisible formatting characters can make a name read
/// as something else ("evil<RLO>vkm.exe" shows as "evilexe.mkv"): they never
/// reach a file name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidi_controls_never_reach_a_file_name() {
    let title = "evil\u{202E}vkm.exe\u{2066}\u{200F}\u{061C}";
    assert_eq!(renamed_with(Some(title)).await, vec!["evilvkm.exe.mkv"]);
}

// --- A server that resets every body -----------------------------------------------

/// Logs in, then resets the connection in the middle of every body.
async fn resetting_session(sock: TcpStream, _: usize) {
    let (rd, mut wr) = sock.into_split();
    let _ = wr.write_all(b"200 hi\r\n").await;
    let mut reader = BufReader::new(rd);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let command = line.to_ascii_uppercase();
        let reply: &[u8] = if command.starts_with("AUTHINFO USER") {
            b"381 more\r\n"
        } else if command.starts_with("AUTHINFO PASS") {
            b"281 ok\r\n"
        } else if command.starts_with("GROUP") {
            b"211 1 1 1 g\r\n"
        } else if command.starts_with("BODY") {
            let _ = wr
                .write_all(b"222 0 body\r\n=ybegin part=1 line=128 size=1000 name=x\r\n")
                .await;
            reset(reader.into_inner().reunite(wr).unwrap());
            return;
        } else {
            b"111 20260101000000\r\n"
        };
        let _ = wr.write_all(reply).await;
    }
}

/// Articles a server never finished sending aren't missing: when that is
/// what failed, the job fails as a lost connection (resumable, so the app
/// pauses its queue), not "100% of articles are missing". Resuming against a
/// healthy server then finishes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_that_resets_every_body_fails_as_a_lost_connection() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let port = listen(accepted.clone(), resetting_session).await;
    let temp = tempfile::tempdir().unwrap();
    let (articles, ids, sizes, full) = make_file_articles("x.bin", "x", 3, 1_000);
    let nzb = write_nzb(temp.path(), "x", &[("x.bin", &ids, &sizes)]);
    let out = temp.path().join("X");
    let mut cfg = config(port, temp.path(), 2);
    cfg.usenet.retry_attempts = 1;
    let engine = Engine::new(cfg).unwrap();
    let first = wait(
        &engine.start(request(&nzb, &out), Arc::new(Recorder::default())),
        60,
    )
    .await;
    assert_eq!(first.outcome, Outcome::Failed, "{first:?}");
    assert_eq!(first.error_kind, Some(ErrorKind::Connect), "{first:?}");
    assert!(first.resumable, "{first:?}");
    let message = first.message.clone().unwrap_or_default();
    assert!(plain(&message) && !message.contains("missing"), "{message}");
    assert!(out.join(SIDECAR).exists());

    let healthy = Arc::new(MockServerState {
        articles,
        ..Default::default()
    });
    let port = spawn_server(healthy).await;
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let second = wait(
        &engine.start(request(&nzb, &out), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_eq!(second.outcome, Outcome::Completed, "{:?}", second.message);
    assert_eq!(std::fs::read(out.join("x.bin")).unwrap(), full);
}

// --- A stop as post-processing ends ------------------------------------------------

/// Stops the job from another thread `delay_us` after `phase` begins.
struct StopAfterPhase {
    phase: JobPhase,
    delay_us: u64,
    job: OnceLock<JobHandle>,
}

impl JobObserver for StopAfterPhase {
    fn on_event(&self, event: JobEvent) {
        if event != JobEvent::Phase(self.phase) {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.job.get().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let job = self.job.get().expect("job handle").clone();
        let delay = Duration::from_micros(self.delay_us);
        std::thread::spawn(move || {
            let until = Instant::now() + delay;
            while Instant::now() < until {
                std::hint::spin_loop();
            }
            job.stop();
        });
    }
}

/// A stop can land just after post-processing finished, before the job
/// ends. Whatever the timing, the end is consistent: a job with nothing left
/// to do is Completed (and leaves no sidecar); a Stopped one is resumable,
/// keeps its sidecar, and finishes when started again. (A sweep over the
/// timing; `engine::job`'s unit tests pin the exact window.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_as_post_processing_ends_leaves_a_consistent_job() {
    let (rar, rar_size) = (
        build_part("A.rar", 1, 1, 1, PLAIN_RAR.len() as u64, PLAIN_RAR),
        PLAIN_RAR.len() as u64 + 64,
    );
    let (mut articles, big_ids, big_sizes, _) = make_file_articles("B.bin", "b", 4, 10_000);
    articles.push(MockArticle {
        message_id: "a@t".into(),
        body: rar,
    });
    let state = Arc::new(MockServerState {
        articles,
        ..Default::default()
    });
    let port = spawn_server(state).await;

    const RUNS: u64 = 300;
    let mut inconsistent = Vec::new();
    let mut stopped = 0;
    for i in 0..RUNS {
        let temp = tempfile::tempdir().unwrap();
        let nzb = write_nzb(
            temp.path(),
            "job",
            &[
                ("A.rar", &["a@t".to_string()], &[rar_size]),
                ("B.bin", &big_ids, &big_sizes),
            ],
        );
        let out = temp.path().join("Job");
        let mut cfg = config(port, temp.path(), 2);
        cfg.post_processing.auto_extract_rar = true;
        cfg.post_processing.delete_rar_after_extract = i % 2 == 0;
        let engine = Engine::new(cfg).unwrap();
        let observer = Arc::new(StopAfterPhase {
            phase: JobPhase::Extracting,
            delay_us: (i * 7) % 2000,
            job: OnceLock::new(),
        });
        let job = engine.start(request(&nzb, &out), observer.clone());
        let _ = observer.job.set(job.clone());
        let first = wait(&job, 30).await;
        let sidecar = out.join(SIDECAR).exists();
        match first.outcome {
            Outcome::Completed if !sidecar => continue,
            Outcome::Stopped if first.resumable && sidecar => stopped += 1,
            _ => {
                inconsistent.push(format!(
                    "+{}us: {:?} resumable={} {:?}, folder {:?}",
                    observer.delay_us,
                    first.outcome,
                    first.resumable,
                    first.message,
                    names_in(&out)
                ));
                continue;
            }
        }
        let second = wait(
            &engine.start(request(&nzb, &out), Arc::new(Recorder::default())),
            30,
        )
        .await;
        if second.outcome != Outcome::Completed || !out.join("VERSION").exists() {
            inconsistent.push(format!(
                "+{}us resumed: {:?} {:?}, folder {:?}",
                observer.delay_us,
                second.outcome,
                second.message,
                names_in(&out)
            ));
        }
    }
    eprintln!("{stopped} of {RUNS} runs stopped");
    assert!(inconsistent.is_empty(), "{inconsistent:#?}");
}

// --- Plain warnings ------------------------------------------------------------------

/// Unusable resume data is reported in a plain sentence, not with the
/// reason in parentheses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_damaged_resume_record_is_reported_plainly() {
    let (articles, ids, sizes, _) = make_file_articles("doc.bin", "d", 2, 1_000);
    let port = spawn_server(Arc::new(MockServerState {
        articles,
        ..Default::default()
    }))
    .await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "doc", &[("doc.bin", &ids, &sizes)]);
    let out = temp.path().join("Doc");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join(SIDECAR), "{not json").unwrap();
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let recorder = Arc::new(Recorder::default());
    let summary = wait(&engine.start(request(&nzb, &out), recorder.clone()), 30).await;
    assert_eq!(summary.outcome, Outcome::Completed);
    let warnings = recorder.warnings();
    let warning = warnings
        .iter()
        .find(|w| w.contains("resume data"))
        .expect("a warning about the resume data");
    assert!(plain(warning), "{warning}");
}

/// Restores a folder's permissions when dropped (so the temp dir can go).
#[cfg(unix)]
struct ReadOnly(PathBuf);

#[cfg(unix)]
impl ReadOnly {
    fn new(dir: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        Self(dir.to_path_buf())
    }
}

#[cfg(unix)]
impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// An archive that can't be extracted because the folder isn't writable
/// says so in plain words ("Could not extract A.rar (cannot create a working
/// folder: Permission denied (os error 13))." was the warning).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unwritable_folder_is_reported_plainly_when_extracting() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("Job");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("A.rar"), PLAIN_RAR).unwrap();
    let mut cfg = make_config("127.0.0.1", 9, temp.path().to_path_buf());
    cfg.post_processing.auto_extract_rar = true;
    let engine = Engine::new(cfg).unwrap();
    let recorder = Arc::new(Recorder::default());
    let summary = {
        let _read_only = ReadOnly::new(&dir);
        wait(
            &engine.reprocess(dir.clone(), Vec::new(), recorder.clone()),
            30,
        )
        .await
    };
    assert_eq!(summary.archives_failed, 1, "{summary:?}");
    let warnings = recorder.warnings();
    let warning = warnings
        .iter()
        .find(|w| w.starts_with("Could not extract A.rar"))
        .unwrap_or_else(|| panic!("{warnings:?}"));
    assert!(plain(warning), "{warning}");
    assert!(warning.contains("writable"), "{warning}");
}

/// A file that can't be created in the job folder is reported in plain
/// words too ("Could not create /path/x.bin.partial: Permission denied (os
/// error 13)." was the warning).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unwritable_folder_is_reported_plainly_when_downloading() {
    let (articles, ids, sizes, _) = make_file_articles("doc.bin", "d", 2, 1_000);
    let port = spawn_server(Arc::new(MockServerState {
        articles,
        ..Default::default()
    }))
    .await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "doc", &[("doc.bin", &ids, &sizes)]);
    let out = temp.path().join("Doc");
    std::fs::create_dir_all(&out).unwrap();
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let recorder = Arc::new(Recorder::default());
    {
        let _read_only = ReadOnly::new(&out);
        wait(&engine.start(request(&nzb, &out), recorder.clone()), 30).await;
    }
    let warnings = recorder.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("Could not create doc.bin")),
        "{warnings:?}"
    );
    for warning in &warnings {
        assert!(plain(warning), "{warning}");
    }
}

// --- The job's connection check ------------------------------------------------------

/// A server that turns one connection away (400, "try later") doesn't fail
/// the job: the connection check is tried again, with a pause in between.
/// One that keeps refusing fails after `retry_attempts` more tries; a
/// rejected password fails at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_first_connection_is_tried_again() {
    let (articles, ids, sizes, full) = make_file_articles("doc.bin", "d", 4, 1_000);
    let state = Arc::new(MockServerState {
        articles,
        ..Default::default()
    });
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "doc", &[("doc.bin", &ids, &sizes)]);

    // The first connection is refused, later ones served.
    let accepted = Arc::new(AtomicUsize::new(0));
    let served = state.clone();
    let port = listen(accepted.clone(), move |mut sock, n| {
        let state = served.clone();
        async move {
            if n == 0 {
                let _ = sock
                    .write_all(b"400 Service temporarily unavailable\r\n")
                    .await;
            } else {
                let _ = handle_client(sock, state).await;
            }
        }
    })
    .await;
    let out = temp.path().join("Doc");
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let summary = wait(
        &engine.start(request(&nzb, &out), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(std::fs::read(out.join("doc.bin")).unwrap(), full);

    // Always refused: 1 + retry_attempts tries, then Connect.
    let accepted = Arc::new(AtomicUsize::new(0));
    let port = listen(accepted.clone(), |mut sock, _| async move {
        let _ = sock.write_all(b"502 Too many connections\r\n").await;
    })
    .await;
    let mut cfg = config(port, temp.path(), 2);
    cfg.usenet.retry_attempts = 2;
    let engine = Engine::new(cfg).unwrap();
    let summary = wait(
        &engine.start(
            request(&nzb, &temp.path().join("Refused")),
            Arc::new(Recorder::default()),
        ),
        30,
    )
    .await;
    assert_eq!(summary.outcome, Outcome::Failed);
    assert_eq!(summary.error_kind, Some(ErrorKind::Connect));
    assert_eq!(accepted.load(Ordering::SeqCst), 3);

    // A rejected password: once, at once.
    let rejecting = Arc::new(MockServerState {
        reject_auth: true,
        ..Default::default()
    });
    let accepted = Arc::new(AtomicUsize::new(0));
    let port = listen(accepted.clone(), move |sock, _| {
        let state = rejecting.clone();
        async move {
            let _ = handle_client(sock, state).await;
        }
    })
    .await;
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let summary = wait(
        &engine.start(
            request(&nzb, &temp.path().join("Rejected")),
            Arc::new(Recorder::default()),
        ),
        30,
    )
    .await;
    assert_eq!(summary.error_kind, Some(ErrorKind::Auth));
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

// --- reprocess on a folder with known holes ---------------------------------------------

/// The job record knows an article never arrived. With no PAR2 to repair it,
/// `reprocess` must not call the folder complete and delete that record
/// (the next run would then take the holes for a whole file).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reprocess_honours_the_articles_the_record_says_are_missing() {
    let (articles, ids, sizes, _) = make_file_articles("doc.bin", "d", 4, 1_000);
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "doc", &[("doc.bin", &ids, &sizes)]);
    let out = temp.path().join("Doc");
    let state = Arc::new(MockServerState {
        articles,
        missing_ids: vec![ids[1].clone()],
        ..Default::default()
    });
    let port = spawn_server(state).await;
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let first = wait(
        &engine.start(request(&nzb, &out), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_eq!(first.outcome, Outcome::Failed, "{first:?}");
    assert!(out.join(SIDECAR).exists());

    let again = wait(
        &engine.reprocess(out.clone(), Vec::new(), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_ne!(again.outcome, Outcome::Completed, "{again:?}");
    // The same verdict as the download's own.
    assert_eq!(again.outcome, first.outcome);
    assert_eq!(again.message, first.message);
    assert_eq!(again.articles_failed, 1, "{again:?}");
    assert!(out.join(SIDECAR).exists(), "the record was deleted");

    // Still known the next time: the same verdict, the record kept.
    let once_more = wait(
        &engine.reprocess(out.clone(), Vec::new(), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_eq!(once_more.outcome, again.outcome);
    assert_eq!(once_more.message, again.message);
    assert!(out.join(SIDECAR).exists());
}
