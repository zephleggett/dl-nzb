//! The engine: one API for downloading NZBs, used by the CLI and the Apple apps
//! (`apple/CONTRACT.md` §1).
//!
//! An [`Engine`] owns the configuration and one NNTP connection pool. Each
//! [`start`](Engine::start) runs a job (parse, free-space check, connect,
//! optional availability scan, download, PAR2, extraction, renaming) on the
//! current Tokio runtime and reports to a [`JobObserver`]; the returned
//! [`JobHandle`] pauses, resumes, stops and awaits it. Jobs share nothing
//! mutable but the pool and the speed limit ([`Engine::set_speed_limit`], one
//! budget for all connections): stopping one never affects another, and the engine
//! never touches the terminal, stdin or process-wide state (beyond raising the
//! open-file limit once).

pub(crate) mod context;
mod disk;
mod inspect;
mod job;
pub(crate) mod sidecar;
mod types;

pub use types::*;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::config::{Config, UsenetConfig};
use crate::error::{ConfigError, DlNzbError, NntpError, Result};
use crate::nntp::{AsyncNntpConnection, NntpPool, NntpPoolBuilder, SpeedLimiter};
use context::JobCtx;

/// Close idle pool connections once nothing has used them for this long.
const POOL_IDLE_CLOSE: Duration = Duration::from_secs(60);
const POOL_IDLE_CHECK: Duration = Duration::from_secs(15);

/// The download engine. Cheap to clone (an `Arc` inside); clones share the
/// configuration, pool and jobs.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<EngineInner>,
}

pub(crate) struct EngineInner {
    config: RwLock<Config>,
    pool: Mutex<Option<PoolSlot>>,
    pool_generation: AtomicU64,
    /// The engine-wide speed limit, shared by every connection of every pool.
    limiter: Arc<SpeedLimiter>,
    /// Jobs started and not finished, for `shutdown` (each removes itself
    /// when it finishes).
    jobs: Mutex<Vec<JobHandle>>,
}

struct PoolSlot {
    key: PoolKey,
    pool: NntpPool,
    generation: u64,
}

/// Everything a pool's connections depend on. A job whose configuration gives
/// a different key gets a fresh pool; jobs already running keep theirs.
#[derive(Clone, PartialEq, Eq)]
struct PoolKey {
    server: String,
    port: u16,
    username: String,
    password: String,
    ssl: bool,
    verify_ssl_certs: bool,
    connections: u16,
    max_concurrent_connections: usize,
}

impl PoolKey {
    fn of(config: &Config) -> Self {
        let u = &config.usenet;
        Self {
            server: u.server.clone(),
            port: u.port,
            username: u.username.clone(),
            password: u.password.clone(),
            ssl: u.ssl,
            verify_ssl_certs: u.verify_ssl_certs,
            connections: u.connections,
            max_concurrent_connections: config.tuning.max_concurrent_connections,
        }
    }
}

impl Engine {
    /// Create an engine. Validates the configuration's shape (server
    /// credentials are only required when a job starts, so an engine can exist
    /// before the user has entered them). No network.
    pub fn new(config: Config) -> Result<Engine> {
        config.validate()?;
        raise_fd_limit();
        let limiter = Arc::new(SpeedLimiter::new(config.download.speed_limit));
        Ok(Engine {
            inner: Arc::new(EngineInner {
                config: RwLock::new(config),
                pool: Mutex::new(None),
                pool_generation: AtomicU64::new(0),
                limiter,
                jobs: Mutex::new(Vec::new()),
            }),
        })
    }

    /// Replace the configuration for jobs started from now on. A changed server
    /// or connection setting gets a new pool at the next start; running jobs
    /// finish on the pool they started with. The configuration's
    /// `download.speed_limit` applies at once, to running jobs too (as
    /// [`set_speed_limit`](Self::set_speed_limit)).
    pub fn update_config(&self, config: Config) {
        let limit = config.download.speed_limit;
        if let Ok(mut c) = self.inner.config.write() {
            *c = config;
        }
        self.inner.limiter.set(limit);
    }

    /// The current configuration.
    pub fn config(&self) -> Config {
        self.inner.config()
    }

