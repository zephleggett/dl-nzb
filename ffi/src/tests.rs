//! Conversions across the boundary, and the `Engine` object end to end
//! without a server (inspect, connection errors, a job that cannot connect).

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dl_nzb::engine as core;
use dl_nzb::DlNzbError;

use super::*;

fn server(host: &str, port: u16) -> ServerConfig {
    ServerConfig {
        host: host.to_string(),
        port,
        ssl: false,
        verify_certificate: true,
        username: "user".to_string(),
        password: "secret".to_string(),
        connections: 2,
        retry_attempts: 0,
    }
}

fn config(host: &str, port: u16) -> EngineConfig {
    EngineConfig {
        server: server(host, port),
        auto_par2_repair: true,
        auto_extract_rar: false,
        delete_rar_after_extract: true,
        delete_par2_after_repair: false,
        deobfuscate_file_names: true,
        download_all_par2: true,
        fsync_on_finalize: false,
        speed_limit_bytes_per_second: Some(1_000_000),
    }
}

/// A port nothing listens on (bound, read, released).
fn unused_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Drives one of the exported async methods the way Swift does: from an
/// executor that is not the engine's runtime.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

const NZB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <head><meta type="title">Some.Show.S01E01.1080p.WEB</meta><meta type="password">pw</meta></head>
  <file poster="p" date="1" subject="&quot;show.mkv&quot; yEnc (1/2)">
    <groups><group>alt.binaries.test</group></groups>
    <segments>
      <segment bytes="1000" number="1">a1@test</segment>
      <segment bytes="500" number="2">a2@test</segment>
    </segments>
  </file>
  <file poster="p" date="1" subject="&quot;show.par2&quot; yEnc (1/1)">
    <groups><group>alt.binaries.test</group></groups>
    <segments><segment bytes="200" number="1">p1@test</segment></segments>
  </file>
</nzb>"#;

fn write_nzb(dir: &Path) -> PathBuf {
    let path = dir.join("show.nzb");
    std::fs::write(&path, NZB).unwrap();
    path
}

/// The NZB above, into `dir/Show`, with the default policies.
fn request(dir: &Path) -> JobRequest {
    JobRequest::new(write_nzb(dir), dir.join("Show"))
}

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<JobEvent>>,
}

