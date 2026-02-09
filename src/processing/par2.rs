//! PAR2 verification and repair functionality

use indicatif::ProgressBar;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::PostProcessingConfig;
use crate::error::{DlNzbError, PostProcessingError};
use crate::patterns::par2 as par2_patterns;
use crate::progress;
use par2_rs::{MessageCallback, MessageLevel, Par2Operation, Par2Repairer, ProgressCallback};

type Result<T> = std::result::Result<T, DlNzbError>;

/// Result of PAR2 repair attempt
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Par2Status {
    /// No PAR2 files found - safe to proceed with extraction
    NoPar2Files,
    /// PAR2 repair succeeded - files verified/repaired, safe to extract
    Success,
    /// PAR2 repair failed - files may be corrupted, NOT safe to extract
    Failed,
}

/// Run PAR2 verification and repair on downloaded files
pub async fn repair_with_par2(
    config: &PostProcessingConfig,
    download_dir: &Path,
    downloaded_par2_files: &[PathBuf],
    progress_bar: &ProgressBar,
) -> Result<Par2Status> {
    progress_bar.set_message("Searching for PAR2 files...");

    if downloaded_par2_files.is_empty() {
        progress_bar.finish_and_clear();
        return Ok(Par2Status::NoPar2Files);
    }

    // Get list of files before PAR2 repair (to detect renames)
    let files_before: HashSet<String> = std::fs::read_dir(download_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();

    let mut par2_files = downloaded_par2_files.to_vec();

    // Use a fixed scale to keep progress monotonic across PAR2 stages
    let progress_scale = 1000u64;
    progress_bar.set_length(progress_scale);
    progress::apply_style(progress_bar, progress::ProgressStyle::Par2);

    // Find the main PAR2 file (index file without .vol)
    let main_par2 = if let Some(main) = par2_files.iter().find(|p| par2_patterns::is_main_par2(p)) {
        main
    } else {
        // Fall back to smallest file
        par2_files.sort_by_key(|p| p.metadata().ok().map(|m| m.len()).unwrap_or(u64::MAX));
        par2_files
            .first()
            .ok_or(PostProcessingError::Par2(par2_rs::Par2Error::NotFound))?
    };

    progress_bar.set_position(0);
    progress_bar.set_message("Verifying files...");

    let repairer = Par2Repairer::new(main_par2).map_err(PostProcessingError::Par2)?;

    // Track counts for live status updates
    #[derive(Default)]
    struct Par2Counts {
        damaged: usize,
        missing: usize,
        obfuscated: usize,
        repaired: usize,
    }
    let counts = Arc::new(std::sync::Mutex::new(Par2Counts::default()));
    let messages: Arc<std::sync::Mutex<Vec<(MessageLevel, String)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    // Progress callback updates the progress bar
    let pb_clone = progress_bar.clone();
    let counts_for_progress = counts.clone();
    let progress_callback: ProgressCallback = Arc::new(move |operation, current, total| {
        let (base, span) = match operation {
            Par2Operation::Scanning => (0.0, 0.15),
            Par2Operation::Loading => (0.15, 0.1),
            Par2Operation::Verifying => (0.25, 0.55),
            Par2Operation::Repairing => (0.8, 0.2),
        };
        let pct = if total > 0 {
            (current as f64 / total as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let overall = ((base + span * pct) * progress_scale as f64).round() as u64;
        pb_clone.set_position(overall.min(progress_scale));

        match operation {
            Par2Operation::Scanning => {
                pb_clone.set_message("Scanning files...");
                progress::apply_style(&pb_clone, progress::ProgressStyle::Par2);
            }
            Par2Operation::Loading => {
                pb_clone.set_message("Loading PAR2 data...");
                progress::apply_style(&pb_clone, progress::ProgressStyle::Par2);
            }
            Par2Operation::Verifying => {
                if let Ok(c) = counts_for_progress.lock() {
                    let mut parts = Vec::new();
                    if c.obfuscated > 0 {
                        parts.push(format!("{} found", c.obfuscated));
                    }
                    if c.damaged > 0 {
                        parts.push(format!("{} damaged", c.damaged));
                    }
                    if c.missing > 0 {
                        parts.push(format!("{} missing", c.missing));
                    }
                    if parts.is_empty() {
                        pb_clone.set_message("Verifying...");
                    } else {
                        pb_clone.set_message(format!("Verifying... ({})", parts.join(", ")));
                    }
                } else {
                    pb_clone.set_message("Verifying...");
                }
                progress::apply_style(&pb_clone, progress::ProgressStyle::Par2Verify);
            }
            Par2Operation::Repairing => {
                pb_clone.set_message("Repairing...");
                progress::apply_style(&pb_clone, progress::ProgressStyle::Par2Repair);
            }
        }
    });

    // Message callback collects messages and updates counts
    // Note: Message patterns are coupled to par2-rs message format
    let messages_clone = messages.clone();
    let counts_clone = counts.clone();
    let message_callback: MessageCallback = Arc::new(move |level, message| {
        if let Ok(mut msgs) = messages_clone.lock() {
            msgs.push((level, message.to_string()));
        }

        if let Ok(mut c) = counts_clone.lock() {
            match level {
                MessageLevel::Warning if message.contains("damaged") => c.damaged += 1,
                MessageLevel::Error if message.contains("Missing") => c.missing += 1,
                MessageLevel::Info if message.contains("obfuscated") => c.obfuscated += 1,
                MessageLevel::Info if message.contains("Repairing") => c.repaired += 1,
                _ => {}
            }
        }
    });

    match repairer.repair_with_callbacks(
        true,
        false,
        Some(progress_callback),
        Some(message_callback),
    ) {
        Ok(()) => {
            progress_bar.set_position(progress_scale);

            // Check if any files were renamed
            let files_after: HashSet<String> = std::fs::read_dir(download_dir)?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect();

            let renamed_count = files_before.symmetric_difference(&files_after).count() / 2;

            // Delete PAR2 files if configured
            if config.delete_par2_after_repair {
                for par2_path in downloaded_par2_files {
                    if par2_path.exists() {
                        let _ = std::fs::remove_file(par2_path);
                    }
                }
            }

            progress_bar.finish_with_message("  ");

            // Build summary from counts
            let mut summary_parts = Vec::new();
            if renamed_count > 0 {
                summary_parts.push(format!("{} renamed", renamed_count));
            }
            if let Ok(c) = counts.lock() {
                if c.obfuscated > 0 {
                    summary_parts.push(format!("{} deobfuscated", c.obfuscated));
                }
                if c.repaired > 0 {
                    summary_parts.push(format!("{} repaired", c.repaired));
                }
            }

            if summary_parts.is_empty() {
                println!("  └─ \x1b[33m✓ PAR2 verified\x1b[0m");
            } else {
                println!(
                    "  └─ \x1b[33m✓ PAR2 verified ({})\x1b[0m",
                    summary_parts.join(", ")
                );
            }

            Ok(Par2Status::Success)
        }
        Err(e) => {
            let error_msg = e.to_string();

            progress::apply_style(progress_bar, progress::ProgressStyle::Par2Error);
            progress_bar.finish_with_message("  ");

            if let Ok(c) = counts.lock() {
                let mut issue_parts = Vec::new();
                if c.damaged > 0 {
                    issue_parts.push(format!("{} damaged", c.damaged));
                }
                if c.missing > 0 {
                    issue_parts.push(format!("{} missing", c.missing));
                }

                if !issue_parts.is_empty() {
                    println!(
                        "  \x1b[33m⚠ {} files with issues\x1b[0m",
                        issue_parts.join(", ")
                    );
                }
            }

            let short_error = if error_msg.contains("Need") && error_msg.contains("recovery blocks")
            {
                "Not enough recovery data to repair"
            } else {
                &error_msg
            };

            println!("  └─ \x1b[31m✗ PAR2 failed: {}\x1b[0m", short_error);

            Ok(Par2Status::Failed)
        }
    }
}
