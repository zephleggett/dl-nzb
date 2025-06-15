use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use which::which;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;
use unrar::Archive;

use crate::config::PostProcessingConfig;
use crate::downloader::DownloadResult;

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

        // Check if any files need PAR2 repair
        let needs_repair = results.iter().any(|r| r.segments_failed > 0);
        let mut par2_repair_attempted = false;

        if needs_repair && self.config.auto_par2_repair {
            let spinner = ProgressBar::new_spinner();
            spinner.set_style(ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")
                .unwrap());
            spinner.set_message("🔧 Checking for PAR2 repair...");
            spinner.enable_steady_tick(Duration::from_millis(100));

            par2_repair_attempted = self.repair_with_par2(download_dir, &spinner).await?;
            spinner.finish_and_clear();
        }

        // Check if archive files specifically have failed segments
        let archive_files_with_failures = self.check_archive_files_integrity(results, download_dir)?;

        // Extract archives if:
        // 1. No archive files have failed segments, OR
        // 2. PAR2 repair was attempted (which may have fixed the issues)
        let should_extract = (self.config.auto_extract_rar || self.config.auto_extract_zip) &&
                           (archive_files_with_failures.is_empty() || par2_repair_attempted);

        if should_extract {
            let spinner = ProgressBar::new_spinner();
            spinner.set_style(ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap());
            spinner.set_message("📦 Scanning for archives...");
            spinner.enable_steady_tick(Duration::from_millis(100));

            self.extract_archives(download_dir, &spinner).await?;
            spinner.finish_and_clear();
        } else if !archive_files_with_failures.is_empty() && (self.config.auto_extract_rar || self.config.auto_extract_zip) {
            println!("⚠️  Skipping archive extraction - {} archive files have failed segments", archive_files_with_failures.len());
            for file in &archive_files_with_failures {
                println!("   • {}", file);
            }
        }

        Ok(())
    }

    async fn repair_with_par2(&self, download_dir: &Path, spinner: &ProgressBar) -> Result<bool> {
        // Check if par2 tool is available
        let par2_cmd = which("par2").or_else(|_| which("par2repair")).or_else(|_| which("par2cmdline"));

        let par2_executable = match par2_cmd {
            Ok(path) => path,
            Err(_) => {
                spinner.finish_with_message("⚠️  PAR2 tool not found - install with: brew install par2");
                return Ok(false);
            }
        };

        // Find PAR2 files in download directory
        let par2_files: Vec<PathBuf> = std::fs::read_dir(download_dir)?
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
            spinner.finish_with_message("ℹ️  No PAR2 files found");
            return Ok(false);
        }

        // Use the first PAR2 file (usually the main one)
        let main_par2 = &par2_files[0];
        spinner.set_message("🔧 Repairing files with PAR2...");

        let output = Command::new(&par2_executable)
            .arg("repair")
            .arg(main_par2)
            .current_dir(download_dir)
            .output()?;

        if output.status.success() {
            spinner.finish_with_message("✅ PAR2 repair completed successfully");

            if self.config.delete_par2_after_repair {
                for par2_file in par2_files {
                    let _ = std::fs::remove_file(&par2_file);
                }
            }
            Ok(true)
        } else {
            spinner.finish_with_message("❌ PAR2 repair failed");
            Ok(true) // Still return true because we attempted repair
        }
    }

    fn check_archive_files_integrity(&self, results: &[DownloadResult], download_dir: &Path) -> Result<Vec<String>> {
        let mut failed_archive_files = Vec::new();

        // Get list of archive files in the download directory
        let archive_files: Vec<PathBuf> = std::fs::read_dir(download_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| self.is_extractable_archive(path))
            .collect();

        // Check if any of these archive files had failed segments during download
        for archive_path in archive_files {
            let filename = archive_path.file_name()
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
                    failed_archive_files.push(filename.to_string());
                }
            }
        }

        Ok(failed_archive_files)
    }

    async fn extract_archives(&self, download_dir: &Path, spinner: &ProgressBar) -> Result<()> {
        let mut extracted_any = false;
        let mut extracted_files = Vec::new();
        let mut failed_archives = Vec::new();
        let mut successful_archives = Vec::new();

        // Get list of files before extraction
        let files_before: std::collections::HashSet<String> = std::fs::read_dir(download_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();

        // Find archive files
        let archive_files: Vec<PathBuf> = std::fs::read_dir(download_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| self.is_extractable_archive(path))
            .collect();

        if archive_files.is_empty() {
            spinner.finish_with_message("ℹ️  No archives found");
            return Ok(());
        }

        for archive_path in archive_files {
            let filename = archive_path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            spinner.set_message(format!("📦 Extracting {}", filename));

            let extension = archive_path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("")
                .to_lowercase();

            let success = match extension.as_str() {
                "rar" | "7z" if self.config.auto_extract_rar => {
                    self.extract_archive(&archive_path, spinner).await?
                }
                "zip" | "tar" | "tgz" | "gz" | "bz2" | "xz" if self.config.auto_extract_zip => {
                    self.extract_archive(&archive_path, spinner).await?
                }
                _ => continue,
            };

            if success {
                extracted_any = true;
                successful_archives.push(filename.to_string());
                if self.config.delete_archives_after_extract {
                    self.delete_archive_and_parts(&archive_path, download_dir)?;
                }
            } else {
                failed_archives.push(filename.to_string());
            }
        }

        // Check what new files were created
        if extracted_any {
            let files_after: std::collections::HashSet<String> = std::fs::read_dir(download_dir)?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect();

            for file in &files_after {
                if !files_before.contains(file) && !file.starts_with('.') {
                    extracted_files.push(file.clone());
                }
            }

            let message = if !extracted_files.is_empty() {
                let file_list = if extracted_files.len() == 1 {
                    extracted_files[0].clone()
                } else {
                    format!("{} files", extracted_files.len())
                };
                let mut msg = format!("✅ Extracted: {}", file_list);
                if !failed_archives.is_empty() {
                    msg.push_str(&format!(" • ❌ {} failed", failed_archives.len()));
                }
                msg
            } else {
                let mut msg = "✅ Archive extraction completed".to_string();
                if !failed_archives.is_empty() {
                    msg.push_str(&format!(" • ❌ {} failed", failed_archives.len()));
                }
                msg
            };

            spinner.finish_with_message(message);
        } else if !failed_archives.is_empty() {
            spinner.finish_with_message(format!("❌ Failed to extract {} archives", failed_archives.len()));
        } else {
            spinner.finish_with_message("ℹ️  No archives found");
        }

        Ok(())
    }

    fn is_extractable_archive(&self, path: &Path) -> bool {
        if let Some(extension) = path.extension().and_then(|ext| ext.to_str()) {
            let ext_lower = extension.to_lowercase();
            match ext_lower.as_str() {
                "rar" => {
                    if !self.config.auto_extract_rar {
                        return false;
                    }
                    // Only extract the first part of multi-part RAR archives
                    let filename = path.file_name().unwrap().to_string_lossy().to_lowercase();
                    filename.contains(".part001.") || filename.contains(".part01.") ||
                    filename.ends_with(".part01.rar") || filename.ends_with(".part001.rar") ||
                    (!filename.contains(".part") && ext_lower == "rar")
                }
                "zip" => self.config.auto_extract_zip,
                "7z" => self.config.auto_extract_rar, // Treat 7z like RAR for config purposes
                "tar" | "tgz" | "gz" | "bz2" | "xz" => self.config.auto_extract_zip, // Treat compressed archives like ZIP for config
                _ => false,
            }
        } else {
            false
        }
    }

    fn delete_archive_and_parts(&self, archive_path: &Path, download_dir: &Path) -> Result<()> {
        let filename = archive_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        // Delete the main archive file
        let _ = std::fs::remove_file(archive_path);

        // For multi-part RAR files, find and delete all parts
        if filename.to_lowercase().contains(".rar") {
            // Extract the base name (everything before .partXX.rar or .rXX)
            let base_name = if let Some(pos) = filename.to_lowercase().find(".part") {
                &filename[..pos]
            } else if let Some(pos) = filename.to_lowercase().rfind(".r") {
                // Handle .r00, .r01, etc. format
                &filename[..pos]
            } else {
                filename
            };

            // Find all related archive parts in the directory
            if let Ok(entries) = std::fs::read_dir(download_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let entry_name = entry.file_name().to_string_lossy().to_lowercase();
                    let base_lower = base_name.to_lowercase();

                    // Check if this file is part of the same archive
                    if entry_name.starts_with(&base_lower) &&
                       (entry_name.contains(".part") || entry_name.contains(".r")) &&
                       (entry_name.ends_with(".rar") || entry_name.matches(".r").count() > 0) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
        // For ZIP files with parts (like .z01, .z02, etc.)
        else if filename.to_lowercase().contains(".zip") || filename.to_lowercase().contains(".z") {
            let base_name = if let Some(pos) = filename.to_lowercase().find(".z") {
                &filename[..pos]
            } else {
                filename
            };

            if let Ok(entries) = std::fs::read_dir(download_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let entry_name = entry.file_name().to_string_lossy().to_lowercase();
                    let base_lower = base_name.to_lowercase();

                    if entry_name.starts_with(&base_lower) &&
                       (entry_name.contains(".zip") || entry_name.matches(".z").count() > 0) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }

        Ok(())
    }

    async fn extract_archive(&self, archive_path: &Path, spinner: &ProgressBar) -> Result<bool> {
        let output_dir = archive_path.parent().unwrap_or(Path::new("."));
        let extension = archive_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();

        match extension.as_str() {
            "rar" => self.extract_rar_archive(archive_path, output_dir, spinner).await,
            "zip" => self.extract_zip_archive(archive_path, output_dir, spinner).await,
            "7z" => self.extract_7z_archive(archive_path, output_dir, spinner).await,
            "tar" | "tgz" | "gz" | "bz2" | "xz" => self.extract_tar_archive(archive_path, output_dir, spinner).await,
            _ => {
                spinner.set_message(format!("⚠️  Unsupported archive format: {}", extension));
                Ok(false)
            }
        }
    }

    async fn extract_rar_archive(&self, archive_path: &Path, output_dir: &Path, spinner: &ProgressBar) -> Result<bool> {
        // First, check if this is actually a valid RAR archive by trying to list it
        spinner.set_message("📦 Checking RAR archive validity...");

        // Try to open for listing first to validate it's a real RAR archive
        match Archive::new(archive_path).open_for_listing() {
            Ok(listing) => {
                // Check if there are any entries in the archive
                let mut has_entries = false;
                let mut entry_count = 0;
                for entry_result in listing {
                    match entry_result {
                        Ok(entry) => {
                            has_entries = true;
                            entry_count += 1;
                            println!("📄 Found in archive: {}", entry.filename.display());
                            if entry_count >= 5 { // Don't list too many files
                                break;
                            }
                        }
                        Err(e) => {
                            eprintln!("Error reading RAR entry: {}", e);
                            return Ok(false);
                        }
                    }
                }

                if !has_entries {
                    eprintln!("RAR archive appears to be empty or invalid");
                    return Ok(false);
                }

                println!("✅ Valid RAR archive with {} entries (showing first 5)", entry_count);
            }
            Err(e) => {
                eprintln!("Not a valid RAR archive {}: {}", archive_path.display(), e);
                return Ok(false);
            }
        }

        // Ensure output directory exists and is writable
        if let Err(e) = std::fs::create_dir_all(output_dir) {
            eprintln!("Failed to create output directory {}: {}", output_dir.display(), e);
            return Ok(false);
        }

        // Now extract the archive
        spinner.set_message("📦 Extracting RAR archive...");
        match Archive::new(archive_path).open_for_processing() {
            Ok(mut archive) => {
                let mut extracted_files = 0;

                loop {
                    match archive.read_header() {
                        Ok(Some(header)) => {
                            let entry = header.entry();
                            let filename = entry.filename.clone();
                            spinner.set_message(format!("📦 Extracting: {}", filename.display()));

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

                                                        // Create the full output path for debugging
                            let output_path = output_dir.join(&filename);
                            println!("🔍 Attempting to extract to: {}", output_path.display());

                            // Check if output directory is writable
                            match std::fs::metadata(output_dir) {
                                Ok(metadata) => {
                                    if metadata.permissions().readonly() {
                                        eprintln!("❌ Output directory is read-only: {}", output_dir.display());
                                        break;
                                    }
                                }
                                Err(e) => {
                                    eprintln!("❌ Cannot access output directory {}: {}", output_dir.display(), e);
                                    break;
                                }
                            }

                            // Ensure parent directory exists for nested files
                            if let Some(parent) = output_path.parent() {
                                if let Err(e) = std::fs::create_dir_all(parent) {
                                    eprintln!("❌ Failed to create parent directory {}: {}", parent.display(), e);
                                    break;
                                }
                            }

                            // Try extract_with_base first (better path handling)
                            match header.extract_with_base(output_dir) {
                                Ok(next_archive) => {
                                    archive = next_archive;
                                    extracted_files += 1;
                                    println!("✅ Extracted: {}", filename.display());
                                }
                                Err(e) => {
                                    eprintln!("❌ Failed to extract {} to {}: {}",
                                             filename.display(),
                                             output_dir.display(),
                                             e);
                                    // Can't continue after extract fails since header is consumed
                                    break;
                                }
                            }
                        }
                        Ok(None) => break, // End of archive
                        Err(e) => {
                            eprintln!("Error reading RAR header: {}", e);
                            break;
                        }
                    }
                }

                Ok(extracted_files > 0)
            }
            Err(e) => {
                eprintln!("Failed to open RAR archive for processing {}: {}", archive_path.display(), e);
                Ok(false)
            }
        }
    }

    async fn extract_zip_archive(&self, archive_path: &Path, output_dir: &Path, spinner: &ProgressBar) -> Result<bool> {
        // Use system unzip command (standard on macOS)
        if let Ok(unzip_path) = which("unzip") {
            spinner.set_message("📦 Extracting ZIP archive...");
            let output = Command::new(unzip_path)
                .arg("-o") // Overwrite files without prompting
                .arg("-q") // Quiet mode
                .arg(archive_path)
                .arg("-d")
                .arg(output_dir)
                .output();

            match output {
                Ok(result) => {
                    if result.status.success() {
                        Ok(true)
                    } else {
                        eprintln!("unzip failed: {}", String::from_utf8_lossy(&result.stderr));
                        Ok(false)
                    }
                }
                Err(e) => {
                    eprintln!("Failed to run unzip: {}", e);
                    Ok(false)
                }
            }
        } else {
            eprintln!("unzip command not found - ZIP extraction not available");
            Ok(false)
        }
    }

    async fn extract_7z_archive(&self, archive_path: &Path, output_dir: &Path, spinner: &ProgressBar) -> Result<bool> {
        // Try 7z command first, then fall back to 7za (p7zip)
        let sevenz_cmd = which("7z").or_else(|_| which("7za"));

        if let Ok(sevenz_path) = sevenz_cmd {
            spinner.set_message("📦 Extracting 7z archive...");
            let output = Command::new(sevenz_path)
                .arg("x") // Extract with full paths
                .arg("-y") // Assume Yes on all queries
                .arg(format!("-o{}", output_dir.display())) // Output directory
                .arg(archive_path)
                .output();

            match output {
                Ok(result) => {
                    if result.status.success() {
                        Ok(true)
                    } else {
                        eprintln!("7z extraction failed: {}", String::from_utf8_lossy(&result.stderr));
                        Ok(false)
                    }
                }
                Err(e) => {
                    eprintln!("Failed to run 7z: {}", e);
                    Ok(false)
                }
            }
        } else {
            eprintln!("7z/7za command not found - install with: brew install p7zip");
            Ok(false)
        }
    }

    async fn extract_tar_archive(&self, archive_path: &Path, output_dir: &Path, spinner: &ProgressBar) -> Result<bool> {
        let extension = archive_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Determine the appropriate tar command based on file extension
        let tar_args = match extension.as_str() {
            "tar" => "xf",
            "tgz" | "gz" => "xzf",
            "bz2" => "xjf",
            "xz" => "xJf",
            _ => "xf", // Default to uncompressed
        };

        if let Ok(tar_path) = which("tar") {
            spinner.set_message(format!("📦 Extracting {} archive...", extension.to_uppercase()));

            let output = Command::new(tar_path)
                .arg(tar_args)
                .arg(archive_path)
                .arg("-C")
                .arg(output_dir)
                .output();

            match output {
                Ok(result) => {
                    if result.status.success() {
                        Ok(true)
                    } else {
                        eprintln!("tar extraction failed: {}", String::from_utf8_lossy(&result.stderr));
                        Ok(false)
                    }
                }
                Err(e) => {
                    eprintln!("Failed to run tar: {}", e);
                    Ok(false)
                }
            }
        } else {
            eprintln!("tar command not found - TAR extraction not available");
            Ok(false)
        }
    }
}