impl JobListener for Recorder {
    fn on_event(&self, event: JobEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl Recorder {
    fn events(&self) -> Vec<JobEvent> {
        self.events.lock().unwrap().clone()
    }
}

// MARK: Conversions

#[test]
fn engine_config_round_trips_through_the_engine_config() {
    let ffi = config("news.example.com", 563);
    let back = EngineConfig::from_core(&ffi.to_core());
    // The CLI's speed limit is not imported.
    assert_eq!(
        back,
        EngineConfig {
            speed_limit_bytes_per_second: None,
            ..ffi.clone()
        }
    );
    let core = ffi.to_core();
    assert_eq!(core.usenet.server, "news.example.com");
    assert_eq!(core.usenet.password, "secret");
    assert!(!core.post_processing.auto_extract_rar);
    assert!(core.post_processing.download_all_par2);
    assert_eq!(core.download.speed_limit, Some(1_000_000));
}

#[test]
fn server_config_debug_hides_the_password() {
    let ffi = config("news.example.com", 563);
    for text in [format!("{:?}", ffi.server), format!("{ffi:?}")] {
        assert!(!text.contains("secret"), "{text}");
        assert!(text.contains("news.example.com"), "{text}");
    }
}

#[test]
fn server_host_is_trimmed_and_zero_speed_is_unlimited() {
    let mut ffi = config("  news.example.com \n", 119);
    ffi.speed_limit_bytes_per_second = Some(0);
    let core = ffi.to_core();
    assert_eq!(core.usenet.server, "news.example.com");
    assert_eq!(core.download.speed_limit, None);
}

/// Every kind has its own error case, and gets it back.
#[test]
fn error_cases_match_kinds() {
    use ErrorKind as K;
    for kind in [
        K::Config,
        K::Auth,
        K::Dns,
        K::Connect,
        K::Tls,
        K::Timeout,
        K::Protocol,
        K::Nzb,
        K::Io,
        K::DiskFull,
    ] {
        assert_eq!(EngineError::new(kind, "m").kind(), kind);
    }
}

#[test]
fn events_keep_their_payloads() {
    let progress = core::JobProgress {
        phase: core::JobPhase::Repairing,
        bytes_done: 1,
        bytes_total: 2,
        speed_bps: 3.5,
        eta_secs: Some(4),
        files_done: 5,
        files_total: 6,
        articles_failed: 7,
        fraction: 0.5,
        detail: Some("2 of 5".into()),
        paused: true,
        damaged_blocks: 12,
    };
    assert_eq!(
        JobEvent::from(core::JobEvent::Progress(progress.clone())),
        JobEvent::Progress { progress }
    );

    assert_eq!(
        JobEvent::from(core::JobEvent::Warning("w".into())),
        JobEvent::Warning {
            message: "w".into()
        }
    );
}

#[test]
fn engine_errors_carry_kind_and_sentence() {
    let e: EngineError = DlNzbError::Job {
        kind: ErrorKind::Auth,
        message: "The server rejected the username or password.".into(),
    }
    .into();
    assert_eq!(e.kind(), ErrorKind::Auth);
    assert_eq!(
        e.to_string(),
        "The server rejected the username or password."
    );

    let io: EngineError = std::io::Error::other("boom").into();
    assert_eq!(io.kind(), ErrorKind::Io);
}

// MARK: Engine

#[test]
fn engine_validates_config_and_stores_the_speed_limit() {
    let engine = Engine::new(config("news.example.com", 563)).unwrap();
    assert_eq!(engine.speed_limit(), Some(1_000_000));
    engine.set_speed_limit(Some(0));
    assert_eq!(engine.speed_limit(), None);

    let mut bad = config("news.example.com", 563);
    bad.server.connections = 0;
    let err = engine.update_config(bad).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Config);

    // No server yet is fine: the app creates the engine before onboarding.
    assert!(Engine::new(config("", 563)).is_ok());
}

#[test]
fn inspect_describes_an_nzb() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(config("", 563)).unwrap();
    let nzb = write_nzb(dir.path()).to_string_lossy().into_owned();
    let info = engine.inspect(nzb, None).unwrap();
    assert_eq!(info.title, "Some.Show.S01E01.1080p.WEB");
    assert_eq!(info.passwords, vec!["pw".to_string()]);
    assert_eq!(info.total_bytes, 1700);
    assert_eq!(info.par2_bytes, 200);
    assert_eq!(info.files.len(), 2);
    assert_eq!(info.files[0].segments, 2);
    assert_eq!(info.files[1].kind, FileKind::Par2);
    assert_eq!(info.content_kind, ContentKind::Video);

    let missing = engine
        .inspect("/nonexistent/x.nzb".into(), None)
        .unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::Io);
    let junk = dir.path().join("junk.nzb");
    std::fs::write(&junk, "this is not xml").unwrap();
    let junk = engine
        .inspect(junk.to_string_lossy().into_owned(), None)
        .unwrap_err();
    assert_eq!(junk.kind(), ErrorKind::Nzb);
}

/// The queue's copy (`<id>.nzb`) inspected as the file the user opened: the
/// title falls back to that name and its `{{password}}` joins the NZB's.
#[test]
fn inspect_reads_the_name_the_nzb_was_opened_as() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(config("", 563)).unwrap();
    let copy = dir.path().join("1D4C6F0E.nzb");
    std::fs::write(&copy, NZB.replace("Some.Show.S01E01.1080p.WEB", "")).unwrap();
    let copy = copy.to_string_lossy().into_owned();

    let info = engine
        .inspect(copy.clone(), Some("Some Show{{s3cret}}.nzb".into()))
        .unwrap();
    assert_eq!(info.title, "Some Show");
    assert_eq!(info.passwords, vec!["pw".to_string(), "s3cret".to_string()]);
    assert_eq!(engine.inspect(copy, None).unwrap().title, "1D4C6F0E");
}

#[test]
fn test_connection_reports_a_refused_connection() {
    let engine = Engine::new(config("", 563)).unwrap();
    let err = block_on(engine.test_connection(server("127.0.0.1", unused_port()))).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Connect, "{err}");

    let err = block_on(engine.test_connection(server(" ", 119))).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Config);
}

