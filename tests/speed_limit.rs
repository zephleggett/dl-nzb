//! The engine-wide speed limit against the mock NNTP server: a limit holds
//! within 20%, changes apply to a running job, one limit covers every job on
//! the engine (and a paused job takes none of it), and no limit costs nothing.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use dl_nzb::config::Config;
use dl_nzb::download::{Downloader, Nzb};
use dl_nzb::engine::{
    Engine, JobEvent, JobObserver, JobPhase, JobRequest, JobSummary, Outcome, Preflight,
};

const MIB: u64 = 1024 * 1024;
const ARTICLE_BYTES: usize = 256 * 1024;

/// A mock server holding one file per NZB.
struct Fixture {
    temp: tempfile::TempDir,
    port: u16,
    nzbs: Vec<PathBuf>,
}

impl Fixture {
    /// One NZB per entry of `sizes_mib`, each a single file of that many MiB.
    async fn new(sizes_mib: &[u64]) -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let mut articles = Vec::new();
        let mut nzbs = Vec::new();
        for (i, &mib) in sizes_mib.iter().enumerate() {
            let name = format!("file{i}.bin");
            let count = (mib * MIB) as usize / ARTICLE_BYTES;
            let (arts, ids, sizes, _) =
                make_file_articles(&name, &format!("f{i}p"), count, ARTICLE_BYTES);
            articles.extend(arts);
            let path = temp.path().join(format!("file{i}.nzb"));
            std::fs::write(&path, build_synthetic_nzb(&name, &ids, &sizes)).unwrap();
            nzbs.push(path);
        }
        let state = Arc::new(MockServerState {
            articles,
            ..Default::default()
        });
        let port = spawn_server(state).await;
        Fixture { temp, port, nzbs }
    }

    fn config(&self, connections: u16) -> Config {
        let mut config = make_config("127.0.0.1", self.port, self.temp.path().join("out"));
        config.usenet.connections = connections;
        config.tuning.max_concurrent_connections = connections as usize;
        config
    }

    fn request(&self, nzb: usize, run: &str) -> JobRequest {
        let out = self.temp.path().join(format!("job{nzb}-{run}"));
        JobRequest {
            preflight: Preflight::Never,
            ..JobRequest::new(&self.nzbs[nzb], out)
        }
    }

    fn nzb(&self, nzb: usize) -> &Path {
        &self.nzbs[nzb]
    }
}

/// Keeps the latest download progress (article bytes settled).
#[derive(Default)]
struct Progress {
    bytes_done: AtomicU64,
}

impl JobObserver for Progress {
    fn on_event(&self, event: JobEvent) {
        if let JobEvent::Progress(p) = event {
            if p.phase == JobPhase::Downloading {
                self.bytes_done.store(p.bytes_done, Ordering::Relaxed);
            }
        }
    }
}

async fn finish(job: &dl_nzb::JobHandle) -> JobSummary {
    let summary = wait(job, 30).await;
    assert_eq!(summary.outcome, Outcome::Completed, "{:?}", summary.message);
    summary
}

