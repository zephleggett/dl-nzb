//! The terminal front end: renders one engine job's events as the CLI's
//! spinners, progress bars and status lines (all on stderr). The final
//! summary is printed by `main` from the job's `JobSummary`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use human_bytes::human_bytes;
use indicatif::ProgressBar;

use crate::engine::{
    AvailabilityInfo, FileKind, JobEvent, JobObserver, JobPhase, JobProgress, NzbInfo, Outcome,
    Verdict,
};
use crate::progress::{self, DelayedSpinner, ProgressStyle};
use crate::ui::{self, glyph, style};

/// Bars for the fraction-driven phases (PAR2, extraction) run 0..=1000.
const FRACTION_SCALE: u64 = 1000;

pub struct TerminalObserver {
    /// NZB data bytes, to express availability as "% of data available".
    data_bytes: u64,
    /// PAR2 recovery bytes the on-demand download defers (all PAR2 files but
    /// the smallest index), for the "skipped" line; 0 when everything is
    /// downloaded up front.
    deferred_recovery_bytes: u64,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    phase: Option<JobPhase>,
    spinner: Option<DelayedSpinner>,
    /// `create_spinner` bar for the renaming phase.
    spinner_bar: Option<ProgressBar>,
    bar: Option<ProgressBar>,
    /// Live wire speed (f64 bits) read by the download bar's speed widget.
    speed: Arc<AtomicU64>,
    last: Option<JobProgress>,
    /// Articles failed when the current phase began.
    failed_at_phase_start: u64,
    first_byte_at: Option<Instant>,
    last_progress_at: Option<Instant>,
    /// A phase was entered (the first one tells a resumed job apart).
    seen_phase: bool,
    /// Bytes an earlier session already downloaded in the current download
    /// phase (its first progress reports them): excluded from the speed.
    resumed_bytes: u64,
    /// The "Resuming" line was printed.
    said_resuming: bool,
}

impl TerminalObserver {
    pub fn new(info: &NzbInfo, download_all_par2: bool) -> Self {
        let mut par2: Vec<u64> = info
            .files
            .iter()
            .filter(|f| f.kind == FileKind::Par2)
            .map(|f| f.bytes)
            .collect();
        par2.sort_unstable();
        let deferred_recovery_bytes = if download_all_par2 {
            0
        } else {
            par2.iter().skip(1).sum()
        };
        Self {
            data_bytes: info.data_bytes,
            deferred_recovery_bytes,
            state: Mutex::new(State::default()),
        }
    }

    fn enter(&self, state: &mut State, phase: JobPhase) {
        self.leave(state, Some(phase), None);
        // Every download starts by connecting; a job that doesn't continues
        // one whose download an earlier run finished.
        if !std::mem::replace(&mut state.seen_phase, true) && phase != JobPhase::Connecting {
            state.said_resuming = true;
            ui::child(
                false,
                style::dim(&format!(
                    "{} Resuming: the download was already complete",
                    glyph::INFO
                )),
            );
        }
        state.phase = Some(phase);
        state.last = None;
        state.first_byte_at = None;
        state.last_progress_at = None;
        match phase {
            JobPhase::Connecting => {
                state.spinner = Some(spinner("Connecting to server…"));
            }
            JobPhase::Checking => {
                state.spinner = Some(spinner("Checking article availability…"));
            }
            JobPhase::Downloading | JobPhase::DownloadingRecovery => {
                if phase == JobPhase::DownloadingRecovery {
                    ui::child(
                        false,
                        style::warn(&format!(
                            "{} Missing/corrupt data — fetching PAR2 recovery for repair…",
                            glyph::RETRY
                        )),
                    );
                }
                state.speed.store(0f64.to_bits(), Ordering::Relaxed);
                let bar = progress::create_download_progress_bar(0, state.speed.clone());
                bar.set_message("(0/0)");
                state.bar = Some(bar);
            }
            JobPhase::Verifying => {
                let bar = progress::create_progress_bar(FRACTION_SCALE, ProgressStyle::Par2);
                bar.set_message("Verifying...");
                state.bar = Some(bar);
            }
            JobPhase::Repairing => {
                let bar = state.bar.take().unwrap_or_else(|| {
                    progress::create_progress_bar(FRACTION_SCALE, ProgressStyle::Par2Repair)
                });
                progress::apply_style(&bar, ProgressStyle::Par2Repair);
                bar.set_position(0);
                bar.set_message("Repairing...");
                state.bar = Some(bar);
            }
            JobPhase::Extracting => {
                let bar = progress::create_progress_bar(FRACTION_SCALE, ProgressStyle::Extract);
                bar.set_message("Extracting...");
                state.bar = Some(bar);
            }
            JobPhase::Renaming => {
                state.spinner_bar = Some(progress::create_spinner("Deobfuscating..."));
            }
        }
    }

