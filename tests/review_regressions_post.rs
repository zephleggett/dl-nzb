//! Regressions from the post-processing review: resuming after PAR2 deleted
//! its files or an archive was extracted and deleted, the free-space check on
//! resume, no post-processing after a download that lost the server, links
//! and file copies inside archives, and jobs extracting at the same time.

mod common;
#[path = "support/rar5.rs"]
mod rar5;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::engine::{
    Engine, ErrorKind, JobEvent, JobHandle, JobObserver, JobPhase, JobRequest, Outcome, Preflight,
};
use rar5::Rar5;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

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

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A single-part yEnc body without a `pcrc32`, so the wire can't vouch for
/// the data and PAR2 really verifies it.
fn article_without_crc(name: &str, plain: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "=ybegin part=1 total=1 line=128 size={} name={}\r\n",
            plain.len(),
            name
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("=ypart begin=1 end={}\r\n", plain.len()).as_bytes());
    body.extend_from_slice(&yenc_encode(plain));
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("=yend size={} part=1\r\n", plain.len()).as_bytes());
    body
}

/// A release: files posted under one name (`posted`) whose PAR2 set knows
/// them under another (`real`), plus the PAR2 index only (no recovery
/// volumes), or every PAR2 file with `with_recovery`.
struct Release {
    temp: tempfile::TempDir,
    nzb: PathBuf,
    articles: Vec<MockArticle>,
    /// Message id of each posted file, by posted name.
    ids: Vec<(String, String)>,
}

impl Release {
    fn new(files: &[(&str, &str, Vec<u8>)], with_recovery: bool) -> Self {
        let source = tempfile::tempdir().unwrap();
        let mut sources = Vec::new();
        for (_, real, bytes) in files {
            let path = source.path().join(real);
            std::fs::write(&path, bytes).unwrap();
            sources.push(path);
        }
        let mut par2 = par2_rs::Par2Creator::new(sources)
            .unwrap()
            .with_block_size(2048)
            .unwrap()
            .with_redundancy(50.0)
            .unwrap()
            .create()
            .unwrap();
        par2.sort_by_key(|p| std::fs::metadata(p).unwrap().len());
        if !with_recovery {
            par2.truncate(1); // the index is the smallest
        }

        let mut posted: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(name, _, bytes)| (name.to_string(), bytes.clone()))
            .collect();
        for path in &par2 {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            posted.push((name, std::fs::read(path).unwrap()));
        }

        let mut articles = Vec::new();
        let mut ids = Vec::new();
        let mut listed: Vec<(String, Vec<String>, Vec<u64>)> = Vec::new();
        for (i, (name, bytes)) in posted.iter().enumerate() {
            let id = format!("f{i}@t");
            articles.push(MockArticle {
                message_id: id.clone(),
                body: article_without_crc(name, bytes),
            });
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
            .find(|(name, _)| name == posted)
            .map(|(_, id)| id.clone())
            .unwrap()
    }

    fn state(&self) -> MockServerState {
        MockServerState {
            articles: self
                .articles
                .iter()
                .map(|a| MockArticle {
                    message_id: a.message_id.clone(),
                    body: a.body.clone(),
                })
                .collect(),
            ..Default::default()
        }
    }

    async fn serve(&self) -> u16 {
        spawn_server(Arc::new(self.state())).await
    }

