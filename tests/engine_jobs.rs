//! The engine's job API against the mock NNTP server: event ordering, prompt
//! resumable stop, pause/resume, the pre-flight unrepairable policy, several
//! jobs on one engine, and real error kinds from connection tests.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::engine::{
    Engine, ErrorKind, JobEvent, JobPhase, JobRequest, OnUnrepairable, Outcome, OutputFile,
    Preflight, Verdict,
};
use dl_nzb::DlNzbError;

fn downloading_with_bytes(e: &JobEvent) -> bool {
    matches!(e, JobEvent::Progress(p) if p.phase == JobPhase::Downloading && p.bytes_done > 0)
}

fn request(nzb: &Path, out: &Path, preflight: Preflight) -> JobRequest {
    JobRequest {
        preflight,
        ..JobRequest::new(nzb, out)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_events_are_ordered_and_finished_comes_last_exactly_once() {
    let (articles, ids, sizes, full) = make_file_articles("movie.bin", "m", 6, 40_000);
    let state = Arc::new(MockServerState {
        articles,
        ..Default::default()
    });
    let port = spawn_server(state.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "movie", &[("movie.bin", &ids, &sizes)]);
    let out = temp.path().join("Movie");

    let engine = Engine::new(make_config("127.0.0.1", port, temp.path().into())).unwrap();
    let recorder = Arc::new(Recorder::default());
    // Auto preflight scans because the NZB has no PAR2.
    let job = engine.start(JobRequest::new(&nzb, &out), recorder.clone());
    let summary = wait(&job, 30).await;
    assert!(job.is_finished());
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);

    let events = recorder.events();
    assert!(matches!(
        events.first(),
        Some(JobEvent::Phase(JobPhase::Connecting))
    ));
    assert!(matches!(events.last(), Some(JobEvent::Finished(_))));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, JobEvent::Finished(_)))
            .count(),
        1
    );
    // Every Phase is followed at once by a Progress of that phase, and phases
    // only move forward.
    let mut previous: Option<JobPhase> = None;
    for (i, event) in events.iter().enumerate() {
        match event {
            JobEvent::Phase(phase) => {
                match &events[i + 1] {
                    JobEvent::Progress(p) => assert_eq!(p.phase, *phase),
                    other => panic!("Phase({phase:?}) followed by {other:?}"),
                }
                if let Some(prev) = previous {
                    assert!(*phase > prev, "{phase:?} after {prev:?}");
                }
                previous = Some(*phase);
            }
            JobEvent::Progress(p) => {
                assert!((0.0..=1.0).contains(&p.fraction));
                assert!(p.bytes_done <= p.bytes_total || p.bytes_total == 0);
                assert!(!p.paused);
            }
            _ => {}
        }
    }
    assert_eq!(
        recorder.phases(),
        vec![
            JobPhase::Connecting,
            JobPhase::Checking,
            JobPhase::Downloading
        ]
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, JobEvent::Availability(a) if a.verdict == Verdict::Complete)));

    // The download phase's last progress is complete, in NZB bytes.
    let last = events
        .iter()
        .rev()
        .find_map(|e| match e {
            JobEvent::Progress(p) if p.phase == JobPhase::Downloading => Some(p.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(last.bytes_total, sizes.iter().sum::<u64>());
    assert_eq!(last.bytes_done, last.bytes_total);
    assert_eq!((last.files_done, last.files_total), (1, 1));

    assert_eq!(summary.data_bytes, full.len() as u64);
    assert_eq!((summary.articles_total, summary.articles_failed), (6, 0));
    assert!(summary.wire_bytes > full.len() as u64);
    assert_eq!(
        summary.files,
        vec![OutputFile {
            name: "movie.bin".into(),
            bytes: full.len() as u64
        }]
    );
    assert_eq!(std::fs::read(out.join("movie.bin")).unwrap(), full);

    // Nothing arrives after Finished, even once the ticker would have fired.
    let count = recorder.events().len();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(recorder.events().len(), count);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_is_prompt_and_leaves_a_resumable_partial_file() {
    let (articles, ids, sizes, _) = make_file_articles("movie.bin", "s", 6, 40_000);
    // The server never answers one article: without a prompt stop the job
    // would sit in a 60 s read timeout.
    let state = Arc::new(MockServerState {
        articles,
        hang_ids: vec!["s4@t".into()],
        ..Default::default()
    });
    let port = spawn_server(state.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "movie", &[("movie.bin", &ids, &sizes)]);
    let out = temp.path().join("Movie");

    let engine = Engine::new(make_config("127.0.0.1", port, temp.path().into())).unwrap();
    let recorder = Arc::new(Recorder::default());
    let job = engine.start(request(&nzb, &out, Preflight::Never), recorder.clone());
    recorder.wait_for(downloading_with_bytes).await;
    // Let the stalled article's request reach the server.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let stopped_at = Instant::now();
    job.stop();
    let summary = wait(&job, 5).await;
    assert!(
        stopped_at.elapsed() < Duration::from_secs(2),
        "stop took {:?}",
        stopped_at.elapsed()
    );
    assert_eq!(summary.outcome, Outcome::Stopped);
    assert!(summary.resumable);
    assert!(summary.message.is_some());
    // The incomplete file keeps its .partial name; nothing is presented as done.
    assert!(out.join("movie.bin.partial").exists());
    assert!(!out.join("movie.bin").exists());

    let events = recorder.events();
    assert!(matches!(events.last(), Some(JobEvent::Finished(_))));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, JobEvent::Finished(_)))
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_then_resume_completes_with_every_article_fetched() {
    // Declared sizes of ~1.1 MB keep the pipeline window at its minimum (4),
    // so a pause has unrequested articles left to hold back; the bodies
    // themselves are small, served 25 ms apart.
    let count = 60;
    let (articles, ids, _, full) = make_file_articles("show.bin", "p", count, 20_000);
    let sizes = vec![1_100_000u64; count];
    let state = Arc::new(MockServerState {
        articles,
        body_delay: Duration::from_millis(25),
        ..Default::default()
    });
    let port = spawn_server(state.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "show", &[("show.bin", &ids, &sizes)]);
    let out = temp.path().join("Show");

    let mut config = make_config("127.0.0.1", port, temp.path().into());
    config.usenet.connections = 2;
    config.tuning.max_concurrent_connections = 2;
    let engine = Engine::new(config).unwrap();
    let recorder = Arc::new(Recorder::default());
    let job = engine.start(request(&nzb, &out, Preflight::Never), recorder.clone());

    // Paused before any article: nothing is requested, and progress says so.
    job.pause();
    recorder
        .wait_for(
            |e| matches!(e, JobEvent::Progress(p) if p.phase == JobPhase::Downloading && p.paused),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(state.total_body_requests(), 0);
    assert!(!job.is_finished());

    // Resume, then pause mid-download: the requests in flight are abandoned
    // and no new ones follow while paused.
    job.resume();
    recorder.wait_for(downloading_with_bytes).await;
    job.pause();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let while_paused = state.total_body_requests();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.total_body_requests(), while_paused);
    assert!(
        while_paused < count,
        "paused too late to test ({while_paused})"
    );
    assert!(!job.is_finished());

    job.resume();
    let summary = wait(&job, 30).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(summary.articles_failed, 0);
    assert_eq!(std::fs::read(out.join("show.bin")).unwrap(), full);
    // Pausing never failed an article: only the requests in flight at the
    // pause (at most a window per connection) were asked for again.
    for id in &ids {
        assert!((1..=2).contains(&state.body_count(id)), "{id}");
    }
    let again = ids.iter().filter(|id| state.body_count(id) == 2).count();
    assert!(again <= 2 * 4, "{again} articles fetched twice");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preflight_stop_finishes_unrepairable_without_downloading() {
    let (articles, ids, sizes, _) = make_file_articles("data.bin", "u", 3, 10_000);
    let state = Arc::new(MockServerState {
        articles,
        missing_ids: vec!["u2@t".into(), "u3@t".into()],
        ..Default::default()
    });
    let port = spawn_server(state.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let nzb = write_nzb(temp.path(), "data", &[("data.bin", &ids, &sizes)]);
    let out = temp.path().join("Data");

    let engine = Engine::new(make_config("127.0.0.1", port, temp.path().into())).unwrap();
    let recorder = Arc::new(Recorder::default());
    let job = engine.start(
        JobRequest {
            on_unrepairable: OnUnrepairable::Stop,
            ..request(&nzb, &out, Preflight::Always)
        },
        recorder.clone(),
    );
    let summary = wait(&job, 30).await;

    assert_eq!(summary.outcome, Outcome::Unrepairable);
    assert!(summary.message.as_deref().unwrap().contains("missing"));
    let availability = summary.availability.clone().expect("availability");
    assert_eq!(availability.verdict, Verdict::Unrepairable);
    assert_eq!(
        (availability.articles_total, availability.articles_missing),
        (3, 2)
    );
    assert_eq!(state.total_body_requests(), 0, "nothing downloaded");
    assert_eq!(
        recorder.phases(),
        vec![JobPhase::Connecting, JobPhase::Checking]
    );
    let events = recorder.events();
    let availability_at = events
        .iter()
        .position(|e| matches!(e, JobEvent::Availability(_)))
        .unwrap();
    assert!(availability_at < events.len() - 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_engine_runs_a_job_after_another_was_stopped() {
    let (mut articles, ids_a, sizes_a, _) = make_file_articles("a.bin", "a", 4, 30_000);
    let (articles_b, ids_b, sizes_b, full_b) = make_file_articles("b.bin", "b", 4, 30_000);
    articles.extend(articles_b);
    let state = Arc::new(MockServerState {
        articles,
        hang_ids: vec!["a3@t".into()],
        ..Default::default()
    });
    let port = spawn_server(state.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let nzb_a = write_nzb(temp.path(), "a", &[("a.bin", &ids_a, &sizes_a)]);
    let nzb_b = write_nzb(temp.path(), "b", &[("b.bin", &ids_b, &sizes_b)]);
    let engine = Engine::new(make_config("127.0.0.1", port, temp.path().into())).unwrap();

    // Job A stalls, and is stopped (its stalled connection is discarded).
    let recorder_a = Arc::new(Recorder::default());
    let job_a = engine.start(
        request(&nzb_a, &temp.path().join("A"), Preflight::Never),
        recorder_a.clone(),
    );
    recorder_a.wait_for(downloading_with_bytes).await;
    job_a.stop();
    assert_eq!(wait(&job_a, 5).await.outcome, Outcome::Stopped);

    // Job B on the same engine and pool is unaffected by A's stop.
    let recorder_b = Arc::new(Recorder::default());
    let job_b = engine.start(
        request(&nzb_b, &temp.path().join("B"), Preflight::Never),
        recorder_b,
    );
    let summary = wait(&job_b, 30).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(
        std::fs::read(temp.path().join("B").join("b.bin")).unwrap(),
        full_b
    );

    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_errors_keep_their_real_kind() {
    // Wrong password: an Auth error, not "pool exhausted".
    let rejecting = Arc::new(MockServerState {
        reject_auth: true,
        ..Default::default()
    });
    let port = spawn_server(rejecting).await;
    let temp = tempfile::tempdir().unwrap();
    let mut config = make_config("127.0.0.1", port, temp.path().into());
    config.usenet.password = "s3cret-pw".into();
    let err: DlNzbError = Engine::test_connection(&config.usenet)
        .await
        .expect_err("login must fail");
    assert_eq!(err.kind(), ErrorKind::Auth);
    assert!(
        !err.user_message().contains("s3cret"),
        "no credentials echoed"
    );
    assert!(!err.to_string().contains("s3cret"), "no credentials echoed");

    // A job against that server fails with the same kind.
    let (_, ids, sizes, _) = make_file_articles("x.bin", "x", 1, 1_000);
    let nzb = write_nzb(temp.path(), "x", &[("x.bin", &ids, &sizes)]);
    let engine = Engine::new(config).unwrap();
    let job = engine.start(
        request(&nzb, &temp.path().join("X"), Preflight::Never),
        Arc::new(Recorder::default()),
    );
    let summary = wait(&job, 30).await;
    assert_eq!(summary.outcome, Outcome::Failed);
    assert_eq!(summary.error_kind, Some(ErrorKind::Auth));

    // Nothing listening: a Connect error.
    let closed = make_config("127.0.0.1", pick_free_port(), temp.path().into());
    let err = Engine::test_connection(&closed.usenet).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Connect);

    // A healthy server: greeting and latency.
    let healthy = Arc::new(MockServerState::default());
    let port = spawn_server(healthy).await;
    let config = make_config("127.0.0.1", port, temp.path().into());
    let check = Engine::test_connection(&config.usenet).await.unwrap();
    assert!(check.greeting.starts_with("200"));
    assert!(!check.tls);
}

#[tokio::test]
async fn a_relative_job_folder_is_a_config_error() {
    let temp = tempfile::tempdir().unwrap();
    let (_, ids, sizes, _) = make_file_articles("x.bin", "r", 1, 1_000);
    let nzb = write_nzb(temp.path(), "x", &[("x.bin", &ids, &sizes)]);
    let engine = Engine::new(make_config("127.0.0.1", 9, temp.path().into())).unwrap();
    let summary = wait(
        &engine.start(
            JobRequest::new(&nzb, "relative/folder"),
            Arc::new(Recorder::default()),
        ),
        10,
    )
    .await;
    assert_eq!(summary.outcome, Outcome::Failed);
    assert_eq!(summary.error_kind, Some(ErrorKind::Config));
    assert!(!summary.resumable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn par2_repair_reports_real_block_counts() {
    // A 200 kB file in four 50 kB articles, protected by PAR2 with 10 kB blocks
    // and 8 recovery blocks. The second article is missing on the server, so
    // exactly blocks 5..10 (five blocks) need rebuilding.
    let block = 10_000usize;
    let (articles, ids, sizes, full) = make_file_articles("movie.bin", "d", 4, 50_000);
    let source = tempfile::tempdir().unwrap();
    let original = source.path().join("movie.bin");
    std::fs::write(&original, &full).unwrap();
    let mut par2_paths = par2_rs::Par2Creator::new(vec![original])
        .unwrap()
        .with_block_size(block as u64)
        .unwrap()
        .with_redundancy(40.0)
        .unwrap()
        .create()
        .unwrap();
    par2_paths.sort();

    let mut all_articles = articles;
    let mut par2_files: Vec<(String, Vec<String>, Vec<u64>)> = Vec::new();
    for (i, path) in par2_paths.iter().enumerate() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(path).unwrap();
        let id = format!("par{i}@t");
        let body = build_part(&name, 1, 1, 1, bytes.len() as u64, &bytes);
        all_articles.push(MockArticle {
            message_id: id.clone(),
            body,
        });
        par2_files.push((name, vec![id], vec![bytes.len() as u64 + 64]));
    }
    assert!(par2_files.len() >= 2, "an index and recovery volumes");

    let state = Arc::new(MockServerState {
        articles: all_articles,
        missing_ids: vec!["d2@t".into()],
        ..Default::default()
    });
    let port = spawn_server(state.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let mut listed: Vec<(&str, &[String], &[u64])> = vec![("movie.bin", &ids, &sizes)];
    for (name, ids, sizes) in &par2_files {
        listed.push((name.as_str(), ids.as_slice(), sizes.as_slice()));
    }
    let nzb = write_nzb(temp.path(), "movie", &listed);
    let out = temp.path().join("Movie");

    let mut config = make_config("127.0.0.1", port, temp.path().into());
    config.post_processing.auto_par2_repair = true;
    let engine = Engine::new(config).unwrap();
    let recorder = Arc::new(Recorder::default());
    let job = engine.start(request(&nzb, &out, Preflight::Never), recorder.clone());
    let summary = wait(&job, 60).await;

    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(
        recorder.phases(),
        vec![
            JobPhase::Connecting,
            JobPhase::Downloading,
            JobPhase::DownloadingRecovery,
            JobPhase::Verifying,
            JobPhase::Repairing,
        ]
    );
    let par2 = &summary.par2;
    assert!(par2.ran && par2.verified_ok && par2.repaired, "{par2:?}");
    assert_eq!(par2.damaged_blocks, 5);
    assert_eq!(par2.repaired_blocks, 5);
    assert!(recorder.events().iter().any(|e| matches!(
        e,
        JobEvent::Progress(p) if p.phase == JobPhase::Repairing && p.damaged_blocks == 5
    )));
    assert_eq!(std::fs::read(out.join("movie.bin")).unwrap(), full);
    // Recovery files are not user-facing output.
    assert_eq!(
        summary.files,
        vec![OutputFile {
            name: "movie.bin".into(),
            bytes: full.len() as u64
        }]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reprocess_extracts_plain_archives_and_flags_encrypted_ones() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = make_config("127.0.0.1", 9, temp.path().into());
    config.post_processing.auto_extract_rar = true;
    let engine = Engine::new(config).unwrap();

    let reprocess = |name: &str, bytes: &[u8]| {
        let dir = temp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.rar")), bytes).unwrap();
        let recorder = Arc::new(Recorder::default());
        (
            engine.reprocess(dir, Vec::new(), recorder.clone()),
            recorder,
        )
    };

    let (job, recorder) = reprocess("plain", PLAIN_RAR);
    let summary = wait(&job, 30).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(summary.archives_extracted, 1);
    assert!(recorder.phases().contains(&JobPhase::Extracting));
    // The extracted member is listed; the extracted archive is not.
    let names: Vec<&str> = summary.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["VERSION"]);

    for (name, bytes) in [("crypted", CRYPTED_RAR), ("headers", HEADER_ENCRYPTED_RAR)] {
        let (job, _) = reprocess(name, bytes);
        let summary = wait(&job, 30).await;
        assert_eq!(summary.outcome, Outcome::NeedsPassword, "{name}");
        assert_eq!(summary.archives_extracted, 0, "{name}");
        assert!(summary.message.unwrap().contains("password"), "{name}");
    }
}
