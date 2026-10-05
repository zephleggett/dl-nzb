//! The dl-nzb engine for Swift, through UniFFI (`apple/CONTRACT.md` §2).
//!
//! Mirrors `dl_nzb::engine` one-to-one: an [`Engine`] object, a [`JobHandle`]
//! object per job, a [`JobListener`] the app implements, and the engine's own
//! records and enums for the data ([`types`]). Swift sees them Swift-cased
//! (`Engine(config:)`, `start(request:listener:)`, `onEvent(event:)`).
//!
//! The engine runs on a multi-thread Tokio runtime this crate owns (the app
//! has none), so every call works from any Swift thread. Async methods run
//! their work on that runtime and only await it from Swift's executor.
//! Cancelling a Swift task does not reach Rust: jobs are stopped with
//! [`JobHandle::stop`].

uniffi::setup_scaffolding!();

mod error;
mod types;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dl_nzb::engine as core;
use tokio::runtime::{Handle, Runtime};

pub use error::EngineError;
pub use types::*;

/// Receives a job's events, in order, one at a time. Implemented in Swift.
///
/// Called on engine threads, sometimes while the engine holds the job's event
/// lock: hand the event on (the app yields it into an `AsyncStream`) and
/// return. Never call back into the engine from here.
#[uniffi::export(foreign)]
pub trait JobListener: Send + Sync {
    fn on_event(&self, event: JobEvent);
}

/// Adapts a Swift listener to the engine's observer, and takes the job off
/// its engine's [`LiveJobs`] once it has finished.
struct Forward {
    listener: Arc<dyn JobListener>,
    live: Weak<LiveJobs>,
    id: u64,
}

impl core::JobObserver for Forward {
    fn on_event(&self, event: core::JobEvent) {
        let finished = matches!(event, core::JobEvent::Finished(_));
        self.listener.on_event(event.into());
        if finished {
            if let Some(live) = self.live.upgrade() {
                live.finished(self.id);
            }
        }
    }
}

/// The jobs an engine started that have not finished, so dropping the
/// engine can stop them. A job leaves as soon as its `Finished` is delivered.
#[derive(Default)]
struct LiveJobs {
    state: Mutex<LiveState>,
}

#[derive(Default)]
struct LiveState {
    next_id: u64,
    running: HashMap<u64, core::JobHandle>,
    /// Jobs that finished before `add` could list them.
    finished_early: HashSet<u64>,
}

impl LiveJobs {
    fn lock(&self) -> std::sync::MutexGuard<'_, LiveState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A listener for a job about to start: `listener`, plus the id `add`
    /// then lists the job under.
    fn forward(self: &Arc<Self>, listener: Arc<dyn JobListener>) -> (Arc<Forward>, u64) {
        let id = {
            let mut state = self.lock();
            state.next_id += 1;
            state.next_id
        };
        let forward = Forward {
            listener,
            live: Arc::downgrade(self),
            id,
        };
        (Arc::new(forward), id)
    }

    fn add(&self, id: u64, job: core::JobHandle) {
        let mut state = self.lock();
        if !state.finished_early.remove(&id) {
            state.running.insert(id, job);
        }
    }

    fn finished(&self, id: u64) {
        let mut state = self.lock();
        if state.running.remove(&id).is_none() {
            state.finished_early.insert(id);
        }
    }

    fn take_all(&self) -> Vec<core::JobHandle> {
        self.lock().running.drain().map(|(_, job)| job).collect()
    }
}

/// How long dropping the engine waits for its stopped jobs to wind down.
const DROP_GRACE: Duration = Duration::from_secs(5);

/// The download engine. One per app; owns its runtime, its configuration and
/// one connection pool.
#[derive(uniffi::Object)]
pub struct Engine {
    core: core::Engine,
    handle: Handle,
    /// Kept to be shut down without blocking on drop (dropping a `Runtime`
    /// blocks, and panics on one of its own threads).
    runtime: Option<Runtime>,
    live: Arc<LiveJobs>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Stop every job first. Shutting the runtime down only drops its
        // tasks: PAR2 and extraction running on blocking threads would carry
        // on, and listeners would never hear `Finished`. A stop reaches those
        // threads through the job's cancel flag.
        let jobs = self.live.take_all();
        for job in &jobs {
            job.stop();
        }
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        // Give them a moment to end, so listeners get `Finished(Stopped)`
        // and resume data is saved. Not on a runtime thread, where blocking
        // is not allowed (the jobs stay stopped regardless).
        if !jobs.is_empty() && Handle::try_current().is_err() {
            runtime.block_on(async {
                let all = async {
                    for job in &jobs {
                        job.wait().await;
                    }
                };
                let _ = tokio::time::timeout(DROP_GRACE, all).await;
            });
        }
        runtime.shutdown_background();
    }
}

