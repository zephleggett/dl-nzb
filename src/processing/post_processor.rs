//! Post-processing orchestration for downloaded files
//!
//! Coordinates PAR2 verification/repair, RAR extraction, and deobfuscation.

use indicatif::ProgressBar;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::par2::{self, Par2Status};
use super::rar::{self, RarExtractor};
use crate::config::PostProcessingConfig;
use crate::download::DownloadResult;
use crate::error::DlNzbError;
use crate::patterns::par2 as par2_patterns;

type Result<T> = std::result::Result<T, DlNzbError>;

pub struct PostProcessor {
    config: PostProcessingConfig,
    large_file_threshold: u64,
}

#[derive(Debug, Clone)]
pub struct PostProcessingOutcome {
    pub par2_status: Par2Status,
    pub rar_extracted: bool,
    pub files_renamed: usize,
    pub extensions_fixed: usize,
}

impl PostProcessor {
    pub fn new(config: PostProcessingConfig, large_file_threshold: u64) -> Self {
        Self {
            config,
            large_file_threshold,
        }
    }

    pub async fn process_downloads(
        &self,
        results: &[DownloadResult],
    ) -> Result<PostProcessingOutcome> {
        if results.is_empty() {
            return Ok(PostProcessingOutcome {
                par2_status: Par2Status::NoPar2Files,
                rar_extracted: false,
                files_renamed: 0,
                extensions_fixed: 0,
            });
        }

        let download_dir = results[0].path.parent().unwrap_or(Path::new("."));

        // Collect PAR2 files from download results
        let downloaded_par2_files: Vec<PathBuf> = results
            .iter()
            .filter(|r| par2_patterns::is_par2_file(&r.path))
            .map(|r| r.path.clone())
            .collect();

        let useful_name = download_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download");

        // Run PAR2 repair if configured
        let par2_status = if self.config.auto_par2_repair {
            let bar = ProgressBar::new(100);
            bar.enable_steady_tick(Duration::from_millis(100));

            par2::repair_with_par2(&self.config, download_dir, &downloaded_par2_files, &bar).await?
        } else {
            Par2Status::NoPar2Files
        };

        // Check archive integrity
        let archive_files_with_failures = self.check_archive_integrity(results, download_dir)?;

        // Extract RAR archives only if safe
        let should_extract = self.config.auto_extract_rar
            && ((archive_files_with_failures.is_empty() && par2_status == Par2Status::NoPar2Files)
                || par2_status == Par2Status::Success);

        let mut rar_extracted = false;
        if should_extract {
            let bar = ProgressBar::new(100);
            bar.enable_steady_tick(Duration::from_millis(100));

            let extractor = RarExtractor::new(self.config.clone(), self.large_file_threshold);
            let extracted_count = extractor.extract_archives(download_dir, &bar).await?;
            rar_extracted = extracted_count > 0;
        }

        // Deobfuscate file names if configured
        let mut files_renamed = 0;
        let mut extensions_fixed = 0;
        if self.config.deobfuscate_file_names {
            let result = self.run_deobfuscation(download_dir, useful_name)?;
            files_renamed = result.files_renamed;
            extensions_fixed = result.extensions_fixed;
        }

        Ok(PostProcessingOutcome {
            par2_status,
            rar_extracted,
            files_renamed,
            extensions_fixed,
        })
    }

    /// Check if any RAR files have failed segments
    fn check_archive_integrity(
        &self,
        results: &[DownloadResult],
        download_dir: &Path,
    ) -> Result<Vec<String>> {
        let mut failed_rar_files = Vec::new();

        let rar_files: Vec<PathBuf> = std::fs::read_dir(download_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| rar::is_rar_archive(path))
            .collect();

        for rar_path in rar_files {
            let filename = rar_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            if let Some(result) = results.iter().find(|r| {
                r.path
                    .file_name()
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

    /// Run deobfuscation on extracted files
    fn run_deobfuscation(
        &self,
        download_dir: &Path,
        useful_name: &str,
    ) -> Result<super::deobfuscate::DeobfuscateResult> {
        use indicatif::ProgressStyle as IndicatifStyle;

        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            IndicatifStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        spinner.enable_steady_tick(Duration::from_millis(80));
        spinner.set_message("Deobfuscating...");

        match super::deobfuscate::deobfuscate_files(download_dir, useful_name) {
            Ok(result) => {
                if result.files_renamed > 0 || result.extensions_fixed > 0 {
                    let mut msg = Vec::new();
                    if result.extensions_fixed > 0 {
                        msg.push(format!("{} ext", result.extensions_fixed));
                    }
                    if result.files_renamed > 0 {
                        msg.push(format!("{} renamed", result.files_renamed));
                    }
                    spinner.finish_and_clear();
                    println!("  \x1b[36m✓ Deobfuscated ({})\x1b[0m", msg.join(", "));
                } else {
                    spinner.finish_and_clear();
                }
                Ok(result)
            }
            Err(e) => {
                tracing::debug!("Deobfuscation failed: {}", e);
                spinner.finish_and_clear();
                Ok(super::deobfuscate::DeobfuscateResult {
                    files_renamed: 0,
                    extensions_fixed: 0,
                })
            }
        }
    }
}