    /// Limit the engine's total download speed, in bytes per second, across
    /// every connection and job; `None` (or 0) removes the limit. Takes effect
    /// immediately for running jobs: a lower limit within a read or two, a
    /// raised or removed one within 50 ms. Paused jobs read nothing, so they
    /// use none of it. Also recorded as the configuration's
    /// `download.speed_limit`.
    pub fn set_speed_limit(&self, bytes_per_sec: Option<u64>) {
        let limit = bytes_per_sec.filter(|&n| n > 0);
        if let Ok(mut c) = self.inner.config.write() {
            c.download.speed_limit = limit;
        }
        self.inner.limiter.set(limit);
    }

    /// The limit in force (from the configuration or
    /// [`set_speed_limit`](Self::set_speed_limit)); `None` = unlimited.
    pub fn speed_limit(&self) -> Option<u64> {
        self.inner.limiter.get()
    }

    /// Connect and log in to `server` once, reporting the real reason on
    /// failure ([`DlNzbError::kind`]: `Auth`, `Dns`, `Connect`, `Tls`,
    /// `Timeout`, `Protocol`, `Config`).
    pub async fn test_connection(server: &UsenetConfig) -> Result<ServerCheck> {
        if server.server.trim().is_empty() {
            return Err(ConfigError::NoServer.into());
        }
        let mut conn = AsyncNntpConnection::connect(server, None).await?;
        let greeting = conn.greeting().to_string();
        // Latency is one command round trip after login. DATE is also the
        // pool's health check, so a server that won't answer it would have
        // every pooled connection discarded: worth failing the test for.
        let started = Instant::now();
        let healthy = conn.is_healthy().await;
        let latency_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
        let _ = conn.close().await;
        if !healthy {
            return Err(NntpError::ProtocolError(
                "the server accepted the login but did not answer a DATE command".to_string(),
            )
            .into());
        }
        Ok(ServerCheck {
            greeting,
            tls: server.ssl,
            latency_ms,
        })
    }

    /// Parse an NZB and describe it. No network.
    pub fn inspect(nzb_path: &Path) -> Result<NzbInfo> {
        inspect::inspect(nzb_path)
    }

    /// Start a job on the current Tokio runtime. Its events go to `observer`;
    /// `Finished` is always delivered, exactly once, last.
    ///
    /// # Panics
    /// Outside a Tokio runtime.
    pub fn start(&self, request: JobRequest, observer: Arc<dyn JobObserver>) -> JobHandle {
        let output_dir = request.output_dir.clone();
        self.spawn_job(observer, output_dir, move |engine, ctx| {
            job::run(engine, ctx, request)
        })
    }

    /// Run post-processing only (PAR2, extraction, renaming) on a job folder,
    /// e.g. once the user has supplied the password an archive needed.
    ///
    /// # Panics
    /// Outside a Tokio runtime.
    pub fn reprocess(
        &self,
        output_dir: PathBuf,
        passwords: Vec<String>,
        observer: Arc<dyn JobObserver>,
    ) -> JobHandle {
        let dir = output_dir.clone();
        self.spawn_job(observer, output_dir, move |engine, ctx| {
            job::reprocess(engine, ctx, dir, passwords)
        })
    }

    /// Stop every job (resumably), wait for them to finish, and close the pool.
    pub async fn shutdown(&self) {
        let jobs: Vec<JobHandle> = self
            .inner
            .jobs
            .lock()
            .map(|mut j| j.drain(..).collect())
            .unwrap_or_default();
        for job in &jobs {
            job.stop();
        }
        for job in &jobs {
            job.wait().await;
        }
        self.inner.close_pool();
    }

