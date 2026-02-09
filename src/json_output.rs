use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// JSON output for list mode
#[derive(Debug, Serialize, Deserialize)]
pub struct NzbInfo {
    pub file: PathBuf,
    pub total_files: usize,
    pub total_size: u64,
    pub total_segments: usize,
    pub files: Vec<FileInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileInfo {
    pub filename: String,
    pub size: u64,
    pub segments: usize,
    pub is_par2: bool,
}

/// JSON output for download results
#[derive(Debug, Serialize, Deserialize)]
pub struct DownloadSummary {
    pub nzb: PathBuf,
    pub output_dir: PathBuf,
    pub success: bool,
    pub total_size: u64,
    pub download_time_seconds: f64,
    pub average_speed_mbps: f64,
    pub files: Vec<DownloadFileResult>,
    pub post_processing: PostProcessingResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DownloadFileResult {
    pub filename: String,
    pub path: PathBuf,
    pub size: u64,
    pub segments_downloaded: usize,
    pub segments_failed: usize,
    pub success: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PostProcessingResult {
    pub par2_verified: bool,
    pub par2_repaired: bool,
    pub rar_extracted: bool,
    pub files_renamed: usize,
}

/// JSON output for test command
#[derive(Debug, Serialize, Deserialize)]
pub struct TestResult {
    pub server: String,
    pub port: u16,
    pub ssl: bool,
    pub connected: bool,
    pub authenticated: bool,
    pub healthy: bool,
    pub error: Option<String>,
}

/// JSON output for errors
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorOutput {
    pub error: String,
    pub details: Option<String>,
}

impl ErrorOutput {
    pub fn from_error(e: &dyn std::error::Error) -> Self {
        Self {
            error: e.to_string(),
            details: e.source().map(|s| s.to_string()),
        }
    }
}