    fn folder(&self) -> PathBuf {
        self.temp.path().join("Job")
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
        wait(&job, 30).await
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

/// With `delete_par2_after_repair`, PAR2 deletes its files once it verified
/// the download. A stop during the extraction that follows used to lose that
/// verdict, so the resumed job looked for the deleted PAR2 files, failed, and
/// never extracted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_during_extraction_after_par2_deleted_its_files_resumes_and_extracts() {
    let release = Release::new(&[("plain.rar", "plain.rar", PLAIN_RAR.to_vec())], false);
    let port = release.serve().await;
    let out = release.folder();
    let mut cfg = config(port, release.temp.path(), 2);
    cfg.post_processing.auto_par2_repair = true;
    cfg.post_processing.auto_extract_rar = true;
    cfg.post_processing.delete_par2_after_repair = true;
    let engine = Engine::new(cfg).unwrap();

    let observer = StopWhen::new(|e| *e == JobEvent::Phase(JobPhase::Extracting));
    let first = observer.run(&engine, request(&release.nzb, &out)).await;
    assert_eq!(first.outcome, Outcome::Stopped);
    assert!(first.resumable);
    assert!(
        !names_in(&out).iter().any(|n| n.ends_with(".par2")),
        "PAR2 deleted its files: {:?}",
        names_in(&out)
    );
    assert!(!out.join("VERSION").exists());

    let second = wait(
        &engine.start(request(&release.nzb, &out), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_eq!(second.outcome, Outcome::Completed, "{:?}", second.message);
    assert_eq!(second.archives_extracted, 1);
    assert!(
        !second.par2.ran,
        "PAR2 verified these files before the stop"
    );
    assert!(second.par2.verified_ok);
    assert!(out.join("VERSION").is_file());
}

/// With `delete_rar_after_extract`, a stop after the first archive was
/// extracted (and its volumes deleted) used to break the resume: PAR2 found
/// the deleted volumes missing, failed, and the rest was never extracted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_after_one_archive_was_extracted_and_deleted_resumes_with_the_rest() {
    // B is damaged inside (its headers read fine): extracting it fails with
    // a warning, which is where the job is stopped, between A and C.
    let mut damaged = PLAIN_RAR.to_vec();
    damaged[70] ^= 0x55;
    let release = Release::new(
        &[
            ("A.rar", "A.rar", PLAIN_RAR.to_vec()),
            ("B.rar", "B.rar", damaged),
            ("C.rar", "C.rar", PLAIN_RAR.to_vec()),
        ],
        false,
    );
    let port = release.serve().await;
    let out = release.folder();
    let mut cfg = config(port, release.temp.path(), 2);
    cfg.post_processing.auto_par2_repair = true;
    cfg.post_processing.auto_extract_rar = true;
    cfg.post_processing.delete_rar_after_extract = true;
    let engine = Engine::new(cfg).unwrap();

    let observer = StopWhen::new(
        |e| matches!(e, JobEvent::Warning(w) if w.starts_with("Could not extract B.rar")),
    );
    let first = observer.run(&engine, request(&release.nzb, &out)).await;
    assert_eq!(first.outcome, Outcome::Stopped);
    assert!(first.resumable);
    assert!(!out.join("A.rar").exists(), "A was extracted and deleted");
    assert!(out.join("VERSION").is_file());
    assert!(out.join("C.rar").is_file(), "C was not reached");

    let second = wait(
        &engine.start(request(&release.nzb, &out), Arc::new(Recorder::default())),
        30,
    )
    .await;
    assert_eq!(
        second.outcome,
        Outcome::CompletedWithIssues,
        "{:?}",
        second.message
    );
    assert_eq!(
        second.message.as_deref(),
        Some("1 archive could not be extracted.")
    );
    assert!(
        !second.par2.ran,
        "PAR2 verified these files before the stop"
    );
    assert_eq!(second.archives_extracted, 1, "only C was left to extract");
    assert!(!out.join("C.rar").exists(), "C was extracted and deleted");
    assert!(out.join("B.rar").is_file(), "B failed, so it is kept");
}

/// PAR2 name recovery renames an obfuscated file. Resuming after a stop used
/// to check free space as if that file still had to be downloaded under its
/// posted name (a 50 KB hint was "needs about 319 KB").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resuming_a_finished_download_does_not_count_renamed_files_against_free_space() {
    let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let release = Release::new(
        &[("a1b2c3d4e5f6a7b8c9d0.bin", "Real.Name.bin", data)],
        false,
    );
    let port = release.serve().await;
    let out = release.folder();
    let mut cfg = config(port, release.temp.path(), 2);
    cfg.post_processing.auto_par2_repair = true;
    cfg.post_processing.deobfuscate_file_names = true;
    let engine = Engine::new(cfg).unwrap();

    let observer = StopWhen::new(|e| *e == JobEvent::Phase(JobPhase::Verifying));
    let first = observer.run(&engine, request(&release.nzb, &out)).await;
    assert_eq!(first.outcome, Outcome::Stopped);
    assert!(out.join("Real.Name.bin").is_file(), "{:?}", names_in(&out));

    // Nothing is left to download, so 50 KB free is plenty.
    let resume = JobRequest {
        free_space_hint: Some(50_000),
        ..request(&release.nzb, &out)
    };
    let second = wait(&engine.start(resume, Arc::new(Recorder::default())), 30).await;
    assert_ne!(second.error_kind, Some(ErrorKind::DiskFull));
    assert_eq!(second.outcome, Outcome::Completed, "{:?}", second.message);
}

/// A mock server that goes away (every connection closed, new ones refused)
/// once `trigger` has been requested and every id in `first` served.
async fn serve_until(state: Arc<MockServerState>, trigger: String, first: Vec<String>) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let gone = CancellationToken::new();
    {
        let (state, gone) = (state.clone(), gone.clone());
        tokio::spawn(async move {
            while state.body_count(&trigger) == 0
                || first.iter().any(|id| state.body_count(id) == 0)
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            // Let the bodies already being sent arrive.
            tokio::time::sleep(Duration::from_millis(300)).await;
            gone.cancel();
        });
    }
    tokio::spawn(async move {
        loop {
            tokio::select! {
                // Dropping the listener refuses new connections.
                _ = gone.cancelled() => return,
                accepted = listener.accept() => {
                    let Ok((sock, _)) = accepted else { return };
                    let (state, gone) = (state.clone(), gone.clone());
                    tokio::spawn(async move {
                        tokio::select! {
                            _ = handle_client(sock, state) => {}
                            _ = gone.cancelled() => {}
                        }
                    });
                }
            }
        }
    });
    port
}

