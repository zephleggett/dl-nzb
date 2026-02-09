//! Centralized progress reporting
//!
//! Provides a unified interface for displaying progress across downloads and post-processing.

use indicatif::{ProgressBar, ProgressStyle as IndicatifStyle};
use std::time::Duration;

/// Progress display style
#[derive(Debug, Clone, Copy)]
pub enum ProgressStyle {
    Download,
    Par2,
    Par2Verify,
    Par2Repair,
    Par2Warning,
    Par2Error,
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
                    "[{bar:40.cyan/blue}] \x1b[1m{percent:>3}%\x1b[0m \x1b[36m{bytes:>10}\x1b[0m\x1b[90m/\x1b[0m\x1b[90m{total_bytes:<10}\x1b[0m \x1b[90m│\x1b[0m {bytes_per_sec} \x1b[90m│\x1b[0m {eta} \x1b[36m{msg}\x1b[0m"
                )
                .expect("invalid download progress template")
                .progress_chars("━━╸ ")
                .with_key("eta", |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                    let _ = write!(w, "\x1b[33mETA {:>4.0}s\x1b[0m", state.eta().as_secs_f64());
                })
                .with_key("bytes_per_sec", |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                    let bytes_per_sec = state.per_sec();
                    if bytes_per_sec > 1_048_576.0 {
                        let _ = write!(w, "\x1b[1;32m{:>6.2} MiB/s\x1b[0m", bytes_per_sec / 1_048_576.0);
                    } else if bytes_per_sec > 1024.0 {
                        let _ = write!(w, "\x1b[1;32m{:>6.2} KiB/s\x1b[0m", bytes_per_sec / 1024.0);
                    } else {
                        let _ = write!(w, "\x1b[1;32m{:>6.0}  B/s\x1b[0m", bytes_per_sec);
                    }
                })
            );
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
        ProgressStyle::Par2Warning => {
            bar.set_style(
                IndicatifStyle::with_template(
                    "[{bar:40.yellow}] \x1b[1m{percent:>3}%\x1b[0m \x1b[33m{msg}\x1b[0m",
                )
                .expect("invalid par2 warning progress template")
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
