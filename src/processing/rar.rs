//! RAR archive extraction functionality

use indicatif::ProgressBar;
use std::path::{Path, PathBuf};
use std::time::Duration;
use unrar::Archive;

use crate::config::PostProcessingConfig;
use crate::error::DlNzbError;
use crate::patterns::rar as rar_patterns;
use crate::progress;

type Result<T> = std::result::Result<T, DlNzbError>;

struct ArchivePlan {
    path: PathBuf,
    file_count: u64,
    total_bytes: u64,
}

/// RAR extraction configuration
pub struct RarExtractor {
    config: PostProcessingConfig,
    large_file_threshold: u64,
}

impl RarExtractor {
    pub fn new(config: PostProcessingConfig, large_file_threshold: u64) -> Self {
        Self {
            config,
            large_file_threshold,
        }
    }

    /// Extract all RAR archives in the directory
    pub async fn extract_archives(
        &self,
        download_dir: &Path,
        progress_bar: &ProgressBar,
    ) -> Result<usize> {
        progress_bar.set_message("Scanning for RAR archives...");

        let rar_files: Vec<PathBuf> = std::fs::read_dir(download_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| is_rar_archive(path))
            .collect();

        if rar_files.is_empty() {
            progress_bar.finish_and_clear();
            return Ok(0);
        }

        let mut plans = Vec::new();
        for rar_path in rar_files {
            if let Some(plan) = scan_archive(&rar_path) {
                plans.push(plan);
            }
        }

        if plans.is_empty() {
            progress_bar.finish_and_clear();
            return Ok(0);
        }

        let total_bytes: u64 = plans.iter().map(|plan| plan.total_bytes).sum();
        progress_bar.set_length(total_bytes);
        progress::apply_style(progress_bar, progress::ProgressStyle::Extract);

        let mut extracted_count = 0;
        let mut base_offset = 0u64;

        for plan in &plans {
            let filename = plan
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            progress_bar.set_position(base_offset);
            progress_bar.set_message(format!("Extracting {}", filename));

            if self
                .extract_archive(plan, download_dir, progress_bar, base_offset)
                .await?
            {
                extracted_count += 1;
                if self.config.delete_rar_after_extract {
                    delete_rar_parts(&plan.path, download_dir)?;
                }
            }

            base_offset = base_offset.saturating_add(plan.total_bytes);
        }

        progress_bar.set_position(total_bytes);
        progress_bar.finish_with_message("  ");
        println!(
            "  └─ \x1b[32m✓ Extracted {} archive{}\x1b[0m",
            extracted_count,
            if extracted_count == 1 { "" } else { "s" }
        );
        Ok(extracted_count)
    }

