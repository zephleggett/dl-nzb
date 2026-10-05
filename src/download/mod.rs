//! Download orchestration and NZB file handling
//!
//! This module provides the core download functionality including NZB parsing,
//! segment downloading, and file assembly.

pub(crate) mod downloader;
mod nzb;

pub use downloader::{AvailabilityReport, DownloadOutcome, DownloadResult, Downloader};
pub use nzb::{split_password, Nzb, NzbMeta};