/// A download that lost the server did not finish, so post-processing must
/// wait: PAR2 name recovery used to rename a finished file anyway, and the
/// resumed download, not finding it under its posted name, fetched it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_download_that_lost_the_server_is_not_post_processed() {
    const OBFUSCATED: &str = "a1b2c3d4e5f6a7b8c9d0.bin";
    let data: Vec<u8> = (0..6000u32).map(|i| (i * 7 % 251) as u8).collect();
    let sample: Vec<u8> = (0..3000u32).map(|i| (i * 13 % 241) as u8).collect();
    let release = Release::new(
        &[
            (OBFUSCATED, "Real.Name.bin", data.clone()),
            // Lower case: it sorts last, so it is requested after the other
            // files even when one connection takes them all in its window
            // (the first connection up starts before the others connect).
            ("sample.bin", "sample.bin", sample.clone()),
        ],
        false,
    );
    let out = release.folder();
    let configure = |port: u16| {
        let mut cfg = config(port, release.temp.path(), 2);
        cfg.tuning.pipeline_depth = 1;
        cfg.usenet.retry_delay = 10;
        cfg.usenet.retry_attempts = 1;
        cfg.post_processing.auto_par2_repair = true;
        cfg.post_processing.deobfuscate_file_names = true;
        cfg
    };

    // The server goes away while sample.bin is being fetched, after the
    // obfuscated file and the PAR2 index arrived.
    let trigger = release.id("sample.bin");
    let others: Vec<String> = release
        .ids
        .iter()
        .filter(|(_, id)| *id != trigger)
        .map(|(_, id)| id.clone())
        .collect();
    let state = MockServerState {
        hang_ids: vec![trigger.clone()],
        ..release.state()
    };
    let port = serve_until(Arc::new(state), trigger, others).await;
    let engine = Engine::new(configure(port)).unwrap();
    let first = wait(
        &engine.start(request(&release.nzb, &out), Arc::new(Recorder::default())),
        60,
    )
    .await;
    assert_eq!(first.outcome, Outcome::Failed, "{:?}", first.message);
    assert!(first.resumable);
    assert!(
        out.join(OBFUSCATED).is_file() && !out.join("Real.Name.bin").exists(),
        "nothing is renamed before the download finishes: {:?}",
        names_in(&out)
    );

    // Back online: the resumed job fetches only sample.bin, then renames.
    let port = release.serve().await;
    let engine = Engine::new(configure(port)).unwrap();
    let second = wait(
        &engine.start(request(&release.nzb, &out), Arc::new(Recorder::default())),
        60,
    )
    .await;
    assert_eq!(second.outcome, Outcome::Completed, "{:?}", second.message);
    assert_eq!(std::fs::read(out.join("Real.Name.bin")).unwrap(), data);
    assert_eq!(std::fs::read(out.join("sample.bin")).unwrap(), sample);
    assert!(
        !out.join(OBFUSCATED).exists(),
        "downloaded twice: {:?}",
        names_in(&out)
    );
}

/// A job folder on the same file system as the crate (so a hard link to one
/// of its files could be made), and an engine for reprocessing it.
fn local_folder(name: &str) -> (tempfile::TempDir, PathBuf, Engine) {
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let dir = temp.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = make_config("127.0.0.1", 9, temp.path().to_path_buf());
    config.post_processing.auto_extract_rar = true;
    (temp, dir, Engine::new(config).unwrap())
}

/// The warnings a reprocess of `dir` produced, and its summary.
async fn reprocess(engine: &Engine, dir: &Path) -> (dl_nzb::JobSummary, Vec<String>) {
    let recorder = Arc::new(Recorder::default());
    let job = engine.reprocess(dir.to_path_buf(), Vec::new(), recorder.clone());
    let summary = wait(&job, 30).await;
    (summary, recorder.warnings())
}

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

