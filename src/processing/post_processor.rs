//! Post-processing: coordinate PAR2 verify/repair, RAR extraction, and deobfuscation.
//!
//! The orchestration is deliberately conservative:
//! 1. If PAR2 fails, we do NOT extract — corrupt RAR data could write garbage.
//! 2. If RAR extraction starts and any segment in the RAR set failed to download,
//!    we still attempt extraction only when PAR2 succeeded (which would have
//!    repaired the damage).
//! 3. Deobfuscation only runs once everything else has settled.

use std::path::{Path, PathBuf};

use super::par2::{self, Par2Status};
use super::rar::RarExtractor;
use crate::config::PostProcessingConfig;
use crate::download::DownloadResult;
use crate::error::DlNzbError;
use crate::patterns::par2 as par2_patterns;
use crate::patterns::rar as rar_patterns;

type Result<T> = std::result::Result<T, DlNzbError>;

pub struct PostProcessor {
    config: PostProcessingConfig,
    large_file_threshold: u64,
}

#[derive(Debug, Clone)]
pub struct PostProcessingOutcome {
    pub par2_status: Par2Status,
    pub rar_extracted: bool,
    pub rar_archives_failed: usize,
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
                rar_archives_failed: 0,
                files_renamed: 0,
                extensions_fixed: 0,
            });
        }

        let download_dir = results[0].path.parent().unwrap_or(Path::new("."));

        let downloaded_par2_files: Vec<PathBuf> = results
            .iter()
            .filter(|r| par2_patterns::is_par2_file(&r.path))
            .map(|r| r.path.clone())
            .collect();

        let useful_name = download_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download");

        // Authoritative PAR2-driven name recovery BEFORE repair: identifies each
        // obfuscated file by its first-16k MD5 against the PAR2 file table and
        // renames it to the real name. Running here (not in the post-repair
        // heuristic pass) means the repairer matches by name and any
        // `delete_par2_after_repair` purge can't remove the par2 first.
        let mut par2_renamed = 0usize;
        if self.config.deobfuscate_file_names
            && !downloaded_par2_files.is_empty()
            && !crate::shutdown::is_requested()
        {
            match super::deobfuscate::recover_par2_names(download_dir, &downloaded_par2_files) {
                Ok(rec) => {
                    par2_renamed = rec.files_renamed;
                    if par2_renamed > 0 && !crate::output_mode::is_quiet() {
                        println!(
                            "  \x1b[36m✓ Recovered {} name{} from PAR2\x1b[0m",
                            par2_renamed,
                            if par2_renamed == 1 { "" } else { "s" }
                        );
                    }
                }
                Err(e) => tracing::debug!("PAR2 name recovery failed: {}", e),
            }
        }

        let par2_status = if self.config.auto_par2_repair && !crate::shutdown::is_requested() {
            let bar =
                crate::progress::create_progress_bar(100, crate::progress::ProgressStyle::Par2);
            par2::repair_with_par2(&self.config, download_dir, &downloaded_par2_files, &bar).await?
        } else {
            Par2Status::NoPar2Files
        };

        let archive_files_with_failures = self.check_archive_integrity(results);

        let safe_to_extract = match par2_status {
            Par2Status::Success => true,
            Par2Status::Failed => false,
            Par2Status::NoPar2Files => archive_files_with_failures.is_empty(),
        };

        let mut rar_extracted = false;
        let mut rar_failed = 0usize;
        if self.config.auto_extract_rar && safe_to_extract && !crate::shutdown::is_requested() {
            let bar =
                crate::progress::create_progress_bar(100, crate::progress::ProgressStyle::Par2);
            let extractor = RarExtractor::new(self.config.clone(), self.large_file_threshold);
            let report = extractor.extract_archives(download_dir, &bar).await?;
            rar_extracted = report.archives_extracted > 0;
            rar_failed = report.archives_failed;
        } else if self.config.auto_extract_rar && !archive_files_with_failures.is_empty() {
            if !crate::output_mode::is_quiet() {
                println!(
                    "  \x1b[33m⚠ Skipping RAR extraction — {} archive{} have download failures and PAR2 did not succeed\x1b[0m",
                    archive_files_with_failures.len(),
                    if archive_files_with_failures.len() == 1 { "" } else { "s" }
                );
            }
        } else if self.config.auto_extract_rar
            && par2_status == Par2Status::Failed
            && !crate::output_mode::is_quiet()
        {
            println!("  \x1b[33m⚠ Skipping RAR extraction — PAR2 verification failed\x1b[0m");
        }

        let (files_renamed, extensions_fixed) =
            if self.config.deobfuscate_file_names && !crate::shutdown::is_requested() {
                let result = self.run_deobfuscation(download_dir, useful_name)?;
                (result.files_renamed, result.extensions_fixed)
            } else {
                (0, 0)
            };

        Ok(PostProcessingOutcome {
            par2_status,
            rar_extracted,
            rar_archives_failed: rar_failed,
            files_renamed: files_renamed + par2_renamed,
            extensions_fixed,
        })
    }

    fn check_archive_integrity(&self, results: &[DownloadResult]) -> Vec<String> {
        results
            .iter()
            .filter(|r| r.segments_failed > 0 && rar_patterns::is_extractable_archive(&r.path))
            .filter_map(|r| {
                r.path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
            })
            .collect()
    }

    fn run_deobfuscation(
        &self,
        download_dir: &Path,
        useful_name: &str,
    ) -> Result<super::deobfuscate::DeobfuscateResult> {
        let spinner = crate::progress::create_spinner("Deobfuscating...");

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
                    if !crate::output_mode::is_quiet() {
                        println!("  \x1b[36m✓ Deobfuscated ({})\x1b[0m", msg.join(", "));
                    }
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
