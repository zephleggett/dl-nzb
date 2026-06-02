//! dl-nzb - High-performance NZB downloader library
//!
//! This library provides a robust, async implementation for downloading NZB files from Usenet.
//!
//! # Features
//!
//! - Async/await support via Tokio
//! - Connection pooling with automatic health checks
//! - Optimized yEnc decoding
//! - Progress reporting
//! - PAR2 verification and repair
//! - RAR extraction
//!
//! # Example
//!
//! ```no_run
//! use dl_nzb::{config::Config, nntp::NntpPoolBuilder};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = Config::load()?;
//!     let pool = NntpPoolBuilder::new(config.usenet.clone()).build()?;
//!     // Use the pool for downloading...
//!     Ok(())
//! }
//! ```

// Core modules
pub mod cli;
pub mod config;
pub mod error;
pub mod json_output;
pub mod patterns;
pub mod progress;

// Feature modules organized by functionality
pub mod download;
pub mod nntp;
pub mod processing;

// Re-export commonly used types
pub use config::Config;
pub use download::{DownloadOutcome, DownloadResult, Downloader, Nzb};
pub use error::{DlNzbError, Result};
pub use nntp::{NntpPool, NntpPoolBuilder, NntpPoolExt};
pub use processing::PostProcessor;

// Re-export serde_json for binary
pub use serde_json;

/// Shutdown coordination for graceful Ctrl+C handling.
///
/// Backed by a shared `Arc<AtomicBool>` so synchronous, CPU-bound work running
/// on blocking threads (PAR2 repair via par2-rs, RAR extraction via unrar) can
/// poll the same flag the async download workers observe.
pub mod shutdown {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};

    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

    fn flag() -> &'static Arc<AtomicBool> {
        FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)))
    }

    /// Signal that a graceful shutdown has been requested.
    pub fn request() {
        flag().store(true, Ordering::Release);
    }

    /// Check whether a graceful shutdown has been requested.
    pub fn is_requested() -> bool {
        flag().load(Ordering::Acquire)
    }

    /// A clonable handle to the shutdown flag, for blocking work that needs to
    /// poll cancellation itself (e.g. handed to par2-rs / unrar loops).
    pub fn handle() -> Arc<AtomicBool> {
        flag().clone()
    }
}

/// Output suppression: when `set_quiet(true)` is called, the download and
/// post-processing modules skip their decorative human-readable prints so the
/// JSON consumer's stdout stays clean.
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
