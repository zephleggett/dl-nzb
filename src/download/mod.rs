//! Download orchestration and NZB file handling
//!
//! This module provides the core download functionality including NZB parsing,
//! segment downloading, and file assembly.

mod downloader;
mod nzb;

pub use downloader::{AvailabilityReport, DownloadOutcome, DownloadResult, Downloader};
pub use nzb::Nzb;
