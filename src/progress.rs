//! Centralized progress reporting
//!
//! Provides a unified interface for displaying progress across downloads and post-processing.

use human_bytes::human_bytes;
use indicatif::{ProgressBar, ProgressStyle as IndicatifStyle};
use std::time::Duration;

/// Progress display style
#[derive(Debug, Clone, Copy)]
pub enum ProgressStyle {
    Download,
    Par2,
    Extract,
}

/// Create a progress bar with the specified style
pub fn create_progress_bar(total: u64, style: ProgressStyle) -> ProgressBar {
    let bar = ProgressBar::new(total);
    apply_style(&bar, style);
    bar.enable_steady_tick(Duration::from_millis(100));
    bar
}

/// Apply a style to an existing progress bar
pub fn apply_style(bar: &ProgressBar, style: ProgressStyle) {
    match style {
        ProgressStyle::Download => {
            bar.set_style(
                IndicatifStyle::with_template(
                    "[{bar:40.cyan/blue}] {percent:>3}% {bytes:>10}/{total_bytes:<10} {bytes_per_sec:>12} ETA {eta:>5} {msg}"
                )
                .unwrap()
                .progress_chars("━━╸ ")
                .with_key("eta", |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                    let _ = write!(w, "{:>5.0}s", state.eta().as_secs_f64());
                })
                .with_key("bytes_per_sec", |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                    let bytes_per_sec = state.per_sec();
                    if bytes_per_sec > 1_048_576.0 {
                        let _ = write!(w, "{:>7.2} MiB/s", bytes_per_sec / 1_048_576.0);
                    } else if bytes_per_sec > 1024.0 {
                        let _ = write!(w, "{:>7.2} KiB/s", bytes_per_sec / 1024.0);
                    } else {
                        let _ = write!(w, "{:>7.0}  B/s", bytes_per_sec);
                    }
                })
            );
        }
        ProgressStyle::Par2 => {
            bar.set_style(
                IndicatifStyle::with_template("[{bar:40.yellow}] {percent:>3}% {msg}")
                    .unwrap()
                    .progress_chars("━━╸ ")
            );
        }
        ProgressStyle::Extract => {
            bar.set_style(
                IndicatifStyle::with_template("[{bar:40.green}] {percent:>3}% {msg}")
                    .unwrap()
                    .progress_chars("━━╸ ")
            );
        }
    }
}

/// Format a download summary message
pub fn format_download_summary(
    files_count: usize,
    total_files: usize,
    bytes_downloaded: u64,
    failed_files: usize,
) -> String {
    if failed_files == 0 {
        format!(
            "({}/{}) ✓ Downloaded {}",
            files_count,
            total_files,
            human_bytes(bytes_downloaded as f64)
        )
    } else {
        format!(
            "({}/{}) ⚠ Downloaded {} ({} with errors)",
            files_count,
            total_files,
            human_bytes(bytes_downloaded as f64),
            failed_files
        )
    }
}