    /// Close the current phase's display. `next` is the phase being entered
    /// (None when the job finished, with its outcome in `finished`).
    fn leave(&self, state: &mut State, next: Option<JobPhase>, finished: Option<Outcome>) {
        if let Some(s) = state.spinner.take() {
            s.finish_and_clear();
        }
        if let Some(s) = state.spinner_bar.take() {
            s.finish_and_clear();
        }
        // Verifying hands its bar on to Repairing.
        if state.phase == Some(JobPhase::Verifying) && next == Some(JobPhase::Repairing) {
            return;
        }
        let Some(bar) = state.bar.take() else {
            return;
        };
        let stopped = matches!(finished, Some(Outcome::Stopped | Outcome::Failed));
        match state.phase {
            Some(phase) if phase.is_download() && !stopped => {
                let line = self.download_line(state);
                ui::finish_clean(&bar, Some(line));
                if phase == JobPhase::Downloading
                    && next != Some(JobPhase::DownloadingRecovery)
                    && self.deferred_recovery_bytes > 0
                {
                    ui::child(
                        false,
                        style::dim(&format!(
                            "{} Data complete — skipped {} of PAR2 recovery",
                            glyph::INFO,
                            human_bytes(self.deferred_recovery_bytes as f64)
                        )),
                    );
                }
            }
            _ => bar.finish_and_clear(),
        }
    }

