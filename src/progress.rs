//! Centralized progress reporting
//!
//! Provides a unified interface for displaying progress across downloads and post-processing.

use indicatif::{ProgressBar, ProgressStyle as IndicatifStyle};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Progress display style
#[derive(Debug, Clone, Copy)]
pub enum ProgressStyle {
    Download,
    Par2,
    Par2Verify,
    Par2Repair,
    Par2Error,
    Extract,
}

/// Create a progress bar with the specified style. Hidden when the consumer
/// is in JSON / quiet mode so output streams stay machine-readable.
pub fn create_progress_bar(total: u64, style: ProgressStyle) -> ProgressBar {
    if crate::output_mode::is_quiet() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::new(total);
    apply_style(&bar, style);
    bar.enable_steady_tick(Duration::from_millis(100));
    bar
}

/// Download progress bar whose `bytes_per_sec` widget reads from an external
/// atomic (the live socket-read rate published by the sampler task) instead of
/// indicatif's `state.per_sec()`. Position still tracks segment-byte progress
/// for accurate %/ETA — the speed widget reports actual wire throughput.
pub fn create_download_progress_bar(total: u64, live_speed_bps: Arc<AtomicU64>) -> ProgressBar {
    if crate::output_mode::is_quiet() {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::new(total);
    apply_download_style(&bar, live_speed_bps);
    bar.enable_steady_tick(Duration::from_millis(100));
    bar
}

/// Create a styled cyan spinner. Hidden in quiet/JSON mode.
pub fn create_spinner(msg: impl Into<String>) -> ProgressBar {
    if crate::output_mode::is_quiet() {
        return ProgressBar::hidden();
    }
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        IndicatifStyle::with_template("{spinner:.cyan} {msg}")
            .expect("invalid spinner template")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner.set_message(msg.into());
    spinner
}

/// Apply a style to an existing progress bar.
///
/// `ProgressStyle::Download` here uses indicatif's built-in `per_sec()` for
/// the speed widget. The downloader prefers [`apply_download_style`] which
/// reads from an external atomic populated by a socket-byte sampler.
pub fn apply_style(bar: &ProgressBar, style: ProgressStyle) {
    match style {
        ProgressStyle::Download => {
            bar.set_style(download_style_template(None));
        }
        ProgressStyle::Par2 => {
            bar.set_style(
                IndicatifStyle::with_template(
                    "[{bar:40.yellow}] \x1b[1m{percent:>3}%\x1b[0m \x1b[33m{msg}\x1b[0m",
                )
                .expect("invalid par2 progress template")
                .progress_chars("━━╸ "),
            );
        }
        ProgressStyle::Par2Verify => {
            bar.set_style(
                IndicatifStyle::with_template(
                    "[{bar:40.cyan/blue}] \x1b[1m{percent:>3}%\x1b[0m \x1b[36m{msg}\x1b[0m",
                )
                .expect("invalid par2 verify progress template")
                .progress_chars("━━╸ "),
            );
        }
        ProgressStyle::Par2Repair => {
            bar.set_style(
                IndicatifStyle::with_template(
                    "[{bar:40.magenta/red}] \x1b[1m{percent:>3}%\x1b[0m \x1b[35m{msg}\x1b[0m",
                )
                .expect("invalid par2 repair progress template")
                .progress_chars("━━╸ "),
            );
        }
        ProgressStyle::Par2Error => {
            bar.set_style(
                IndicatifStyle::with_template(
                    "[{bar:40.red}] \x1b[1m{percent:>3}%\x1b[0m \x1b[31m{msg}\x1b[0m",
                )
                .expect("invalid par2 error progress template")
                .progress_chars("━━╸ "),
            );
        }
        ProgressStyle::Extract => {
            bar.set_style(
                IndicatifStyle::with_template(
                    "[{bar:40.green}] \x1b[1m{percent:>3}%\x1b[0m \x1b[32m{msg}\x1b[0m",
                )
                .expect("invalid extract progress template")
                .progress_chars("━━╸ "),
            );
        }
    }
}

fn apply_download_style(bar: &ProgressBar, live_speed_bps: Arc<AtomicU64>) {
    bar.set_style(download_style_template(Some(live_speed_bps)));
}

/// Build the Download style template. If a `live_speed_bps` source is
/// provided, the `bytes_per_sec` widget reads from it (bits-of-f64 stored in
/// the atomic); otherwise it falls back to indicatif's `state.per_sec()`.
fn download_style_template(live_speed_bps: Option<Arc<AtomicU64>>) -> IndicatifStyle {
    let template = "[{bar:40.cyan/blue}] \x1b[1m{percent:>3}%\x1b[0m \x1b[36m{bytes:>10}\x1b[0m\x1b[90m/\x1b[0m\x1b[90m{total_bytes:<10}\x1b[0m \x1b[90m│\x1b[0m {bytes_per_sec} \x1b[90m│\x1b[0m {eta} \x1b[36m{msg}\x1b[0m";

    let style = IndicatifStyle::with_template(template)
        .expect("invalid download progress template")
        .progress_chars("━━╸ ")
        .with_key(
            "eta",
            |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                let _ = write!(w, "\x1b[33mETA {:>4.0}s\x1b[0m", state.eta().as_secs_f64());
            },
        );

    match live_speed_bps {
        Some(source) => style.with_key(
            "bytes_per_sec",
            move |_state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                let bytes_per_sec = f64::from_bits(source.load(Ordering::Relaxed));
                write_speed(w, bytes_per_sec);
            },
        ),
        None => style.with_key(
            "bytes_per_sec",
            |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                write_speed(w, state.per_sec());
            },
        ),
    }
}

fn write_speed(w: &mut dyn std::fmt::Write, bytes_per_sec: f64) {
    if bytes_per_sec > 1_048_576.0 {
        let _ = write!(
            w,
            "\x1b[1;32m{:>6.2} MiB/s\x1b[0m",
            bytes_per_sec / 1_048_576.0
        );
    } else if bytes_per_sec > 1024.0 {
        let _ = write!(w, "\x1b[1;32m{:>6.2} KiB/s\x1b[0m", bytes_per_sec / 1024.0);
    } else {
        let _ = write!(w, "\x1b[1;32m{:>6.0}  B/s\x1b[0m", bytes_per_sec);
    }
}
