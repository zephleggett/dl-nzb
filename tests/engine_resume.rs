//! Resume after a stop, a crash or a relaunch, against the mock NNTP server:
//! the job sidecar (`.dl-nzb-job.json`) lets `start()` on the same folder skip
//! every article and file an earlier session finished, and lets `reprocess()`
//! skip a PAR2 verification that already passed on unchanged files.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::engine::{Engine, JobEvent, JobPhase, JobRequest, Outcome, Preflight};
use dl_nzb::serde_json;

const SIDECAR: &str = ".dl-nzb-job.json";
/// Declared article size that keeps the adaptive pipeline window at its
/// minimum (4 requests per connection), so "in flight at the stop" is small.
const BIG_ARTICLE: u64 = 1_100_000;

fn downloaded_at_least(bytes: u64) -> impl Fn(&JobEvent) -> bool {
    move |e| matches!(e, JobEvent::Progress(p) if p.phase == JobPhase::Downloading && p.bytes_done >= bytes)
}

fn never_scan(nzb: &Path, out: &Path) -> JobRequest {
    JobRequest {
        preflight: Preflight::Never,
        ..JobRequest::new(nzb, out)
    }
}

/// The sidecar's recorded article positions for file `file` (in NZB order),
/// or `None` when there is no readable sidecar.
fn recorded(out: &Path, file: usize) -> Option<Vec<u32>> {
    let bytes = std::fs::read(out.join(SIDECAR)).ok()?;
    let doc: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let ranges = doc["files"][file]["done"].as_array()?;
    Some(
        ranges
            .iter()
            .flat_map(|r| {
                let first = r[0].as_u64().unwrap() as u32;
                let last = r[1].as_u64().unwrap() as u32;
                first..=last
            })
            .collect(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_job_resumes_without_fetching_finished_articles_again() {
    let count = 60;
    let (articles, ids, _, full) = make_file_articles("show.bin", "r", count, 20_000);
    let sizes = vec![BIG_ARTICLE; count];
    let total: u64 = sizes.iter().sum();
    let state = Arc::new(MockServerState {
        articles,
        body_delay: Duration::from_millis(25),
        ..Default::default()
    });
    let port = spawn_server(state.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "show", &[("show.bin", &ids, &sizes)]);
    let out = temp.path().join("Show");
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();

    // Session 1 (Auto preflight scans: there is no PAR2), stopped a third in.
    let first = Arc::new(Recorder::default());
    let job = engine.start(JobRequest::new(&nzb, &out), first.clone());
    first.wait_for(downloaded_at_least(total / 3)).await;
    job.stop();
    let summary = wait(&job, 10).await;
    assert_eq!(summary.outcome, Outcome::Stopped);
    assert!(summary.resumable);
    assert!(out.join("show.bin.partial").exists());
    let done = recorded(&out, 0).expect("a sidecar after the stop");
    assert!(
        !done.is_empty() && done.len() < count,
        "{} recorded",
        done.len()
    );

    // Session 2: same folder, same NZB.
    let second = Arc::new(Recorder::default());
    let job = engine.start(JobRequest::new(&nzb, &out), second.clone());
    let summary = wait(&job, 30).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert!(!summary.resumable);
    assert_eq!(summary.articles_total, count as u64);
    assert_eq!(summary.articles_failed, 0);
    assert_eq!(std::fs::read(out.join("show.bin")).unwrap(), full);
    assert!(
        !out.join(SIDECAR).exists(),
        "a completed job leaves no sidecar"
    );
    // A continued download doesn't scan again, and its progress starts where
    // the job was.
    assert_eq!(
        second.phases(),
        vec![JobPhase::Connecting, JobPhase::Downloading]
    );
    let start = second.first_progress(JobPhase::Downloading).unwrap();
    assert_eq!(start.bytes_done, done.len() as u64 * BIG_ARTICLE);
    assert_eq!(start.bytes_total, total);
    assert!(second.warnings().is_empty(), "{:?}", second.warnings());

    // Recorded articles were never fetched again; the only repeats are the
    // requests in flight at the stop (2 connections x a window of 4).
    for (i, id) in ids.iter().enumerate() {
        let n = state.body_count(id);
        if done.contains(&(i as u32)) {
            assert_eq!(n, 1, "{id} was recorded but fetched again");
        } else {
            assert!((1..=2).contains(&n), "{id} fetched {n} times");
        }
    }
    let repeats = state.total_body_requests() - count;
    assert!(repeats <= 2 * 4, "{repeats} articles fetched twice");
}

#[test]
fn a_crashed_job_resumes_from_its_last_save() {
    // The server outlives the "crashed" runtime, like a real one would.
    let server = tokio::runtime::Runtime::new().unwrap();
    let count = 60;
    let (articles, ids, _, full) = make_file_articles("crash.bin", "c", count, 20_000);
    let sizes = vec![BIG_ARTICLE; count];
    let state = Arc::new(MockServerState {
        articles,
        // ~3 s in all over 2 connections: the 2 s periodic save lands mid-way.
        body_delay: Duration::from_millis(100),
        ..Default::default()
    });
    let port = server.block_on(spawn_server(state.clone()));
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "crash", &[("crash.bin", &ids, &sizes)]);
    let out = temp.path().join("Crash");

    // Session 1 on its own runtime, torn down mid-download without a stop:
    // no final save, the sidecar is whatever the last periodic save wrote.
    // With fsync on, each save first flushes the data it describes.
    let crashed = dl_nzb::engine::runtime().unwrap();
    let mut durable = config(port, temp.path(), 2);
    durable.tuning.fsync_on_finalize = true;
    let engine = Engine::new(durable).unwrap();
    let job = {
        let _entered = crashed.enter();
        engine.start(never_scan(&nzb, &out), Arc::new(Recorder::default()))
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while recorded(&out, 0).is_none_or(|done| done.is_empty()) {
        assert!(Instant::now() < deadline, "no periodic save");
        std::thread::sleep(Duration::from_millis(10));
    }
    crashed.shutdown_background();
    // Writes already handed to blocking threads land; nothing else runs.
    std::thread::sleep(Duration::from_millis(300));
    drop((job, engine));
    let done = recorded(&out, 0).unwrap();
    assert!(done.len() < count, "crashed too late to test");
    assert!(!out.join("crash.bin").exists());

    // Session 2: a new runtime and engine, as after a relaunch.
    let relaunched = dl_nzb::engine::runtime().unwrap();
    let summary = relaunched.block_on(async {
        let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
        let job = engine.start(never_scan(&nzb, &out), Arc::new(Recorder::default()));
        wait(&job, 60).await
    });
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(std::fs::read(out.join("crash.bin")).unwrap(), full);
    assert!(!out.join(SIDECAR).exists());
    for i in &done {
        assert_eq!(state.body_count(&ids[*i as usize]), 1, "article {i}");
    }
    assert!(state.total_body_requests() < 2 * count);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finalized_files_are_kept_and_a_missing_partial_is_fetched_again() {
    // big.bin's articles are queued first and finish; slow.bin's last article
    // never arrives, so the stop leaves it partial with three articles written.
    let (mut articles, big_ids, _, big_full) = make_file_articles("big.bin", "b", 6, 20_000);
    let (slow_articles, slow_ids, _, slow_full) = make_file_articles("slow.bin", "s", 4, 20_000);
    articles.extend(slow_articles);
    let big_sizes = vec![BIG_ARTICLE; 6];
    let slow_sizes = vec![BIG_ARTICLE; 4];
    let stalling = Arc::new(MockServerState {
        articles,
        hang_ids: vec!["s4@t".into()],
        ..Default::default()
    });
    let port = spawn_server(stalling.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(
        temp.path(),
        "two",
        &[
            ("big.bin", &big_ids, &big_sizes),
            ("slow.bin", &slow_ids, &slow_sizes),
        ],
    );
    let out = temp.path().join("Two");

    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let first = Arc::new(Recorder::default());
    let job = engine.start(never_scan(&nzb, &out), first.clone());
    first.wait_for(downloaded_at_least(9 * BIG_ARTICLE)).await;
    job.stop();
    assert_eq!(wait(&job, 10).await.outcome, Outcome::Stopped);
    assert_eq!(std::fs::read(out.join("big.bin")).unwrap(), big_full);
    assert!(out.join("slow.bin.partial").exists());
    assert_eq!(recorded(&out, 1).unwrap().len(), 3);

    // The partial file goes missing: its articles must be fetched again
    // rather than trusted. big.bin stays as it is.
    std::fs::remove_file(out.join("slow.bin.partial")).unwrap();
    let (articles, ..) = make_file_articles("big.bin", "b", 6, 20_000);
    let (more, ..) = make_file_articles("slow.bin", "s", 4, 20_000);
    let healthy = Arc::new(MockServerState {
        articles: articles.into_iter().chain(more).collect(),
        ..Default::default()
    });
    let port = spawn_server(healthy.clone()).await;
    let engine = Engine::new(config(port, temp.path(), 2)).unwrap();
    let second = Arc::new(Recorder::default());
    let job = engine.start(never_scan(&nzb, &out), second.clone());
    let summary = wait(&job, 30).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(std::fs::read(out.join("big.bin")).unwrap(), big_full);
    assert_eq!(std::fs::read(out.join("slow.bin")).unwrap(), slow_full);
    for id in &big_ids {
        assert_eq!(healthy.body_count(id), 0, "{id} of a finalized file");
    }
    for id in &slow_ids {
        assert_eq!(healthy.body_count(id), 1, "{id}");
    }
    let start = second.first_progress(JobPhase::Downloading).unwrap();
    assert_eq!(start.bytes_done, 6 * BIG_ARTICLE);
    assert_eq!((start.files_done, start.files_total), (1, 2));
    assert!(!out.join(SIDECAR).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_data_from_another_nzb_or_damaged_is_not_trusted() {
    let sidecars = [
        (
            // Another NZB's sidecar claiming every article of a same-named file.
            r#"{"version":1,"nzb":"00000000000000000000000000000000","data_done":false,"recovery":"pending","files":[{"name":"movie.bin","segments":6,"done":[[0,5]],"failed":[],"max_byte":240000,"finalized":false}]}"#,
            "different NZB",
        ),
        ("{not json", "could not be used"),
    ];
    for (i, (sidecar, warning)) in sidecars.into_iter().enumerate() {
        let (articles, ids, sizes, full) = make_file_articles("movie.bin", "m", 6, 40_000);
        let state = Arc::new(MockServerState {
            articles,
            ..Default::default()
        });
        let port = spawn_server(state.clone()).await;
        let temp = tempfile::tempdir().unwrap();
        let nzb = write_nzb(temp.path(), "movie", &[("movie.bin", &ids, &sizes)]);
        let out = temp.path().join("Movie");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join(SIDECAR), sidecar).unwrap();
        std::fs::write(out.join("movie.bin.partial"), vec![0xAAu8; 300_000]).unwrap();

        let engine = Engine::new(config(port, temp.path(), 4)).unwrap();
        let recorder = Arc::new(Recorder::default());
        let job = engine.start(never_scan(&nzb, &out), recorder.clone());
        let summary = wait(&job, 30).await;
        assert_eq!(summary.outcome, Outcome::Completed, "case {i}");
        assert!(
            recorder.warnings().iter().any(|w| w.contains(warning)),
            "case {i}: {:?}",
            recorder.warnings()
        );
        assert_eq!(
            std::fs::read(out.join("movie.bin")).unwrap(),
            full,
            "case {i}"
        );
        for id in &ids {
            assert_eq!(state.body_count(id), 1, "case {i}: {id}");
        }
        assert!(!out.join(SIDECAR).exists(), "case {i}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn needs_password_keeps_the_sidecar_and_skips_a_verification_that_passed() {
    // An encrypted archive protected by PAR2, served by the mock server.
    let source = tempfile::tempdir().unwrap();
    let rar = source.path().join("secret.rar");
    std::fs::write(&rar, CRYPTED_RAR).unwrap();
    let par2_paths = par2_rs::Par2Creator::new(vec![rar])
        .unwrap()
        .with_block_size(2048)
        .unwrap()
        .with_redundancy(100.0)
        .unwrap()
        .create()
        .unwrap();
    let mut articles = Vec::new();
    let mut files: Vec<(String, Vec<String>, Vec<u64>)> = Vec::new();
    let mut add = |name: String, bytes: &[u8], id: String| {
        articles.push(MockArticle {
            message_id: id.clone(),
            body: build_part(&name, 1, 1, 1, bytes.len() as u64, bytes),
        });
        files.push((name, vec![id], vec![bytes.len() as u64 + 64]));
    };
    add("secret.rar".into(), CRYPTED_RAR, "rar@t".into());
    for (i, path) in par2_paths.iter().enumerate() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        add(name, &std::fs::read(path).unwrap(), format!("par{i}@t"));
    }
    let state = Arc::new(MockServerState {
        articles,
        ..Default::default()
    });
    let port = spawn_server(state.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let listed: Vec<(&str, &[String], &[u64])> = files
        .iter()
        .map(|(n, i, s)| (n.as_str(), i.as_slice(), s.as_slice()))
        .collect();
    let nzb = write_nzb(temp.path(), "secret", &listed);
    let out = temp.path().join("Secret");
    let mut config = config(port, temp.path(), 2);
    config.post_processing.auto_par2_repair = true;
    config.post_processing.auto_extract_rar = true;
    let engine = Engine::new(config).unwrap();

    // Downloaded clean (PAR2 trusts the wire checksums), then the archive
    // needs a password: the sidecar stays, with PAR2's verdict.
    let summary = wait(
        &engine.start(never_scan(&nzb, &out), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_eq!(
        summary.outcome,
        Outcome::NeedsPassword,
        "{:?}",
        summary.message
    );
    assert!(summary.par2.verified_ok);
    assert!(!summary.resumable, "nothing left that start() would do");
    assert!(out.join(SIDECAR).exists());
    let requests = state.total_body_requests();

    // Reprocessing (e.g. with a password) doesn't verify again.
    let recorder = Arc::new(Recorder::default());
    let summary = wait(
        &engine.reprocess(out.clone(), Vec::new(), recorder.clone()),
        30,
    )
    .await;
    assert_eq!(summary.outcome, Outcome::NeedsPassword);
    assert!(
        !recorder.phases().contains(&JobPhase::Verifying),
        "{:?}",
        recorder.phases()
    );
    assert!(summary.par2.verified_ok && !summary.par2.ran);
    assert!(summary.par2.skipped_reason.unwrap().contains("earlier run"));
    assert!(out.join(SIDECAR).exists());

    // start() on the finished folder goes straight to post-processing: no
    // connection, no download, no verification.
    let recorder = Arc::new(Recorder::default());
    let summary = wait(&engine.start(never_scan(&nzb, &out), recorder.clone()), 30).await;
    assert_eq!(summary.outcome, Outcome::NeedsPassword);
    assert!(
        recorder
            .phases()
            .iter()
            .all(|p| matches!(p, JobPhase::Extracting | JobPhase::Renaming)),
        "{:?}",
        recorder.phases()
    );
    assert_eq!(state.total_body_requests(), requests);
    assert!(summary.par2.verified_ok && !summary.par2.ran);

    // A file that changed since is verified for real.
    let changed = std::fs::File::options()
        .write(true)
        .open(out.join("secret.rar"))
        .unwrap();
    changed
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    drop(changed);
    let recorder = Arc::new(Recorder::default());
    let summary = wait(
        &engine.reprocess(out.clone(), Vec::new(), recorder.clone()),
        30,
    )
    .await;
    assert_eq!(summary.outcome, Outcome::NeedsPassword);
    assert!(
        recorder.phases().contains(&JobPhase::Verifying),
        "{:?}",
        recorder.phases()
    );
    assert!(summary.par2.ran && summary.par2.verified_ok);
}