/// unRAR resolves a hard link's target against the process's working
/// directory, not the job folder, so an archive could link any file on the
/// volume into the download. Links are not unpacked at all.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn links_in_an_archive_are_not_unpacked() {
    // Tests run in the crate's folder, so `Cargo.toml` is a file a link can reach.
    let victim = std::env::current_dir().unwrap().join("Cargo.toml");
    assert!(victim.is_file());
    let (_temp, dir, engine) = local_folder("Linked");
    let archive = Rar5::new()
        .file("data.txt", b"hello")
        .link("hard.txt", rar5::HARDLINK, "Cargo.toml")
        .link("soft.txt", rar5::UNIX_SYMLINK, "data.txt")
        .build();
    std::fs::write(dir.join("Linked.rar"), archive).unwrap();

    let (summary, warnings) = reprocess(&engine, &dir).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert_eq!(std::fs::read(dir.join("data.txt")).unwrap(), b"hello");
    assert!(
        !same_file(&dir.join("hard.txt"), &victim),
        "a file outside the job folder was linked into it"
    );
    assert!(!dir.join("hard.txt").exists());
    assert!(std::fs::symlink_metadata(dir.join("soft.txt")).is_err());
    assert_eq!(warnings, vec!["Skipped 2 links in Linked.rar.".to_string()]);
}

/// A file copy (RAR5 stores identical files once) is copied from the member
/// it names, inside the job folder; one naming a file outside it fails the
/// archive instead of copying that file in.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_copies_in_an_archive_are_made_inside_the_job_folder() {
    let (_temp, dir, engine) = local_folder("Copies");
    let archive = Rar5::new()
        .file("Sub/data.txt", b"hello")
        .link("copy.txt", rar5::FILE_COPY, "Sub/data.txt")
        .build();
    std::fs::write(dir.join("Copies.rar"), archive).unwrap();
    let (summary, warnings) = reprocess(&engine, &dir).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(std::fs::read(dir.join("copy.txt")).unwrap(), b"hello");
    assert!(!same_file(&dir.join("copy.txt"), &dir.join("Sub/data.txt")));

    let (_temp, dir, engine) = local_folder("Escape");
    let archive = Rar5::new()
        .file("data.txt", b"hello")
        .link("stolen.toml", rar5::FILE_COPY, "Cargo.toml")
        .build();
    std::fs::write(dir.join("Escape.rar"), archive).unwrap();
    let (summary, _) = reprocess(&engine, &dir).await;
    assert_eq!(summary.outcome, Outcome::CompletedWithIssues);
    assert_eq!(summary.archives_failed, 1);
    assert!(!dir.join("stolen.toml").exists());
    assert!(!dir.join("data.txt").exists(), "nothing half-extracted");
}

/// Two jobs may extract at once (one downloads while the other unpacks), but
/// unRAR keeps its error state in process-wide globals: each job's result
/// must still be its own (a refused password must not leak into another job).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn jobs_extracting_at_the_same_time_each_get_their_own_result() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = make_config("127.0.0.1", 9, temp.path().to_path_buf());
    config.post_processing.auto_extract_rar = true;
    let engine = Engine::new(config).unwrap();

    let mut jobs = Vec::new();
    for i in 0..12 {
        let dir = temp.path().join(format!("Job{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        let encrypted = i % 2 == 0;
        let archive = if encrypted { CRYPTED_RAR } else { PLAIN_RAR };
        std::fs::write(dir.join("Archive.rar"), archive).unwrap();
        let passwords = if encrypted {
            vec!["wrong".to_string()]
        } else {
            Vec::new()
        };
        let job = engine.reprocess(dir.clone(), passwords, Arc::new(Recorder::default()));
        jobs.push((encrypted, dir, job));
    }
    for (encrypted, dir, job) in jobs {
        let summary = wait(&job, 60).await;
        if encrypted {
            assert_eq!(summary.outcome, Outcome::NeedsPassword, "{dir:?}");
        } else {
            assert_eq!(
                summary.outcome,
                Outcome::Completed,
                "{dir:?}: {:?}",
                summary.message
            );
            assert!(dir.join("VERSION").is_file());
        }
    }
}

/// Inside the observer's `Finished`, the job already counts as finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_is_finished_when_its_observer_hears_so() {
    #[derive(Default)]
    struct Check {
        job: OnceLock<JobHandle>,
        finished_inside: Mutex<Option<bool>>,
    }
    impl JobObserver for Check {
        fn on_event(&self, event: JobEvent) {
            if let JobEvent::Finished(_) = event {
                let deadline = Instant::now() + Duration::from_secs(5);
                while self.job.get().is_none() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let finished = self.job.get().map(|j| j.is_finished());
                *self.finished_inside.lock().unwrap() = finished;
            }
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let engine = Engine::new(make_config("127.0.0.1", 9, temp.path().into())).unwrap();
    let check = Arc::new(Check::default());
    let job = engine.reprocess(temp.path().to_path_buf(), Vec::new(), check.clone());
    let _ = check.job.set(job.clone());
    wait(&job, 30).await;
    assert_eq!(*check.finished_inside.lock().unwrap(), Some(true));
}