    /// Extract a single RAR archive with progress tracking
    async fn extract_archive(
        &self,
        plan: &ArchivePlan,
        output_dir: &Path,
        progress_bar: &ProgressBar,
        base_offset: u64,
    ) -> Result<bool> {
        use tokio::sync::mpsc;
        let file_count = plan.file_count;
        let total_bytes = plan.total_bytes;
        progress_bar.set_position(base_offset);

        std::fs::create_dir_all(output_dir)?;

        enum ProgressMsg {
            StartFile {
                name: String,
                index: u64,
                total: u64,
            },
            FileComplete {
                bytes: u64,
            },
            MonitorFile {
                path: PathBuf,
                base_bytes: u64,
            },
            Done {
                success: bool,
            },
        }

        let (tx, mut rx) = mpsc::channel::<ProgressMsg>(32);
        let archive_path = plan.path.clone();
        let output_dir = output_dir.to_path_buf();
        let large_file_threshold = self.large_file_threshold;

        let extraction_handle = tokio::task::spawn_blocking(move || {
            let mut bytes_extracted = 0u64;
            let mut extracted_files = 0u64;

            let mut archive = match Archive::new(&archive_path).open_for_processing() {
                Ok(a) => a,
                Err(_) => {
                    let _ = tx.blocking_send(ProgressMsg::Done { success: false });
                    return;
                }
            };

            loop {
                match archive.read_header() {
                    Ok(Some(header)) => {
                        let entry = header.entry();
                        let filename = entry.filename.clone();
                        let file_size = entry.unpacked_size;

                        if entry.is_directory() {
                            match header.skip() {
                                Ok(next) => {
                                    archive = next;
                                    continue;
                                }
                                Err(_) => break,
                            }
                        }

                        let file_display = filename.to_string_lossy();
                        let short_name = if file_display.len() > 30 {
                            format!("...{}", &file_display[file_display.len() - 27..])
                        } else {
                            file_display.to_string()
                        };
                        let _ = tx.blocking_send(ProgressMsg::StartFile {
                            name: short_name,
                            index: extracted_files + 1,
                            total: file_count,
                        });

                        let safe_filename: PathBuf = filename
                            .components()
                            .filter(|c| matches!(c, std::path::Component::Normal(_)))
                            .collect();

                        if safe_filename.as_os_str().is_empty() {
                            match header.skip() {
                                Ok(next) => {
                                    archive = next;
                                    continue;
                                }
                                Err(_) => break,
                            }
                        }

                        let output_path = output_dir.join(&safe_filename);
                        if let Some(parent) = output_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }

                        if file_size > large_file_threshold {
                            let _ = tx.blocking_send(ProgressMsg::MonitorFile {
                                path: output_path.clone(),
                                base_bytes: bytes_extracted,
                            });
                        }

                        match header.extract_to(&output_path) {
                            Ok(next) => {
                                archive = next;
                                bytes_extracted += file_size;
                                extracted_files += 1;
                                let _ = tx.blocking_send(ProgressMsg::FileComplete {
                                    bytes: bytes_extracted,
                                });
                            }
                            Err(_) => break,
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }

            let _ = tx.blocking_send(ProgressMsg::Done {
                success: extracted_files > 0,
            });
        });

        let mut current_monitor: Option<(PathBuf, u64)> = None;
        let mut result = false;

        loop {
            if let Some((ref path, base_bytes)) = current_monitor {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some(ProgressMsg::StartFile { name, index, total }) => {
                                progress_bar.set_message(format!("Extracting {} [{}/{}]", name, index, total));
                            }
                            Some(ProgressMsg::FileComplete { bytes }) => {
                                progress_bar.set_position(base_offset + bytes);
                                current_monitor = None;
                            }
                            Some(ProgressMsg::MonitorFile { path, base_bytes }) => {
                                current_monitor = Some((path, base_bytes));
                            }
                            Some(ProgressMsg::Done { success }) => {
                                result = success;
                                break;
                            }
                            None => break,
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {
                        if let Ok(meta) = std::fs::metadata(path) {
                            progress_bar.set_position(base_offset + base_bytes + meta.len());
                        }
                    }
                }
            } else {
                match rx.recv().await {
                    Some(ProgressMsg::StartFile { name, index, total }) => {
                        progress_bar
                            .set_message(format!("Extracting {} [{}/{}]", name, index, total));
                    }
                    Some(ProgressMsg::FileComplete { bytes }) => {
                        progress_bar.set_position(base_offset + bytes);
                    }
                    Some(ProgressMsg::MonitorFile { path, base_bytes }) => {
                        current_monitor = Some((path, base_bytes));
                    }
                    Some(ProgressMsg::Done { success }) => {
                        result = success;
                        break;
                    }
                    None => break,
                }
            }
        }

        let _ = extraction_handle.await;
        progress_bar.set_position(base_offset + total_bytes);

        Ok(result)
    }
}

fn scan_archive(path: &Path) -> Option<ArchivePlan> {
    match Archive::new(path).open_for_listing() {
        Ok(listing) => {
            let mut count = 0u64;
            let mut bytes = 0u64;

            for entry_result in listing {
                match entry_result {
                    Ok(entry) => {
                        if !entry.is_directory() {
                            count += 1;
                            bytes += entry.unpacked_size;
                        }
                    }
                    Err(_) => return None,
                }
            }

            if count == 0 {
                return None;
            }

            Some(ArchivePlan {
                path: path.to_path_buf(),
                file_count: count,
                total_bytes: bytes,
            })
        }
        Err(_) => None,
    }
}

/// Check if a path is a RAR archive (first part only for multi-part)
pub fn is_rar_archive(path: &Path) -> bool {
    rar_patterns::is_extractable_archive(path)
}

/// Delete all parts of a RAR archive
fn delete_rar_parts(rar_path: &Path, download_dir: &Path) -> Result<()> {
    let filename = match rar_path.file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => return Ok(()),
    };

    let base_name = rar_patterns::extract_base_name(filename).unwrap_or(filename);

    if let Ok(entries) = std::fs::read_dir(download_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let entry_name = entry.file_name().to_string_lossy().to_string();
            if rar_patterns::is_same_archive(base_name, &entry_name) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    Ok(())
}
