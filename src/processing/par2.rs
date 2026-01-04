//! PAR2 verification and repair functionality via par2cmdline-turbo CLI

use indicatif::ProgressBar;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::config::PostProcessingConfig;
use crate::error::DlNzbError;
use crate::progress;

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

/// Find the par2 binary, checking bundled location first, then PATH
fn find_par2_binary() -> Result<PathBuf> {
    // Check for bundled binary relative to executable
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            // Check common bundled locations
            let bundled_paths = [
                exe_dir.join("par2"),
                exe_dir.join("par2.exe"),
                exe_dir
                    .join("vendor")
                    .join("par2cmdline-turbo")
                    .join("par2"),
                exe_dir
                    .join("vendor")
                    .join("par2cmdline-turbo")
                    .join("par2.exe"),
            ];

            for path in &bundled_paths {
                if path.exists() {
                    return Ok(path.clone());
                }
            }
        }
    }

    // Check if par2 is in PATH
    #[cfg(windows)]
    let par2_name = "par2.exe";
    #[cfg(not(windows))]
    let par2_name = "par2";

    // Try to find in PATH using `which` equivalent
    if let Ok(path) = which::which(par2_name) {
        return Ok(path);
    }

    // Fallback: just use "par2" and hope it's in PATH
    Ok(PathBuf::from(par2_name))
}

/// Run PAR2 verification and repair on downloaded files
pub async fn repair_with_par2(
    config: &PostProcessingConfig,
    _download_dir: &Path,
    downloaded_par2_files: &[PathBuf],
    progress_bar: &ProgressBar,
) -> Result<Par2Status> {
    if downloaded_par2_files.is_empty() {
        progress_bar.finish_and_clear();
        return Ok(Par2Status::NoPar2Files);
    }

    // Find the main PAR2 file (index file without .vol)
    // We use the first PAR2 file provided as the entry point
    let main_par2 = downloaded_par2_files.first().ok_or_else(|| {
        DlNzbError::PostProcessing(crate::error::PostProcessingError::NoRarArchives)
    })?;

    // Find par2 binary
    let par2_bin = find_par2_binary()?;

    progress_bar.set_message("Verifying PAR2...");
    progress::apply_style(progress_bar, progress::ProgressStyle::Par2Verify);

    // Run par2 repair command
    // par2cmdline-turbo uses: par2 repair <par2file>
    let mut child = Command::new(&par2_bin)
        .arg("repair")
        .arg(main_par2)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            DlNzbError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "Failed to execute par2 binary '{}': {}",
                    par2_bin.display(),
                    e
                ),
            ))
        })?;

    // Read stdout for progress updates
    let stdout = child.stdout.take().expect("stdout not captured");
    let mut reader = BufReader::new(stdout).lines();

    let mut repair_needed = false;
    let mut repair_possible = true;
    let mut files_verified = 0u64;
    let mut total_files = 0u64;

    while let Ok(Some(line)) = reader.next_line().await {
        // Parse progress from par2cmdline-turbo output
        // Common patterns:
        // "Loading \"file.par2\"."
        // "Verifying source files:"
        // "Target: \"filename\" - found."
        // "Target: \"filename\" - damaged."
        // "Repair is required."
        // "Repair is possible."
        // "Repair complete."
        // "Repair is not possible."

        if line.contains("Verifying source files") {
            progress_bar.set_message("Verifying files...");
            progress::apply_style(progress_bar, progress::ProgressStyle::Par2Verify);
        } else if line.contains("Target:") && line.contains("found") {
            files_verified += 1;
            if total_files > 0 {
                progress_bar.set_position(files_verified);
            }
        } else if line.contains("Target:") && line.contains("damaged") {
            repair_needed = true;
            progress_bar.set_message("Damaged files found...");
            progress::apply_style(progress_bar, progress::ProgressStyle::Par2Warning);
        } else if line.contains("Repair is required") {
            repair_needed = true;
        } else if line.contains("Repair is not possible") {
            repair_possible = false;
            progress::apply_style(progress_bar, progress::ProgressStyle::Par2Error);
        } else if line.contains("Repairing:") {
            progress_bar.set_message("Repairing...");
            progress::apply_style(progress_bar, progress::ProgressStyle::Par2Repair);
        } else if line.contains("Repair complete") {
            progress_bar.set_message("Repair complete");
        } else if line.contains("All files are correct") {
            progress_bar.set_message("All files verified");
        } else if line.contains("source files") {
            // Try to parse "Scanning X source files"
            if let Some(count) = parse_file_count(&line) {
                total_files = count;
                progress_bar.set_length(total_files);
            }
        }
    }

    // Wait for command to complete
    let status = child.wait().await.map_err(|e| {
        DlNzbError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to wait for par2 process: {}", e),
        ))
    })?;

    progress_bar.finish_and_clear();

    // Determine result based on exit code and parsed output
    // par2cmdline exit codes:
    // 0 = success (no repair needed or repair succeeded)
    // 1 = repair needed and completed successfully
    // 2 = repair needed but not possible
    // Other = error

    let result = if status.success() || status.code() == Some(0) {
        if repair_needed {
            // Delete PAR2 files if configured
            if config.delete_par2_after_repair {
                for par2_path in downloaded_par2_files {
                    if par2_path.exists() {
                        let _ = std::fs::remove_file(par2_path);
                    }
                }
            }
            println!("  └─ \x1b[33m✓ PAR2 repaired successfully\x1b[0m");
        } else {
            // Delete PAR2 files if configured
            if config.delete_par2_after_repair {
                for par2_path in downloaded_par2_files {
                    if par2_path.exists() {
                        let _ = std::fs::remove_file(par2_path);
                    }
                }
            }
            println!("  └─ \x1b[33m✓ PAR2 verified\x1b[0m");
        }
        Par2Status::Success
    } else if !repair_possible {
        println!("  └─ \x1b[31m✗ PAR2 repair not possible - insufficient recovery data\x1b[0m");
        Par2Status::Failed
    } else {
        let code = status.code().unwrap_or(-1);
        println!("  └─ \x1b[31m✗ PAR2 failed (exit code: {})\x1b[0m", code);
        Par2Status::Failed
    };

    Ok(result)
}

/// Parse file count from par2 output like "Scanning 15 source files"
fn parse_file_count(line: &str) -> Option<u64> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "source" && i > 0 {
            if let Ok(count) = parts[i - 1].parse::<u64>() {
                return Some(count);
            }
        }
    }
    None
}
