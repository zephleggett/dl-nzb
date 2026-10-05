//! dl-nzb - High-performance NZB downloader library
//!
//! This library provides a robust, async implementation for downloading NZB files from Usenet.
//!
//! # Features
//!
//! - Async/await support via Tokio
//! - Connection pooling with automatic health checks
//! - Optimized yEnc decoding
//! - Per-job progress events, pause/resume and prompt stop
//! - PAR2 verification and repair
//! - RAR extraction
//!
//! The entry point is [`engine::Engine`]; the `dl-nzb` CLI is one observer of it.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use dl_nzb::engine::{Engine, JobEvent, JobObserver, JobRequest};
//!
//! struct Print;
//! impl JobObserver for Print {
//!     fn on_event(&self, event: JobEvent) {
//!         if let JobEvent::Finished(summary) = event {
//!             println!("{:?}: {:?}", summary.outcome, summary.message);
//!         }
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let engine = Engine::new(dl_nzb::Config::load()?)?;
//!     let job = engine.start(
//!         JobRequest::new("release.nzb", "/tmp/downloads/release"),
//!         Arc::new(Print),
//!     );
//!     let summary = job.wait().await;
//!     println!("{} bytes", summary.data_bytes);
//!     Ok(())
//! }
//! ```

// Core modules
pub mod config;
pub mod engine;
pub mod error;
pub mod patterns;
pub mod util;

// Terminal front end (the `cli` feature): argument parsing, progress bars,
// styling and the JSON output shapes. Never used by the engine itself.
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "cli")]
pub mod json_output;
#[cfg(feature = "cli")]
pub mod progress;
#[cfg(feature = "cli")]
pub mod ui;

// Feature modules organized by functionality
pub mod download;
pub mod nntp;
pub mod processing;

// Re-export commonly used types
pub use config::Config;
pub use download::{DownloadOutcome, DownloadResult, Downloader, Nzb};
pub use engine::{Engine, JobEvent, JobHandle, JobObserver, JobRequest, JobSummary};
pub use error::{DlNzbError, ErrorKind, Result};
pub use nntp::{NntpPool, NntpPoolBuilder, NntpPoolExt};
pub use processing::PostProcessor;

// Re-export serde_json for binary
pub use serde_json;

/// Output suppression for the terminal front end: when `set_quiet(true)` is
/// called, the CLI's decorative human-readable prints are skipped so the JSON
/// consumer's output stays clean. (The engine never prints.)
#[cfg(feature = "cli")]
pub mod output_mode {
    use std::sync::atomic::{AtomicBool, Ordering};

    static QUIET: AtomicBool = AtomicBool::new(false);

    pub fn set_quiet(q: bool) {
        QUIET.store(q, Ordering::Release);
    }

    pub fn is_quiet() -> bool {
        QUIET.load(Ordering::Acquire)
    }
}
