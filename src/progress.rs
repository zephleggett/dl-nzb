//! Centralized progress reporting.
//!
//! Provides a unified interface for displaying progress across downloads and
//! post-processing. All bars draw to **stderr** through one shared
//! [`MultiProgress`] so their draws (and any `tracing` output routed through
//! `multi().suspend`) stay serialized and never tear each other.

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle as IndicatifStyle};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::ui::style::colors_enabled;

/// Process-wide `MultiProgress`. Bars register here via [`MultiProgress::add`] so
/// the single draw lock coordinates every bar and any suspended print/log.
/// Always targets stderr; in quiet/JSON mode no bars are added (the constructors
/// return `ProgressBar::hidden()`) and no decorative lines are printed, so the
/// target draws nothing.
pub fn multi() -> &'static MultiProgress {
    static MP: OnceLock<MultiProgress> = OnceLock::new();
    MP.get_or_init(|| MultiProgress::with_draw_target(ProgressDrawTarget::stderr()))
}

/// Progress display style.
#[derive(Debug, Clone, Copy)]
pub enum ProgressStyle {
    Download,
    Par2,
    Par2Verify,
    Par2Repair,
    Par2Error,
    Extract,
}

/// Bar fill characters: Unicode when available, ASCII on dumb/non-UTF8 terminals.
fn bar_chars() -> &'static str {
    if crate::ui::glyph::is_unicode() {
        "━━╸ "
    } else {
        "=> "
    }
}

/// The shared spinner style (cyan when colour is on) + braille tick set.
fn spinner_style() -> IndicatifStyle {
    IndicatifStyle::with_template(if colors_enabled() {
        "{spinner:.cyan} {msg}"
    } else {
        "{spinner} {msg}"
    })
    .expect("invalid spinner template")
    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
}

/// Create a progress bar with the specified style. Hidden when the consumer is
/// in JSON / quiet mode so output streams stay machine-readable.
pub fn create_progress_bar(total: u64, style: ProgressStyle) -> ProgressBar {
    if crate::output_mode::is_quiet() {
        return ProgressBar::hidden();
    }
    let bar = multi().add(ProgressBar::new(total));
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
    let bar = multi().add(ProgressBar::new(total));
    apply_download_style(&bar, live_speed_bps);
    bar.enable_steady_tick(Duration::from_millis(100));
    bar
}

/// Create a styled cyan spinner. Hidden in quiet/JSON mode.
pub fn create_spinner(msg: impl Into<String>) -> ProgressBar {
    if crate::output_mode::is_quiet() {
        return ProgressBar::hidden();
    }
    let spinner = multi().add(ProgressBar::new_spinner());
    spinner.set_style(spinner_style());
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner.set_message(msg.into());
    spinner
}

/// A spinner that stays hidden until `delay` elapses, so fast operations never
/// flash. A timer task promotes it to a visible stderr draw target if the op
/// hasn't finished first; [`DelayedSpinner::finish_and_clear`] cancels that.
pub struct DelayedSpinner {
    bar: ProgressBar,
    promote: Option<tokio::task::JoinHandle<()>>,
}

impl DelayedSpinner {
    pub fn new(msg: impl Into<String>, delay: Duration) -> Self {
        if crate::output_mode::is_quiet() {
            return Self {
                bar: ProgressBar::hidden(),
                promote: None,
            };
        }
        let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::hidden());
        bar.set_style(spinner_style());
        bar.set_message(msg.into());

        let timer_bar = bar.clone();
        let promote = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if !timer_bar.is_finished() {
                timer_bar.set_draw_target(ProgressDrawTarget::stderr());
                timer_bar.enable_steady_tick(Duration::from_millis(80));
            }
        });

        Self {
            bar,
            promote: Some(promote),
        }
    }

    /// Clear the spinner and cancel any pending promotion. Safe if never shown.
    pub fn finish_and_clear(self) {
        if let Some(h) = self.promote {
            h.abort();
        }
        self.bar.finish_and_clear();
    }
}

/// Apply a style to an existing progress bar.
pub fn apply_style(bar: &ProgressBar, style: ProgressStyle) {
    let template = match style {
        ProgressStyle::Download => return apply_download_style_fallback(bar),
        ProgressStyle::Par2 => simple_template("yellow", "33"),
        ProgressStyle::Par2Verify => simple_template("cyan/blue", "36"),
        ProgressStyle::Par2Repair => simple_template("magenta/red", "35"),
        ProgressStyle::Par2Error => simple_template("red", "31"),
        ProgressStyle::Extract => simple_template("green", "32"),
    };
    bar.set_style(
        IndicatifStyle::with_template(&template)
            .expect("invalid progress template")
            .progress_chars(bar_chars()),
    );
}

/// `[{wide_bar}] {percent}% {msg}` — coloured when enabled, plain otherwise.
fn simple_template(bar_color: &str, msg_sgr: &str) -> String {
    if colors_enabled() {
        format!(
            "[{{wide_bar:.{bar_color}}}] \x1b[1m{{percent:>3}}%\x1b[0m \x1b[{msg_sgr}m{{msg}}\x1b[0m"
        )
    } else {
        "[{wide_bar}] {percent:>3}% {msg}".to_string()
    }
}

fn apply_download_style(bar: &ProgressBar, live_speed_bps: Arc<AtomicU64>) {
    bar.set_style(download_style_template(Some(live_speed_bps)));
}

/// Used when `apply_style(ProgressStyle::Download)` is called without a live
/// speed source (falls back to indicatif's `per_sec()`).
fn apply_download_style_fallback(bar: &ProgressBar) {
    bar.set_style(download_style_template(None));
}

/// Build the Download style. With a `live_speed_bps` source the `bytes_per_sec`
/// widget reads from it; otherwise it uses indicatif's `state.per_sec()`.
fn download_style_template(live_speed_bps: Option<Arc<AtomicU64>>) -> IndicatifStyle {
    let template = if colors_enabled() {
        "[{wide_bar:.cyan/blue}] \x1b[1m{percent:>3}%\x1b[0m \x1b[36m{bytes:>10}\x1b[0m\x1b[90m/{total_bytes:<10}\x1b[0m \x1b[90m│\x1b[0m {bytes_per_sec} \x1b[90m│\x1b[0m {eta} \x1b[36m{msg}\x1b[0m"
    } else {
        "[{wide_bar}] {percent:>3}% {bytes:>10}/{total_bytes:<10} | {bytes_per_sec} | {eta} {msg}"
    };

    let style = IndicatifStyle::with_template(template)
        .expect("invalid download progress template")
        .progress_chars(bar_chars())
        .with_key(
            "eta",
            |state: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                let eta = crate::ui::format_eta(state.eta());
                if colors_enabled() {
                    let _ = write!(w, "\x1b[33mETA {eta:>6}\x1b[0m");
                } else {
                    let _ = write!(w, "ETA {eta:>6}");
                }
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
    let (value, unit) = if bytes_per_sec > 1_048_576.0 {
        (bytes_per_sec / 1_048_576.0, "MiB/s")
    } else if bytes_per_sec > 1024.0 {
        (bytes_per_sec / 1024.0, "KiB/s")
    } else {
        (bytes_per_sec, "  B/s")
    };
    let prec = if unit == "  B/s" { 0 } else { 2 };
    if colors_enabled() {
        let _ = write!(w, "\x1b[1;32m{value:>6.prec$} {unit}\x1b[0m");
    } else {
        let _ = write!(w, "{value:>6.prec$} {unit}");
    }
}