    fn spawn_job<F, Fut>(
        &self,
        observer: Arc<dyn JobObserver>,
        output_dir: PathBuf,
        make: F,
    ) -> JobHandle
    where
        F: FnOnce(Arc<EngineInner>, Arc<JobCtx>) -> Fut,
        Fut: std::future::Future<Output = JobSummary> + Send + 'static,
    {
        let ctx = JobCtx::new(observer);
        let (done_tx, done_rx) = watch::channel(None);
        let handle = JobHandle {
            ctx: ctx.clone(),
            done: done_rx,
            output_dir: output_dir.clone(),
        };
        if let Ok(mut jobs) = self.inner.jobs.lock() {
            jobs.retain(|j| !j.is_finished());
            jobs.push(handle.clone());
        }
        let work = make(self.inner.clone(), ctx.clone());
        let engine = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let ticker = ctx.spawn_ticker();
            // A nested task turns a panic in the job into a Failed summary, so
            // the observer still gets its Finished (with `panic = "unwind"`).
            let summary = match tokio::spawn(work).await {
                Ok(summary) => summary,
                Err(e) => {
                    tracing::error!("job task failed: {e}");
                    let mut s = JobSummary::new(Outcome::Failed, output_dir);
                    s.message = Some("The job stopped because of an internal error.".to_string());
                    s
                }
            };
            drop(ticker);
            ctx.finish(summary.clone());
            if let Some(engine) = engine.upgrade() {
                engine.forget_job(&ctx);
            }
            let _ = done_tx.send(Some(summary));
        });
        handle
    }
}

impl EngineInner {
    pub(crate) fn config(&self) -> Config {
        self.config
            .read()
            .map(|c| c.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    /// The pool for `config`, building (and replacing) it when the server
    /// settings changed. Building opens no connections.
    pub(crate) fn pool_for(self: &Arc<Self>, config: &Config) -> Result<NntpPool> {
        let key = PoolKey::of(config);
        let mut slot = self.pool.lock().map_err(|_| {
            DlNzbError::from(ConfigError::Invalid {
                field: "pool".into(),
                reason: "engine state poisoned".into(),
            })
        })?;
        if let Some(existing) = slot.as_ref().filter(|s| s.key == key) {
            return Ok(existing.pool.clone());
        }
        let pool = NntpPoolBuilder::new(config.usenet.clone())
            .max_concurrent_connections(config.tuning.max_concurrent_connections)
            .speed_limiter(self.limiter.clone())
            .build()?;
        let generation = self.pool_generation.fetch_add(1, Ordering::Relaxed) + 1;
        *slot = Some(PoolSlot {
            key,
            pool: pool.clone(),
            generation,
        });
        drop(slot);
        spawn_idle_reaper(Arc::downgrade(self), generation);
        Ok(pool)
    }

    /// A job finished: `shutdown` has nothing left to stop or wait for.
    fn forget_job(&self, ctx: &Arc<JobCtx>) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.retain(|job| !Arc::ptr_eq(&job.ctx, ctx));
        }
    }

    fn close_pool(&self) {
        let slot = self.pool.lock().ok().and_then(|mut s| s.take());
        if let Some(slot) = slot {
            slot.pool.retain(|_, _| false);
        }
    }
}

/// Close a pool's idle connections once none has been checked out for
/// [`POOL_IDLE_CLOSE`] (a paused job's workers close theirs at once). The
/// pool itself stays usable: the next job just reconnects. Exits
/// when the engine is dropped or the pool is replaced.
fn spawn_idle_reaper(engine: Weak<EngineInner>, generation: u64) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(async move {
        let mut idle_since: Option<Instant> = None;
        loop {
            tokio::time::sleep(POOL_IDLE_CHECK).await;
            let Some(engine) = engine.upgrade() else {
                return;
            };
            let Ok(slot) = engine.pool.lock() else {
                return;
            };
            let Some(pool) = slot
                .as_ref()
                .filter(|s| s.generation == generation)
                .map(|s| &s.pool)
            else {
                return;
            };
            let status = pool.status();
            let in_use = status.size.saturating_sub(status.available);
            if status.size == 0 || in_use > 0 || status.waiting > 0 {
                idle_since = None;
                continue;
            }
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() + POOL_IDLE_CHECK >= POOL_IDLE_CLOSE {
                let closed = pool.retain(|_, _| false).removed.len();
                tracing::debug!("Closed {closed} idle connection(s)");
                idle_since = None;
            }
        }
    });
}

/// Controls one job. Cheap to clone; every clone controls the same job.
#[derive(Clone)]
pub struct JobHandle {
    ctx: Arc<JobCtx>,
    done: watch::Receiver<Option<JobSummary>>,
    output_dir: PathBuf,
}

impl JobHandle {
    /// Pause downloading, within about a second: the requests in flight are
    /// abandoned (their articles go back to the queue without spending a
    /// retry), the job's connections are closed, and nothing more is read.
    /// The resume sidecar is saved. [`resume`](Self::resume) reconnects. Has
    /// no effect on the other phases (a pause requested earlier applies once
    /// downloading starts).
    pub fn pause(&self) {
        self.ctx.set_paused(true);
    }

