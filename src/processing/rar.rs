//! RAR archive extraction.
//!
//! The extractor uses the `unrar` crate, which wraps the unrar source. We run
//! the actual extraction on a blocking thread, with a side channel that lets
//! us either receive per-file progress events (for fast files) or poll the
//! output file size (for large files where unrar may not emit progress).

use indicatif::ProgressBar;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
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

pub struct RarExtractor {
    config: PostProcessingConfig,
    large_file_threshold: u64,
}

#[derive(Debug, Default)]
pub struct RarExtractionReport {
    pub archives_extracted: usize,
    pub archives_failed: usize,
}

impl RarExtractor {
    pub fn new(config: PostProcessingConfig, large_file_threshold: u64) -> Self {
        Self {
            config,
            large_file_threshold,
        }
    }

    pub async fn extract_archives(
        &self,
        download_dir: &Path,
        progress_bar: &ProgressBar,
    ) -> Result<RarExtractionReport> {
        progress_bar.set_message("Scanning for RAR archives...");

        let rar_files: Vec<PathBuf> = std::fs::read_dir(download_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| rar_patterns::is_extractable_archive(p))
            .collect();

        if rar_files.is_empty() {
            progress_bar.finish_and_clear();
            return Ok(RarExtractionReport::default());
        }

        let mut plans = Vec::new();
        for rar_path in rar_files {
            match scan_archive(&rar_path) {
                Some(plan) => plans.push(plan),
                None => {
                    tracing::warn!("Failed to scan RAR archive: {}", rar_path.display());
                }
            }
        }

        if plans.is_empty() {
            progress_bar.finish_and_clear();
            return Ok(RarExtractionReport::default());
        }

        let total_bytes: u64 = plans.iter().map(|plan| plan.total_bytes).sum();
        progress_bar.set_length(total_bytes);
        progress_bar.set_position(0);
        progress::apply_style(progress_bar, progress::ProgressStyle::Extract);

        let mut report = RarExtractionReport::default();
        let mut base_offset = 0u64;

        for plan in &plans {
            // Stop starting new archives once an interrupt is requested.
            if crate::shutdown::is_requested() {
                progress_bar.finish_and_clear();
                break;
            }

            let filename = plan
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            progress_bar.set_position(base_offset);
            progress_bar.set_message(format!("Extracting {}", filename));

            match self
                .extract_archive(plan, download_dir, progress_bar, base_offset)
                .await
            {
                Ok(true) => {
                    report.archives_extracted += 1;
                    if self.config.delete_rar_after_extract {
                        if let Err(e) = delete_rar_parts(&plan.path, download_dir) {
                            tracing::warn!("Failed to delete RAR parts for {}: {}", filename, e);
                        }
                    }
                }
                Ok(false) => {
                    report.archives_failed += 1;
                    if !progress_bar.is_hidden() {
                        use crate::ui::{glyph, style};
                        progress_bar.println(format!(
                            "  {}",
                            style::error(&format!(
                                "{} Extraction failed for {}",
                                glyph::ERR,
                                filename
                            ))
                        ));
                    }
                }
                Err(e) => {
                    report.archives_failed += 1;
                    if !progress_bar.is_hidden() {
                        use crate::ui::{glyph, style};
                        progress_bar.println(format!(
                            "  {}",
                            style::error(&format!(
                                "{} Extraction error for {}: {}",
                                glyph::ERR,
                                filename,
                                e
                            ))
                        ));
                    }
                }
            }

            base_offset = base_offset.saturating_add(plan.total_bytes);
        }

        let result_line = if !crate::output_mode::is_quiet() {
            use crate::ui::{glyph, style};
            let plural = |n: usize| if n == 1 { "" } else { "s" };
            Some(
                if report.archives_extracted > 0 && report.archives_failed == 0 {
                    format!(
                        "{}",
                        style::success(&format!(
                            "{} Extracted {} archive{}",
                            glyph::OK,
                            report.archives_extracted,
                            plural(report.archives_extracted)
                        ))
                    )
                } else if report.archives_extracted > 0 {
                    format!(
                        "{}",
                        style::warn(&format!(
                            "{} Extracted {} archive{} ({} failed)",
                            glyph::WARN,
                            report.archives_extracted,
                            plural(report.archives_extracted),
                            report.archives_failed
                        ))
                    )
                } else {
                    format!(
                        "{}",
                        style::error(&format!(
                            "{} Extraction failed for {} archive{}",
                            glyph::ERR,
                            report.archives_failed,
                            plural(report.archives_failed)
                        ))
                    )
                },
            )
        } else {
            None
        };
        crate::ui::finish_clean(progress_bar, result_line);

        Ok(report)
    }