fn assert_near(actual: f64, expected: f64, tolerance: f64, what: &str) {
    let error = (actual - expected).abs() / expected;
    eprintln!(
        "{what}: {actual:.2} vs {expected:.2} ({:+.1}%)",
        (actual / expected - 1.0) * 100.0
    );
    assert!(
        error <= tolerance,
        "{what}: {actual:.0} vs {expected:.0} (off by {:.0}%)",
        error * 100.0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_limit_caps_the_download_speed() {
    let fx = Fixture::new(&[12]).await;
    let mut config = fx.config(6);
    config.download.speed_limit = Some(4 * MIB);
    let engine = Engine::new(config).unwrap();
    assert_eq!(engine.speed_limit(), Some(4 * MIB));

    let summary = finish(&engine.start(fx.request(0, "a"), Arc::new(Progress::default()))).await;
    let rate = summary.wire_bytes as f64 / summary.download_secs;
    assert_near(rate, (4 * MIB) as f64, 0.2, "bytes per second");
    // ≈ 12 MiB at 4 MiB/s.
    assert_near(summary.download_secs, 12.0 / 4.0, 0.2, "seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raising_the_limit_mid_transfer_finishes_quickly() {
    let fx = Fixture::new(&[16]).await;
    let engine = Engine::new(fx.config(6)).unwrap();
    engine.set_speed_limit(Some(MIB));
    assert_eq!(engine.config().download.speed_limit, Some(MIB));

    let progress = Arc::new(Progress::default());
    let job = engine.start(fx.request(0, "a"), progress.clone());
    tokio::time::sleep(Duration::from_secs(1)).await;
    // About 1 MiB so far (plus the burst), of 16.
    let done = progress.bytes_done.load(Ordering::Relaxed);
    assert!(done < 3 * MIB, "{done} bytes done after 1 s at 1 MiB/s");
    assert!(!job.is_finished());

    engine.set_speed_limit(None);
    assert_eq!(engine.speed_limit(), None);
    let raised = Instant::now();
    finish(&job).await;
    let rest = raised.elapsed();
    eprintln!(
        "{done} bytes done at 1 MiB/s after 1 s; finished {rest:?} after the limit was removed"
    );
    assert!(
        rest < Duration::from_millis(2000),
        "took {rest:?} once unlimited"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_limit_covers_concurrent_jobs() {
    let fx = Fixture::new(&[6, 6]).await;
    let engine = Engine::new(fx.config(8)).unwrap();
    engine.set_speed_limit(Some(4 * MIB));

    let started = Instant::now();
    let a = engine.start(fx.request(0, "a"), Arc::new(Progress::default()));
    let b = engine.start(fx.request(1, "b"), Arc::new(Progress::default()));
    let (a, b) = (finish(&a).await, finish(&b).await);
    let elapsed = started.elapsed().as_secs_f64();

    let total = (a.wire_bytes + b.wire_bytes) as f64;
    assert_near(
        total / elapsed,
        (4 * MIB) as f64,
        0.2,
        "bytes per second, both jobs",
    );
    // Each job alone would have finished at twice the speed.
    assert!(a.download_secs > 1.0 && b.download_secs > 1.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paused_job_leaves_the_whole_limit_to_the_others() {
    let fx = Fixture::new(&[8, 8]).await;
    let engine = Engine::new(fx.config(6)).unwrap();
    engine.set_speed_limit(Some(4 * MIB));

    let paused_progress = Arc::new(Progress::default());
    let paused = engine.start(fx.request(0, "a"), paused_progress.clone());
    paused.pause();
    let running = engine.start(fx.request(1, "b"), Arc::new(Progress::default()));
    let summary = finish(&running).await;
    let rate = summary.wire_bytes as f64 / summary.download_secs;
    assert_near(
        rate,
        (4 * MIB) as f64,
        0.2,
        "bytes per second while the other job is paused",
    );
    assert_eq!(paused_progress.bytes_done.load(Ordering::Relaxed), 0);

    // The paused job still completes once resumed, at the limit.
    paused.resume();
    let summary = finish(&paused).await;
    let rate = summary.wire_bytes as f64 / summary.download_secs;
    assert!(
        rate < 1.2 * (4 * MIB) as f64,
        "{rate:.0} bytes per second after resuming"
    );
}

/// Sanity, not a benchmark: with no limit set, the engine's connections (which
/// carry the limiter) move data as fast as connections without one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_limit_costs_nothing_measurable() {
    let fx = Fixture::new(&[16]).await;
    let engine = Engine::new(fx.config(6)).unwrap();
    assert_eq!(engine.speed_limit(), None);
    let nzb = Nzb::from_file(fx.nzb(0)).unwrap();

    let mut with_limiter = f64::MAX;
    let mut without = f64::MAX;
    for run in 0..3 {
        let summary = finish(&engine.start(
            fx.request(0, &run.to_string()),
            Arc::new(Progress::default()),
        ))
        .await;
        with_limiter = with_limiter.min(summary.download_secs);

        let mut config = fx.config(6);
        config.download.dir = fx.temp.path().join(format!("plain-{run}"));
        std::fs::create_dir_all(&config.download.dir).unwrap();
        let downloader = Downloader::new(config.clone()).await.unwrap();
        let outcome = downloader.download_nzb(&nzb, config, None).await.unwrap();
        without = without.min(outcome.transfer_duration.as_secs_f64());
    }
    eprintln!("16 MiB: engine (no limit) {with_limiter:.3} s, no limiter {without:.3} s");
    assert!(
        with_limiter <= without * 1.5 + 0.05,
        "engine (no limit) {with_limiter:.3} s vs no limiter {without:.3} s"
    );
}
