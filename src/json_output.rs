use serde::Serialize;
use std::path::PathBuf;

/// JSON output for list mode
#[derive(Debug, Serialize)]
pub struct NzbInfo {
    pub file: PathBuf,
    pub total_files: usize,
    pub total_size: u64,
    pub total_segments: usize,
    pub files: Vec<FileInfo>,
}

#[derive(Debug, Serialize)]
pub struct FileInfo {
    pub filename: String,
    pub size: u64,
    pub segments: usize,
    pub is_par2: bool,
}

/// JSON output for download results.
///
/// Byte/speed semantics:
/// - `total_size`: all committed bytes on disk after truncation (decoded yEnc
///   payload), including PAR2 recovery files.
/// - `data_bytes`: committed bytes excluding PAR2 recovery files — the payload.
/// - `par2_bytes`: committed PAR2 recovery bytes (overhead that exists only to
///   repair the payload; 0 when none were fetched).
/// - `wire_bytes`: plaintext bytes pulled from the socket — the encoded payload
///   plus yEnc/NNTP framing and any retry traffic; matches what other NZB
///   clients and network monitors report. Runs ~2-4% above the encoded payload.
/// - `download_time_seconds`: wall clock from before the availability check to
///   after the download settled — useful for "how long did the CLI invocation
///   spend on the download phase".
/// - `transfer_time_seconds`: wall clock from the first segment landing to
///   the last — excludes pool warmup, availability checks, and finalization.
/// - `average_speed_mib_per_sec`: `wire_bytes / transfer_time_seconds`, in
///   1024-based MiB/s, matching the units shown by the live progress bar.
#[derive(Debug, Serialize)]
pub struct DownloadSummary {
    pub nzb: PathBuf,
    pub output_dir: PathBuf,
    pub success: bool,
    pub total_size: u64,
    pub data_bytes: u64,
    pub par2_bytes: u64,
    pub wire_bytes: u64,
    pub download_time_seconds: f64,
    pub transfer_time_seconds: f64,
    pub average_speed_mib_per_sec: f64,
    pub files: Vec<DownloadFileResult>,
    pub post_processing: PostProcessingResult,
}

#[derive(Debug, Serialize)]
pub struct DownloadFileResult {
    pub filename: String,
    pub path: PathBuf,
    pub size: u64,
    pub segments_downloaded: usize,
    pub segments_failed: usize,
    pub success: bool,
}

#[derive(Debug, Serialize)]
pub struct PostProcessingResult {
    pub par2_verified: bool,
    pub par2_repaired: bool,
    pub rar_extracted: bool,
    pub files_renamed: usize,
}

/// JSON output for test command
#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize)]
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