    async fn extract_archive(
        &self,
        plan: &ArchivePlan,
        output_dir: &Path,
        progress_bar: &ProgressBar,
        base_offset: u64,
    ) -> Result<bool> {
        std::fs::create_dir_all(output_dir)?;

        let file_count = plan.file_count;
        let total_bytes = plan.total_bytes;
        progress_bar.set_position(base_offset);

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
                error: Option<String>,
            },
        }

        let (tx, mut rx) = mpsc::channel::<ProgressMsg>(32);
        let archive_path = plan.path.clone();
        let output_dir_owned = output_dir.to_path_buf();
        let large_file_threshold = self.large_file_threshold;
        // unrar runs synchronously; poll the shutdown flag between archive
        // members so a Ctrl+C stops extraction at the next file boundary.
        let cancel = crate::shutdown::handle();

        let _extraction_handle = tokio::task::spawn_blocking(move || {
            let mut bytes_extracted = 0u64;
            let mut extracted_files = 0u64;
            let mut last_error: Option<String> = None;

            let mut archive = match Archive::new(&archive_path).open_for_processing() {
                Ok(a) => a,
                Err(e) => {
                    let _ = tx.blocking_send(ProgressMsg::Done {
                        success: false,
                        error: Some(format!("cannot open: {}", e)),
                    });
                    return;
                }
            };

            loop {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    last_error = Some("cancelled".to_string());
                    break;
                }
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
                                Err(e) => {
                                    last_error = Some(format!("skip header: {}", e));
                                    break;
                                }
                            }
                        }

                        let display = filename.to_string_lossy();
                        let short_name = if display.len() > 30 {
                            let s = display.as_ref();
                            let start = s.char_indices().rev().nth(26).map(|(i, _)| i).unwrap_or(0);
                            format!("...{}", &s[start..])
                        } else {
                            display.to_string()
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
                                Err(e) => {
                                    last_error = Some(format!("skip empty: {}", e));
                                    break;
                                }
                            }
                        }

                        let output_path = output_dir_owned.join(&safe_filename);
                        if let Some(parent) = output_path.parent() {
                            if let Err(e) = std::fs::create_dir_all(parent) {
                                last_error = Some(format!("mkdir: {}", e));
                                break;
                            }
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
                            Err(e) => {
                                last_error = Some(format!("extract {}: {}", display, e));
                                break;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        last_error = Some(format!("read header: {}", e));
                        break;
                    }
                }
            }

            let success = extracted_files > 0 && last_error.is_none();
            let _ = tx.blocking_send(ProgressMsg::Done {
                success,
                error: last_error,
            });
        });

        let mut current_monitor: Option<(PathBuf, u64)> = None;
        let mut success = false;
        let mut error_msg: Option<String> = None;

        loop {
            if let Some((path, base_bytes)) = current_monitor.as_ref().cloned() {
                tokio::select! {
                    msg = rx.recv() => match msg {
                        Some(ProgressMsg::StartFile { name, index, total }) => {
                            progress_bar.set_message(format!("Extracting {} [{}/{}]", name, index, total));
                        }
                        Some(ProgressMsg::FileComplete { bytes }) => {
                            progress_bar.set_position(base_offset + bytes);
                            current_monitor = None;
                        }
                        Some(ProgressMsg::MonitorFile { path: new_path, base_bytes: new_base }) => {
                            current_monitor = Some((new_path, new_base));
                        }
                        Some(ProgressMsg::Done { success: s, error: err }) => {
                            success = s;
                            error_msg = err;
                            break;
                        }
                        None => break,
                    },
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {
                        if let Ok(meta) = std::fs::metadata(&path) {
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
                    Some(ProgressMsg::Done {
                        success: s,
                        error: err,
                    }) => {
                        success = s;
                        error_msg = err;
                        break;
                    }
                    None => break,
                }
            }
        }

        progress_bar.set_position(base_offset + total_bytes);
        if let Some(err) = error_msg {
            tracing::warn!("RAR extraction error: {}", err);
        }
        Ok(success)
    }
}

fn scan_archive(path: &Path) -> Option<ArchivePlan> {
    let listing = Archive::new(path).open_for_listing().ok()?;
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
                if let Err(e) = std::fs::remove_file(entry.path()) {
                    tracing::warn!("Failed to delete {}: {}", entry.path().display(), e);
                }
            }
        }
    }
    Ok(())
}
