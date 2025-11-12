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
pub mod progress;
pub mod json_output;

// Feature modules organized by functionality
pub mod download;
pub mod nntp;
pub mod processing;

// Re-export commonly used types
pub use config::Config;
pub use download::{DownloadResult, Downloader, Nzb};
pub use error::{DlNzbError, Result};
pub use nntp::{NntpPool, NntpPoolBuilder, NntpPoolExt};
pub use processing::PostProcessor;

// Re-export serde_json for binary
pub use serde_json;