#[test]
fn job_against_an_unreachable_server_fails_with_its_kind() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(config("127.0.0.1", unused_port())).unwrap();
    let recorder = Arc::new(Recorder::default());
    let job = engine.start(request(dir.path()), recorder.clone());
    let summary = block_on(job.wait());
    assert!(job.is_finished());
    assert_eq!(summary.outcome, Outcome::Failed);
    assert_eq!(summary.error_kind, Some(ErrorKind::Connect));

    let events = recorder.events();
    assert!(matches!(events.last(), Some(JobEvent::Finished { .. })));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, JobEvent::Finished { .. }))
            .count(),
        1
    );
    // Controls after the end are harmless.
    job.pause();
    job.resume();
    job.stop();
}

#[test]
fn free_space_hint_reaches_the_free_space_check() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(config("127.0.0.1", unused_port())).unwrap();
    let job = engine.start(
        JobRequest {
            preflight: Preflight::Never,
            free_space_hint: Some(10),
            ..request(dir.path())
        },
        Arc::new(Recorder::default()),
    );
    let summary = block_on(job.wait());
    assert_eq!(summary.outcome, Outcome::Failed);
    assert_eq!(summary.error_kind, Some(ErrorKind::DiskFull));
}

#[test]
fn shutdown_stops_running_jobs() {
    let dir = tempfile::tempdir().unwrap();
    // A listener that accepts and never answers keeps the job connecting.
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    let engine = Engine::new(config("127.0.0.1", port)).unwrap();
    let job = engine.start(
        JobRequest {
            preflight: Preflight::Never,
            ..request(dir.path())
        },
        Arc::new(Recorder::default()),
    );
    let started = Instant::now();
    block_on(engine.shutdown());
    assert!(job.is_finished());
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(block_on(job.wait()).outcome, Outcome::Stopped);
    drop(silent);
}

/// Dropping the engine (the app quitting without `shutdown`) stops its jobs
/// rather than abandoning them: each listener still hears `Finished`.
#[test]
fn dropping_the_engine_stops_its_jobs() {
    let dir = tempfile::tempdir().unwrap();
    // A listener that accepts and never answers keeps the job connecting.
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    let engine = Engine::new(config("127.0.0.1", port)).unwrap();
    let recorder = Arc::new(Recorder::default());
    let job = engine.start(
        JobRequest {
            preflight: Preflight::Never,
            ..request(dir.path())
        },
        recorder.clone(),
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(!job.is_finished());

    let started = Instant::now();
    drop(engine);
    assert!(started.elapsed() < Duration::from_secs(10));
    let finished: Vec<JobSummary> = recorder
        .events()
        .into_iter()
        .filter_map(|e| match e {
            JobEvent::Finished { summary } => Some(summary),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), 1, "{:?}", recorder.events());
    assert_eq!(finished[0].outcome, Outcome::Stopped);
    assert!(job.is_finished());
    drop(silent);
}

/// The engine keeps no hold on a finished job (its listener included).
#[test]
fn finished_jobs_leave_the_engine() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(config("127.0.0.1", unused_port())).unwrap();
    let recorder = Arc::new(Recorder::default());
    let job = engine.start(request(dir.path()), recorder.clone());
    block_on(job.wait());
    assert!(engine.core.stop_all().is_empty());
}

#[test]
fn cli_config_import_reads_the_cli_file() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.toml");
    assert_eq!(
        cli_config_import(missing.to_string_lossy().into_owned()).unwrap(),
        None
    );

    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[usenet]
server = "news.example.com"
port = 563
username = "zeph"
password = "p\"w#d"
ssl = true
verify_ssl_certs = false
connections = 40
timeout = 30
retry_attempts = 3
retry_delay = 500

[download]
dir = "/Volumes/Media/Usenet"
create_subfolders = true

[post_processing]
auto_par2_repair = true
auto_extract_rar = false
delete_rar_after_extract = true
delete_par2_after_repair = true
deobfuscate_file_names = false
"#,
    )
    .unwrap();
    let source = path.to_string_lossy().into_owned();
    let imported = cli_config_import(source.clone()).unwrap().unwrap();
    assert_eq!(imported.server.host, "news.example.com");
    assert_eq!(imported.server.password, "p\"w#d");
    assert_eq!(imported.server.connections, 40);
    assert_eq!(imported.server.retry_attempts, 3);
    assert!(!imported.server.verify_certificate);
    assert!(!imported.auto_extract_rar);
    assert!(imported.delete_par2_after_repair);

    std::fs::write(&path, "[usenet]\nserver = \"\"\n").unwrap();
    let err = cli_config_import(source).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Config);
}