    /// "✓ Downloaded 1.2 GiB (14 files) at 38.2 MiB/s" for the phase just ended.
    fn download_line(&self, state: &State) -> String {
        let Some(last) = state.last.as_ref() else {
            return ui::ok_line("Downloaded 0 B");
        };
        let failed = last
            .articles_failed
            .saturating_sub(state.failed_at_phase_start);
        // Only what this run fetched (a resumed phase starts part-way).
        let fetched = last.bytes_done.saturating_sub(state.resumed_bytes);
        let speed = match (state.first_byte_at, state.last_progress_at) {
            (Some(first), Some(end)) => {
                let secs = end.duration_since(first).as_secs_f64();
                if secs >= 0.05 {
                    format!(" at {:.1} MiB/s", fetched as f64 / 1_048_576.0 / secs)
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        };
        let bytes = human_bytes(fetched as f64);
        if failed == 0 {
            ui::ok_line(format!(
                "Downloaded {bytes} ({} file{}){speed}",
                last.files_total,
                ui::plural(last.files_total as usize)
            ))
        } else {
            ui::warn_line(format!(
                "Downloaded {bytes} ({failed} article{} failed){speed}",
                ui::plural(failed as usize)
            ))
        }
    }

    fn progress(&self, state: &mut State, p: JobProgress) {
        let now = Instant::now();
        let first = state.last.is_none();
        if first {
            state.failed_at_phase_start = p.articles_failed;
            // A download phase's first progress counts exactly what earlier
            // runs left on disk.
            state.resumed_bytes = if p.phase.is_download() {
                p.bytes_done
            } else {
                0
            };
            if state.resumed_bytes > 0 && !std::mem::replace(&mut state.said_resuming, true) {
                let text = format!(
                    "{} Resuming: {} of {} already downloaded",
                    glyph::INFO,
                    human_bytes(p.bytes_done as f64),
                    human_bytes(p.bytes_total as f64)
                );
                self.line(state, style::dim(&text).to_string());
            }
        }
        if let Some(bar) = state.bar.as_ref() {
            if p.phase.is_download() {
                if p.bytes_done > state.resumed_bytes && state.first_byte_at.is_none() {
                    state.first_byte_at = Some(now);
                }
                state.last_progress_at = Some(now);
                state.speed.store(p.speed_bps.to_bits(), Ordering::Relaxed);
                bar.set_length(p.bytes_total);
                bar.set_position(p.bytes_done);
                if first {
                    // The jump to where the job was is not download speed.
                    bar.reset_eta();
                }
                bar.set_message(if p.paused {
                    format!("({}/{}) paused", p.files_done, p.files_total)
                } else {
                    format!("({}/{})", p.files_done, p.files_total)
                });
            } else {
                bar.set_position((p.fraction * FRACTION_SCALE as f64).round() as u64);
                let label = match p.phase {
                    JobPhase::Verifying => "Verifying...",
                    JobPhase::Repairing => "Repairing...",
                    _ => "Extracting...",
                };
                let damaged = (p.damaged_blocks > 0).then(|| {
                    format!(
                        "{} damaged block{}",
                        p.damaged_blocks,
                        ui::plural(p.damaged_blocks as usize)
                    )
                });
                bar.set_message(match damaged.or_else(|| p.detail.clone()) {
                    Some(detail) => format!("{label} ({detail})"),
                    None => label.to_string(),
                });
            }
        }
        state.last = Some(p);
    }

    fn availability(&self, info: &AvailabilityInfo) {
        let percent = if self.data_bytes == 0 {
            100.0
        } else {
            self.data_bytes.saturating_sub(info.missing_bytes) as f64 / self.data_bytes as f64
                * 100.0
        };
        match info.verdict {
            Verdict::Complete if info.missing_bytes > 0 => ui::child(
                false,
                style::dim(&format!(
                    "{} {:.1}% of data available (only non-essential files are missing)",
                    glyph::INFO,
                    percent
                )),
            ),
            Verdict::Repairable => ui::child(
                false,
                ui::warn_line(format!(
                    "{:.1}% of data available; {} recovery present — PAR2 repair likely.",
                    percent,
                    human_bytes(info.recovery_bytes as f64)
                )),
            ),
            Verdict::Unrepairable => {
                let reason = if info.recovery_bytes > 0 {
                    "PAR2 recovery is insufficient"
                } else {
                    "no PAR2 files for repair"
                };
                ui::child(
                    false,
                    ui::error_line(format!(
                        "{percent:.1}% of data available; {reason} — repair unlikely."
                    )),
                );
            }
            _ => {}
        }
    }

    /// Print a line without tearing a live bar.
    fn line(&self, state: &mut State, body: String) {
        if let Some(s) = state.spinner.take() {
            s.finish_and_clear();
        }
        progress::multi().suspend(|| ui::child(false, body));
    }
}

impl JobObserver for TerminalObserver {
    fn on_event(&self, event: JobEvent) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        match event {
            JobEvent::Phase(phase) => self.enter(&mut state, phase),
            JobEvent::Progress(p) => self.progress(&mut state, p),
            JobEvent::Availability(info) => {
                if let Some(s) = state.spinner.take() {
                    s.finish_and_clear();
                }
                self.availability(&info);
            }
            JobEvent::Warning(message) => self.line(&mut state, ui::warn_line(message)),
            JobEvent::Finished(summary) => {
                self.leave(&mut state, None, Some(summary.outcome));
                state.phase = None;
            }
        }
    }
}

fn spinner(message: &str) -> DelayedSpinner {
    DelayedSpinner::new(message, std::time::Duration::from_millis(150))
}
