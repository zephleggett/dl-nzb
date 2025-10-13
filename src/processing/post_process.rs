use std::path::{Path, PathBuf};
use indicatif::ProgressBar;
use std::time::Duration;
use unrar::Archive;

use crate::config::PostProcessingConfig;
use crate::download::DownloadResult;
use crate::error::{DlNzbError, PostProcessingError};
use crate::progress;
use super::par2_ffi::Par2Repairer;

type Result<T> = std::result::Result<T, DlNzbError>;

/// Result of PAR2 repair attempt
#[derive(Debug, Clone, Copy, PartialEq)]
enum Par2Status {
    /// No PAR2 files found - safe to proceed with extraction
    NoPar2Files,
    /// PAR2 repair succeeded - files verified/repaired, safe to extract
    Success,
    /// PAR2 repair failed - files may be corrupted, NOT safe to extract
    Failed,
}

pub struct PostProcessor {
    config: PostProcessingConfig,
}

impl PostProcessor {
    pub fn new(config: PostProcessingConfig) -> Self {
        Self { config }
    }

    pub async fn process_downloads(&self, results: &[DownloadResult]) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }

        let download_dir = results[0].path.parent().unwrap_or(Path::new("."));

        // Run PAR2 repair if configured
        // PAR2 will verify files and rename obfuscated names to real filenames
        let par2_status = if self.config.auto_par2_repair {
            let bar = ProgressBar::new(100);
            bar.enable_steady_tick(Duration::from_millis(100));

            let status = self.repair_with_par2(download_dir, &bar).await?;
            status
        } else {
            Par2Status::NoPar2Files
        };

        // Check if archive files specifically have failed segments
        let archive_files_with_failures = self.check_archive_files_integrity(results, download_dir)?;

        // Extract RAR archives ONLY if:
        // 1. No RAR files have failed segments AND no PAR2 files exist, OR
        // 2. PAR2 repair succeeded (verified/repaired the files)
        let should_extract = self.config.auto_extract_rar && (
            (archive_files_with_failures.is_empty() && par2_status == Par2Status::NoPar2Files) ||
            par2_status == Par2Status::Success
        );

        if should_extract {
            let bar = ProgressBar::new(100);
            bar.enable_steady_tick(Duration::from_millis(100));

            self.extract_rar_archives(download_dir, &bar).await?;
        } else if par2_status == Par2Status::Failed {
            println!("⚠️  Skipping RAR extraction - PAR2 repair failed");
        } else if !archive_files_with_failures.is_empty() && self.config.auto_extract_rar {
            println!("⚠️  Skipping RAR extraction - {} files have failed segments", archive_files_with_failures.len());
            for file in &archive_files_with_failures {
                println!("   • {}", file);
            }
        }

        Ok(())
    }

    async fn repair_with_par2(&self, download_dir: &Path, progress_bar: &ProgressBar) -> Result<Par2Status> {
        progress_bar.set_message("🔧 Searching for PAR2 files...");

        // Get list of files before PAR2 repair (to detect renames)
        let files_before: std::collections::HashSet<String> = std::fs::read_dir(download_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();

        // Find PAR2 files in download directory
        let mut par2_files: Vec<PathBuf> = std::fs::read_dir(download_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.to_lowercase() == "par2")
                    .unwrap_or(false)
            })
            .collect();

        if par2_files.is_empty() {
            progress_bar.finish_with_message("ℹ️  No PAR2 files found");
            return Ok(Par2Status::NoPar2Files);
        }

        // Count total files to scan for progress tracking
        let total_files = files_before.len() as u64;
        progress_bar.set_length(total_files);
        progress::apply_style(progress_bar, progress::ProgressStyle::Par2);

        // Find the main PAR2 file (without .vol in the name)
        let main_par2 = if let Some(main) = par2_files.iter().find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|name| !name.contains(".vol"))
                .unwrap_or(false)
        }) {
            main
        } else {
            // Fall back to smallest file
            par2_files.sort_by_key(|p| p.metadata().ok().map(|m| m.len()).unwrap_or(u64::MAX));
            par2_files
                .first()
                .ok_or_else(|| PostProcessingError::Par2NotFound)?
        };

        progress_bar.set_position(0);
        progress_bar.set_message("🔧 Verifying files...");

        // Use par2cmdline-turbo library via FFI
        let repairer = Par2Repairer::new(main_par2)?;

        // Simulate progress while PAR2 runs (since PAR2 lib doesn't provide progress callbacks)
        // We'll update to 100% on completion
        match repairer.repair(true) {
            Ok(()) => {
                progress_bar.set_position(total_files);

                // Check if any files were renamed
                let files_after: std::collections::HashSet<String> = std::fs::read_dir(download_dir)?
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect();

                let renamed_count = files_before.symmetric_difference(&files_after).count() / 2;

                if renamed_count > 0 {
                    progress_bar.finish_with_message(format!("✅ PAR2 verified ({} files renamed)", renamed_count));
                } else {
                    progress_bar.finish_with_message("✅ PAR2 verified");
                }

                // Delete PAR2 files if configured
                if self.config.delete_par2_after_repair {
                    for par2_file in par2_files {
                        let _ = std::fs::remove_file(&par2_file);
                    }
                }
                Ok(Par2Status::Success)
            }
            Err(e) => {
                tracing::warn!("PAR2 repair failed: {}", e);
                progress_bar.finish_with_message("❌ PAR2 verification failed");
                Ok(Par2Status::Failed)
            }
        }
    }

    fn check_archive_files_integrity(&self, results: &[DownloadResult], download_dir: &Path) -> Result<Vec<String>> {
        let mut failed_rar_files = Vec::new();

        // Get list of RAR files in the download directory
        let rar_files: Vec<PathBuf> = std::fs::read_dir(download_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| self.is_rar_archive(path))
            .collect();

        // Check if any of these RAR files had failed segments during download
        for rar_path in rar_files {
            let filename = rar_path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            // Find the corresponding download result
            if let Some(result) = results.iter().find(|r| {
                r.path.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n == filename)
                    .unwrap_or(false)
            }) {
                if result.segments_failed > 0 {
                    failed_rar_files.push(filename.to_string());
                }
            }
        }

        Ok(failed_rar_files)
    }

    async fn extract_rar_archives(&self, download_dir: &Path, progress_bar: &ProgressBar) -> Result<()> {
        progress_bar.set_message("📦 Scanning for RAR archives...");

        // Find RAR archive files (only first part of multi-part archives)
        let rar_files: Vec<PathBuf> = std::fs::read_dir(download_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| self.is_rar_archive(path))
            .collect();

        if rar_files.is_empty() {
            progress_bar.finish_with_message("ℹ️  No RAR archives found");
            return Ok(());
        }

        let total_archives = rar_files.len() as u64;
        progress_bar.set_length(total_archives);
        progress::apply_style(progress_bar, progress::ProgressStyle::Extract);

        let mut extracted_count = 0;

        for (index, rar_path) in rar_files.iter().enumerate() {
            let filename = rar_path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            progress_bar.set_position(index as u64);
            progress_bar.set_message(format!("📦 Extracting {}", filename));

            if self.extract_rar_archive(rar_path, download_dir, progress_bar).await? {
                extracted_count += 1;
                if self.config.delete_rar_after_extract {
                    self.delete_rar_parts(rar_path, download_dir)?;
                }
            }
        }

        progress_bar.set_position(total_archives);
        progress_bar.finish_with_message(format!("✅ Extracted {} archives", extracted_count));
        Ok(())
    }

    fn is_rar_archive(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase() == "rar")
            .unwrap_or(false)
            && {
                // Only extract the first part of multi-part RAR archives
                let filename = path.file_name()
                    .map(|n| n.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                filename.contains(".part001.")
                    || filename.contains(".part01.")
                    || filename.ends_with(".part01.rar")
                    || filename.ends_with(".part001.rar")
                    || !filename.contains(".part")
            }
    }

    fn delete_rar_parts(&self, rar_path: &Path, download_dir: &Path) -> Result<()> {
        let filename = rar_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        // Extract the base name (everything before .partXX.rar or .rXX)
        let base_name = if let Some(pos) = filename.to_lowercase().find(".part") {
            &filename[..pos]
        } else if let Some(pos) = filename.to_lowercase().rfind(".r") {
            // Handle .r00, .r01, etc. format
            &filename[..pos]
        } else {
            filename
        };

        // Find and delete all related RAR parts in the directory
        if let Ok(entries) = std::fs::read_dir(download_dir) {
            let base_lower = base_name.to_lowercase();

            for entry in entries.filter_map(|e| e.ok()) {
                let entry_name = entry.file_name().to_string_lossy().to_lowercase();

                // Check if this file is part of the same RAR archive
                if entry_name.starts_with(&base_lower)
                    && (entry_name.contains(".part") || entry_name.contains(".r"))
                    && (entry_name.ends_with(".rar") || entry_name.matches(".r").count() > 0)
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        Ok(())
    }


    async fn extract_rar_archive(&self, archive_path: &Path, output_dir: &Path, _progress_bar: &ProgressBar) -> Result<bool> {
        // Validate RAR archive by trying to list it
        match Archive::new(archive_path).open_for_listing() {
            Ok(listing) => {
                let mut has_entries = false;
                for entry_result in listing {
                    match entry_result {
                        Ok(_) => {
                            has_entries = true;
                            break;
                        }
                        Err(_) => return Ok(false),
                    }
                }
                if !has_entries {
                    return Ok(false);
                }
            }
            Err(_) => return Ok(false),
        }

        // Ensure output directory exists
        std::fs::create_dir_all(output_dir)?;

        // Extract the archive
        match Archive::new(archive_path).open_for_processing() {
            Ok(mut archive) => {
                let mut extracted_files = 0;

                loop {
                    match archive.read_header() {
                        Ok(Some(header)) => {
                            let entry = header.entry();
                            let filename = entry.filename.clone();

                            // Skip directory entries
                            if entry.is_directory() {
                                match header.skip() {
                                    Ok(next_archive) => {
                                        archive = next_archive;
                                        continue;
                                    }
                                    Err(_) => break,
                                }
                            }

                            // Ensure parent directory exists for nested files
                            let output_path = output_dir.join(&filename);
                            if let Some(parent) = output_path.parent() {
                                std::fs::create_dir_all(parent)?;
                            }

                            // Extract file
                            match header.extract_with_base(output_dir) {
                                Ok(next_archive) => {
                                    archive = next_archive;
                                    extracted_files += 1;
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to extract {}: {}", filename.display(), e);
                                    break;
                                }
                            }
                        }
                        Ok(None) => break, // End of archive
                        Err(e) => {
                            tracing::warn!("Error reading RAR header: {}", e);
                            break;
                        }
                    }
                }

                Ok(extracted_files > 0)
            }
            Err(e) => {
                tracing::error!("Failed to open RAR archive {}: {}", archive_path.display(), e);
                Ok(false)
            }
        }
    }

}
