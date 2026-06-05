//! PAR2 verification and repair.

use indicatif::ProgressBar;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::PostProcessingConfig;
use crate::error::{DlNzbError, PostProcessingError};
use crate::patterns::par2 as par2_patterns;
use crate::progress;
use par2_rs::{MessageCallback, MessageLevel, Par2Operation, Par2Repairer, ProgressCallback};

type Result<T> = std::result::Result<T, DlNzbError>;

/// Outcome of a PAR2 verify+repair attempt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Par2Status {
    /// No PAR2 files present.
    NoPar2Files,
    /// Files verified clean or repair completed successfully.
    Success,
    /// Repair attempted but failed (insufficient recovery, missing data, etc.).
    Failed,
}

/// Run PAR2 verification (and repair if needed) on the downloaded payload.
///
/// Errors from par2-rs that don't already classify as "verification failed" are
/// surfaced to the caller. Verification failure itself returns `Par2Status::Failed`
/// (not an error), because the caller chooses what to do with that information.
pub async fn repair_with_par2(
    config: &PostProcessingConfig,
    download_dir: &Path,
    downloaded_par2_files: &[PathBuf],
    progress_bar: &ProgressBar,
) -> Result<Par2Status> {
    if downloaded_par2_files.is_empty() {
        progress_bar.finish_and_clear();
        return Ok(Par2Status::NoPar2Files);
    }

    progress_bar.set_message("Searching for PAR2 files...");

    // Capture filenames before so we can report on renames/deobfuscation.
    let files_before: HashSet<String> = match std::fs::read_dir(download_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(e) => {
            tracing::warn!("PAR2: cannot read {}: {}", download_dir.display(), e);
            return Ok(Par2Status::Failed);
        }
    };

    // Prefer the main index file (no .vol). Fall back to smallest .par2.
    let mut par2_candidates = downloaded_par2_files.to_vec();
    let main_par2: PathBuf = if let Some(main) = par2_candidates
        .iter()
        .find(|p| par2_patterns::is_main_par2(p))
    {
        main.clone()
    } else {
        par2_candidates.sort_by_key(|p| p.metadata().ok().map(|m| m.len()).unwrap_or(u64::MAX));
        par2_candidates
            .first()
            .cloned()
            .ok_or(PostProcessingError::Par2(par2_rs::Par2Error::NotFound))?
    };

    progress_bar.set_position(0);
    progress_bar.set_message("Verifying files...");
    progress::apply_style(progress_bar, progress::ProgressStyle::Par2);

    let repairer = Par2Repairer::new(&main_par2).map_err(PostProcessingError::Par2)?;

    #[derive(Default)]
    struct Counts {
        damaged: usize,
        missing: usize,
        deobfuscated: usize,
        repaired: usize,
    }
    let counts = Arc::new(Mutex::new(Counts::default()));
    let messages: Arc<Mutex<Vec<(MessageLevel, String)>>> = Arc::new(Mutex::new(Vec::new()));

    let pb_clone = progress_bar.clone();
    let last_op: Arc<Mutex<Option<Par2Operation>>> = Arc::new(Mutex::new(None));
    let progress_cb: ProgressCallback = Arc::new(move |operation, current, total| {
        if let Ok(mut last) = last_op.lock() {
            if *last != Some(operation) {
                *last = Some(operation);
                drop(last);
                pb_clone.set_position(0);
                pb_clone.set_length(total);
                match operation {
                    Par2Operation::Scanning => {
                        pb_clone.set_message("Scanning files...");
                        progress::apply_style(&pb_clone, progress::ProgressStyle::Par2);
                    }
                    Par2Operation::Loading => {
                        pb_clone.set_message("Loading PAR2 metadata...");
                        progress::apply_style(&pb_clone, progress::ProgressStyle::Par2);
                    }
                    Par2Operation::Verifying => {
                        pb_clone.set_message("Verifying...");
                        progress::apply_style(&pb_clone, progress::ProgressStyle::Par2Verify);
                    }
                    Par2Operation::Repairing => {
                        pb_clone.set_message("Repairing...");
                        progress::apply_style(&pb_clone, progress::ProgressStyle::Par2Repair);
                    }
                }
            }
        }
        pb_clone.set_position(current.min(total));
    });

    let messages_clone = messages.clone();
    let counts_clone = counts.clone();
    let pb_for_msg = progress_bar.clone();
    let message_cb: MessageCallback = Arc::new(move |level, message| {
        if let Ok(mut msgs) = messages_clone.lock() {
            msgs.push((level, message.to_string()));
        }
        if let Ok(mut c) = counts_clone.lock() {
            let mut changed = false;
            match level {
                MessageLevel::Warning if message.contains("damaged") => {
                    c.damaged += 1;
                    changed = true;
                }
                MessageLevel::Error if message.contains("Missing") => {
                    c.missing += 1;
                    changed = true;
                }
                MessageLevel::Info if message.contains("obfuscated") => {
                    c.deobfuscated += 1;
                    changed = true;
                }
                MessageLevel::Info if message.contains("Repairing") => {
                    c.repaired += 1;
                }
                _ => {}
            }
            if changed {
                let mut parts = Vec::new();
                if c.deobfuscated > 0 {
                    parts.push(format!("{} found", c.deobfuscated));
                }
                if c.damaged > 0 {
                    parts.push(format!("{} damaged", c.damaged));
                }
                if c.missing > 0 {
                    parts.push(format!("{} missing", c.missing));
                }
                if !parts.is_empty() {
                    pb_for_msg.set_message(format!("Verifying... ({})", parts.join(", ")));
                }
            }
        }
    });

    // par2-rs CPU-intensive work goes on a blocking thread so the async runtime
    // can keep driving the progress bar. The shutdown flag is handed in so a
    // Ctrl+C mid-repair aborts promptly; par2-rs reconstructs into temp files
    // and only commits on success, so an abort never corrupts the originals.
    let purge_par2 = config.delete_par2_after_repair;
    let cancel = crate::shutdown::handle();
    let result = tokio::task::spawn_blocking(move || {
        repairer.repair_with_callbacks_cancellable(
            true,
            purge_par2,
            Some(progress_cb),
            Some(message_cb),
            Some(cancel),
        )
    })
    .await;

    // A cancelled repair is not a real failure — the process is exiting; report
    // quietly so we don't print a scary "PAR2 failed" on Ctrl+C.
    if let Ok(Err(par2_rs::Par2Error::Cancelled)) = &result {
        progress_bar.finish_and_clear();
        return Ok(Par2Status::Failed);
    }

    match result {
        Ok(Ok(())) => {
            progress_bar.set_position(progress_bar.length().unwrap_or(0));

            let files_after: HashSet<String> = match std::fs::read_dir(download_dir) {
                Ok(rd) => rd
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect(),
                Err(_) => HashSet::new(),
            };
            let renamed_count = files_before.symmetric_difference(&files_after).count() / 2;

            let mut summary = Vec::new();
            if renamed_count > 0 {
                summary.push(format!("{} renamed", renamed_count));
            }
            if let Ok(c) = counts.lock() {
                if c.deobfuscated > 0 {
                    summary.push(format!("{} deobfuscated", c.deobfuscated));
                }
                if c.repaired > 0 {
                    summary.push(format!("{} repaired", c.repaired));
                }
            }

            // A successful verify is green (success), not the bar's working yellow.
            let body = if summary.is_empty() {
                crate::ui::ok_line("PAR2 verified")
            } else {
                crate::ui::ok_line(format!("PAR2 verified ({})", summary.join(", ")))
            };
            crate::ui::finish_clean(progress_bar, Some(body));
            Ok(Par2Status::Success)
        }
        Ok(Err(e)) => {
            crate::ui::finish_clean(progress_bar, None);

            let mut issue_parts = Vec::new();
            if let Ok(c) = counts.lock() {
                if c.damaged > 0 {
                    issue_parts.push(format!("{} damaged", c.damaged));
                }
                if c.missing > 0 {
                    issue_parts.push(format!("{} missing", c.missing));
                }
            }
            let error_msg = e.to_string();
            let short_error = if error_msg.contains("Need") && error_msg.contains("recovery blocks")
            {
                "not enough recovery data to repair".to_string()
            } else {
                error_msg
            };

            if !issue_parts.is_empty() {
                crate::ui::child(
                    false,
                    crate::ui::warn_line(format!("{} files with issues", issue_parts.join(", "))),
                );
            }
            crate::ui::child(
                true,
                crate::ui::error_line(format!("PAR2 failed: {short_error}")),
            );
            Ok(Par2Status::Failed)
        }
        Err(join_err) => {
            crate::ui::finish_clean(progress_bar, None);
            crate::ui::child(
                true,
                crate::ui::error_line(format!("PAR2 failed: internal error: {join_err}")),
            );
            Ok(Par2Status::Failed)
        }
    }
}