    pub fn resume(&self) {
        self.ctx.set_paused(false);
    }

    /// Stop promptly (no waiting out read timeouts). Downloaded data stays in
    /// the job folder; incomplete files keep their `.partial` name, and the
    /// resume sidecar is saved so `start()` on the folder continues the job.
    pub fn stop(&self) {
        self.ctx.cancel();
    }

    /// The job has finished: true from the moment its observer receives
    /// `Finished` (inside that call too).
    pub fn is_finished(&self) -> bool {
        self.ctx.is_finished() || self.done.borrow().is_some()
    }

    /// Wait for the job to finish. Returns after the observer received
    /// `Finished`.
    pub async fn wait(&self) -> JobSummary {
        let mut done = self.done.clone();
        let finished = done
            .wait_for(|s| s.is_some())
            .await
            .ok()
            .and_then(|s| s.clone());
        match finished {
            Some(summary) => summary,
            // The job task vanished without finishing (its runtime shut down).
            None => {
                let mut s = JobSummary::new(Outcome::Stopped, self.output_dir.clone());
                s.message = Some("The job was interrupted.".to_string());
                s.resumable = true;
                s
            }
        }
    }
}

/// A multi-thread Tokio runtime sized for the engine: the blocking pool is
/// bounded because per-article writes, finalize, PAR2 and RAR extraction all
/// use `spawn_blocking`, and the 512-thread default can balloon thread, stack
/// and fd use. 64 covers concurrent writes and finalizes plus one PAR2 and one
/// extraction.
pub fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(64)
        .thread_name("dl-nzb")
        .build()
}

/// Best-effort raise of the open-file soft limit toward the hard limit, once
/// per process. A download holds one fd per output file (large NZBs have
/// hundreds) plus one per connection, and PAR2 opens one per recovery file;
/// the inherited limit is often only 256 on macOS.
fn raise_fd_limit() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[cfg(unix)]
        match rlimit::increase_nofile_limit(u64::MAX) {
            Ok(limit) => tracing::debug!("Open-file limit raised to {}", limit),
            Err(e) => tracing::debug!("Could not raise open-file limit: {}", e),
        }
    });
}

/// Read the dl-nzb CLI's configuration file, if there is one, for a
/// first-launch import. Includes the password. Never creates the file and
/// ignores `DL_NZB_*` environment overrides (those belong to CLI runs).
pub fn cli_config_import() -> Option<(Config, PathBuf)> {
    let path = cli_config_path()?;
    if !path.is_file() {
        return None;
    }
    let config = cli_config_import_from(&path).ok()?;
    Some((config, path))
}

/// Where the CLI keeps its configuration. On macOS this is resolved from the
/// user's real home directory, so a sandboxed app (whose `HOME` points into
/// its container) can point an open panel at it.
pub fn cli_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    if let Some(home) = real_home_dir() {
        return Some(
            home.join("Library")
                .join("Application Support")
                .join("dl-nzb")
                .join("config.toml"),
        );
    }
    Config::config_path().ok()
}

/// Parse a CLI configuration file at `path` (e.g. one the user picked in an
/// open panel, which a sandboxed app can read but not find by itself).
pub fn cli_config_import_from(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)?;
    Config::from_toml_str(&text)
}

/// The login user's home directory from the password database, unaffected by
/// a sandbox's `HOME`.
#[cfg(target_os = "macos")]
fn real_home_dir() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    let mut buf = vec![0u8; 4096];
    // SAFETY: `passwd` is plain old data, so all-zero is a valid value.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and `buf` outlives the use
    // of the strings `getpwuid_r` stores into it.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: on success `pw_dir` points to a NUL-terminated string in `buf`.
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finished job leaves the engine's list of running jobs at once, not
    /// when the next job starts.
    #[tokio::test]
    async fn a_finished_job_leaves_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(Config::default()).unwrap();
        let job = engine.reprocess(dir.path().to_path_buf(), Vec::new(), Arc::new(NullObserver));
        job.wait().await;
        assert!(job.is_finished());
        assert!(engine.inner.jobs.lock().unwrap().is_empty());
    }
}
