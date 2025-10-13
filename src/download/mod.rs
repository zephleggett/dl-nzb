//! Download orchestration and NZB file handling
//!
//! This module provides the core download functionality including NZB parsing,
//! segment downloading, and file assembly.

mod downloader;
mod nzb;

pub use downloader::{Downloader, DownloadResult};
pub use nzb::Nzb;