#[uniffi::export]
impl Engine {
    /// Validates `config` (no network). Server credentials are only needed
    /// when a job starts, so an engine can exist before the user enters them.
    #[uniffi::constructor]
    pub fn new(config: EngineConfig) -> Result<Arc<Self>, EngineError> {
        let core = core::Engine::new(config.to_core())?;
        let runtime = core::runtime()?;
        Ok(Arc::new(Self {
            core,
            handle: runtime.handle().clone(),
            runtime: Some(runtime),
            live: Arc::default(),
        }))
    }

    /// New settings for jobs started from now on, speed limit included.
    /// Running jobs keep the server they started with.
    pub fn update_config(&self, config: EngineConfig) -> Result<(), EngineError> {
        let config = config.to_core();
        config.validate()?;
        self.core.update_config(config);
        Ok(())
    }

    /// Engine-wide and live; `None` (or 0) removes the limit.
    pub fn set_speed_limit(&self, bytes_per_second: Option<u64>) {
        self.core.set_speed_limit(bytes_per_second);
    }

    /// What `set_speed_limit` (or the configuration) last set.
    pub fn speed_limit(&self) -> Option<u64> {
        self.core.speed_limit()
    }

    /// Connect and log in once, with the real reason on failure.
    pub async fn test_connection(&self, server: ServerConfig) -> Result<ServerCheck, EngineError> {
        let usenet = server.to_core();
        Ok(self
            .handle
            .spawn(async move { core::Engine::test_connection(&usenet).await })
            .await
            .map_err(EngineError::internal)??)
    }

    /// Parse an NZB and describe it. No network; blocking file I/O.
    pub fn inspect(&self, nzb_path: String) -> Result<NzbInfo, EngineError> {
        Ok(core::Engine::inspect(Path::new(&nzb_path))?)
    }

    /// Start a job. `listener` gets every event; `Finished` exactly once, last.
    pub fn start(&self, request: JobRequest, listener: Arc<dyn JobListener>) -> Arc<JobHandle> {
        let _entered = self.handle.enter();
        let (forward, id) = self.live.forward(listener);
        let job = self.core.start(request, forward);
        self.live.add(id, job.clone());
        Arc::new(JobHandle { core: job })
    }

    /// Post-processing only (PAR2, extraction, renaming) on a job folder,
    /// e.g. once the user has supplied an archive's password.
    pub fn reprocess(
        &self,
        output_dir: String,
        passwords: Vec<String>,
        listener: Arc<dyn JobListener>,
    ) -> Arc<JobHandle> {
        let _entered = self.handle.enter();
        let (forward, id) = self.live.forward(listener);
        let job = self.core.reprocess(output_dir.into(), passwords, forward);
        self.live.add(id, job.clone());
        Arc::new(JobHandle { core: job })
    }

    /// Stop every job so it can resume, wait for them, close the connections.
    pub async fn shutdown(&self) {
        let engine = self.core.clone();
        if let Err(e) = self
            .handle
            .spawn(async move { engine.shutdown().await })
            .await
        {
            tracing::error!("engine shutdown failed: {e}");
        }
    }
}

/// Controls one job. Every method returns at once.
#[derive(uniffi::Object)]
pub struct JobHandle {
    core: core::JobHandle,
}

#[uniffi::export]
impl JobHandle {
    /// Download phases only: within about a second the requests in flight are
    /// abandoned (no retry spent) and every connection is closed; `resume`
    /// reconnects. A pause asked for earlier applies once downloading starts.
    pub fn pause(&self) {
        self.core.pause();
    }

    pub fn resume(&self) {
        self.core.resume();
    }

    /// Ends the job promptly; its data stays so `start` can continue it.
    pub fn stop(&self) {
        self.core.stop();
    }

    pub fn is_finished(&self) -> bool {
        self.core.is_finished()
    }

    /// The job's summary once it has finished (after the listener got
    /// `Finished`). Runtime-independent, so it can be awaited from Swift.
    pub async fn wait(&self) -> JobSummary {
        self.core.wait().await
    }
}

/// Where the CLI keeps its `config.toml`, in the user's real home (a
/// sandboxed app's `HOME` is its container): where an open panel should start.
#[uniffi::export]
pub fn cli_config_path() -> Option<String> {
    core::cli_config_path().map(|p| p.to_string_lossy().into_owned())
}

/// Read the CLI's settings from `path` (the default location, or a file the
/// user picked). `None` when there is no file there; an error when it cannot
/// be read or parsed, or names no server (nothing worth importing).
#[uniffi::export]
pub fn cli_config_import(path: String) -> Result<Option<ImportedConfig>, EngineError> {
    let file = Path::new(&path);
    if !file.exists() {
        return Ok(None);
    }
    let config = core::cli_config_import_from(file)?;
    if config.usenet.server.trim().is_empty() {
        return Err(EngineError::Config(
            "The dl-nzb settings file does not name a server.".to_string(),
        ));
    }
    let download_dir = Some(&config.download.dir)
        .filter(|d| d.is_absolute())
        .map(|d| d.to_string_lossy().into_owned());
    Ok(Some(ImportedConfig {
        config: EngineConfig::from_core(&config),
        download_dir,
        source: path,
    }))
}

#[cfg(test)]
mod tests;
