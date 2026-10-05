//! Post-processing: coordinate PAR2 verify/repair, RAR extraction, and deobfuscation.
//!
//! The orchestration is deliberately conservative:
//! 1. If PAR2 fails, we do NOT extract — corrupt RAR data could write garbage.
//! 2. If RAR extraction starts and any segment in the RAR set failed to download,
//!    we still attempt extraction only when PAR2 succeeded (which would have
//!    repaired the damage).
//! 3. Deobfuscation only runs once everything else has settled.
//!
//! For an engine job with a resume record, PAR2's verdict is saved as soon as
//! PAR2 settles (before extraction, which may take long and be stopped), so a
//! resumed job doesn't need the PAR2 files `delete_par2_after_repair` deleted.
//!
//! Everything happens inside the job folder passed in: no step reads or writes
//! outside it. Phases and progress go to the job context; nothing is printed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::par2::{self, Par2Result, Par2Status};
use super::rar::RarExtractor;
use crate::config::PostProcessingConfig;
use crate::download::DownloadResult;
use crate::engine::context::JobCtx;
use crate::engine::sidecar::JobRecord;
use crate::engine::JobPhase;
use crate::error::DlNzbError;
use crate::patterns::par2 as par2_patterns;
use crate::patterns::rar as rar_patterns;

type Result<T> = std::result::Result<T, DlNzbError>;

pub struct PostProcessor {
    config: PostProcessingConfig,
    /// The user's passwords, tried first.
    passwords: Vec<String>,
    /// The NZB's own passwords, tried after the user's.
    nzb_passwords: Vec<String>,
    job: Arc<JobCtx>,
    /// The job's resume record, which keeps PAR2's verdict and the archives
    /// extracted.
    record: Option<Arc<JobRecord>>,
    /// PAR2 verified these files in an earlier run and none has changed.
    par2_verified_earlier: bool,
    /// What renaming calls the main file (made safe for a file name); `None`
    /// uses the job folder's name.
    name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PostProcessingOutcome {
    pub par2_status: Par2Status,
    /// PAR2 verification actually ran (false when skipped).
    pub par2_ran: bool,
    pub par2_damaged_blocks: u64,
    pub par2_repaired_blocks: u64,
    pub rar_archives_extracted: usize,
    pub rar_archives_failed: usize,
    /// Archives left unextracted because they are encrypted and no password
    /// worked.
    pub rar_archives_encrypted: usize,
    /// One of those archives refused passwords the user supplied (as opposed
    /// to having none, or only the NZB's, to try).
    pub rar_user_passwords_failed: bool,
    /// First volumes of the archives that extracted.
    pub extracted_archives: Vec<PathBuf>,
    pub files_renamed: usize,
}

impl PostProcessor {
    pub fn new(config: PostProcessingConfig) -> Self {
        Self {
            config,
            passwords: Vec::new(),
            nzb_passwords: Vec::new(),
            job: JobCtx::detached(),
            record: None,
            par2_verified_earlier: false,
            name: None,
        }
    }

    /// The job's name: renaming (`deobfuscate_file_names`) calls an
    /// obfuscated main file after it, made safe for a file name, instead of
    /// after the job folder (which the app may have de-duplicated: "Name 2").
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = super::deobfuscate::name_from_title(name);
        self
    }

    /// Work for an engine job: report phases/progress to it and honour its stop.
    pub(crate) fn with_job(mut self, job: Arc<JobCtx>) -> Self {
        self.job = job;
        self
    }

    /// The job's resume record: PAR2's verdict is saved in it once PAR2
    /// settles, and each archive once extracted (which a resumed job then
    /// skips).
    pub(crate) fn with_record(mut self, record: Option<Arc<JobRecord>>) -> Self {
        self.record = record;
        self
    }

    /// PAR2 verified the files in an earlier run and none has changed since:
    /// it counts as verified without running (its files may be gone).
    pub(crate) fn with_par2_verified_earlier(mut self, verified: bool) -> Self {
        self.par2_verified_earlier = verified;
        self
    }

    /// The user's passwords for encrypted archives, tried first and in order
    /// (newest first). When they all fail the outcome says so
    /// ([`PostProcessingOutcome::rar_user_passwords_failed`]).
    pub fn with_passwords(mut self, passwords: Vec<String>) -> Self {
        self.passwords = passwords;
        self
    }

    /// The NZB's own passwords (`<meta type="password">`, `{{password}}` in
    /// its title or file name), tried after the user's.
    pub fn with_nzb_passwords(mut self, passwords: Vec<String>) -> Self {
        self.nzb_passwords = passwords;
        self
    }

    /// Post-process the job folder `download_dir`, whose downloaded files are
    /// described by `results`.
    pub async fn process(
        &self,
        download_dir: &Path,
        results: &[DownloadResult],
    ) -> Result<PostProcessingOutcome> {
        let job = &self.job;
        let mut outcome = PostProcessingOutcome {
            par2_status: Par2Status::NoPar2Files,
            par2_ran: false,
            par2_damaged_blocks: 0,
            par2_repaired_blocks: 0,
            rar_archives_extracted: 0,
            rar_archives_failed: 0,
            rar_archives_encrypted: 0,
            rar_user_passwords_failed: false,
            extracted_archives: Vec::new(),
            files_renamed: 0,
        };
        if results.is_empty() {
            return Ok(outcome);
        }

        // Only PAR2 files still there: a resumed job's results also name those
        // `delete_par2_after_repair` deleted in an earlier run.
        let downloaded_par2_files: Vec<PathBuf> = results
            .iter()
            .filter(|r| {
                par2_patterns::is_par2_file(&r.path)
                    && r.path.starts_with(download_dir)
                    && r.path.is_file()
            })
            .map(|r| r.path.clone())
            .collect();

        let useful_name = self.name.as_deref().unwrap_or_else(|| {
            download_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("download")
        });

        // Authoritative PAR2-driven name recovery BEFORE repair: identifies each
        // obfuscated file by its first-16k MD5 against the PAR2 file table and
        // renames it to the real name. Running here (not in the post-repair
        // heuristic pass) means the repairer matches by name and any
        // `delete_par2_after_repair` purge can't remove the par2 first. Quick
        // (16 KiB per file), so it gets no phase of its own.
        let mut par2_renamed = 0usize;
        if self.config.deobfuscate_file_names
            && !downloaded_par2_files.is_empty()
            && !job.is_cancelled()
        {
            match super::deobfuscate::recover_par2_names(
                download_dir,
                &downloaded_par2_files,
                &mut |from, to| self.renamed(download_dir, from, to),
            ) {
                Ok(rec) => par2_renamed = rec.files_renamed,
                Err(e) => tracing::debug!("PAR2 name recovery failed: {}", e),
            }
        }

        // The download is fully integrity-verified on the wire when every segment
        // of every file carried a yEnc checksum that matched and nothing failed.
        // In that case the payload is provably intact, so we skip PAR2's
        // full-payload re-hash (minutes on a large release) — PAR2 is only needed
        // to *repair*, and there is nothing to repair. Any file with a
        // checksum-less segment, or any failure, falls through to a real verify.
        let download_clean = results
            .iter()
            .all(|r| r.segments_failed == 0 && r.all_segments_crc_verified);

        if self.config.auto_par2_repair && !job.is_cancelled() {
            if self.par2_verified_earlier || (download_clean && !downloaded_par2_files.is_empty()) {
                outcome.par2_status = Par2Status::Success;
            } else if !downloaded_par2_files.is_empty() {
                let Par2Result {
                    status,
                    damaged_blocks,
                    repaired_blocks,
                    error,
                } = par2::repair_with_par2(&self.config, &downloaded_par2_files, job).await?;
                outcome.par2_status = status;
                outcome.par2_ran = true;
                outcome.par2_damaged_blocks = damaged_blocks;
                outcome.par2_repaired_blocks = repaired_blocks;
                if let Some(error) = error {
                    tracing::debug!("PAR2: {}", error);
                }
            }
            if !self.par2_verified_earlier {
                self.save_par2_verdict(download_dir, outcome.par2_status)
                    .await;
            }
        }

        // Without PAR2 to repair them, archives with download failures would
        // extract garbage.
        let safe_to_extract = match outcome.par2_status {
            Par2Status::Success => true,
            Par2Status::Failed => false,
            Par2Status::NoPar2Files => !results
                .iter()
                .any(|r| r.segments_failed > 0 && rar_patterns::is_extractable_archive(&r.path)),
        };

        if self.config.auto_extract_rar && safe_to_extract && !job.is_cancelled() {
            let extractor = RarExtractor::new(self.config.clone())
                .with_passwords(&self.passwords, &self.nzb_passwords)
                .with_record(self.record.clone());
            let report = extractor.extract_archives(download_dir, job).await?;
            outcome.rar_archives_extracted = report.archives_extracted;
            outcome.rar_archives_failed = report.archives_failed;
            outcome.rar_archives_encrypted = report.archives_encrypted;
            outcome.rar_user_passwords_failed = report.user_passwords_failed;
            outcome.extracted_archives = report.extracted;
        }

        // An archive still waiting for its password stays exactly as
        // downloaded: renaming one volume (deobfuscation renames the biggest
        // file) would break the set for `reprocess`, which renames afterwards.
        let awaiting_password = outcome.rar_archives_encrypted > 0;
        if self.config.deobfuscate_file_names && !job.is_cancelled() && !awaiting_password {
            job.set_phase(JobPhase::Renaming);
            outcome.files_renamed = self.run_deobfuscation(download_dir, useful_name);
            job.set_fraction(1.0);
        }
        outcome.files_renamed += par2_renamed;

        Ok(outcome)
    }

    /// Save PAR2's verdict in the job's record right away (a stop during the
    /// extraction that follows must not lose it). Nothing is saved when PAR2
    /// had nothing to say. A success is saved even when the job was stopped
    /// meanwhile: par2-rs checks for a stop before it deletes its files
    /// (`delete_par2_after_repair`), so a success is real, and once those
    /// files are gone the saved verdict is all a resumed job has to go on. A
    /// failure after a stop is not saved: it may only mean PAR2 was stopped.
    async fn save_par2_verdict(&self, download_dir: &Path, status: Par2Status) {
        let Some(record) = &self.record else {
            return;
        };
        let stopped_short = status == Par2Status::Failed && self.job.is_cancelled();
        if status == Par2Status::NoPar2Files || stopped_short {
            return;
        }
        let verified = status == Par2Status::Success;
        let (stamped, dir) = (record.clone(), download_dir.to_path_buf());
        let _ =
            tokio::task::spawn_blocking(move || stamped.set_par2_verified(&dir, verified)).await;
        if let Err(e) = record.save().await {
            tracing::debug!("could not save resume data: {e}");
        }
    }

    /// A file of the job folder was renamed: PAR2's verdict in the job's
    /// record follows it to its new name, and is saved right away (the app
    /// may be suspended or quit at any moment). Otherwise a resumed job
    /// would find the verified file gone, and with the PAR2 files deleted
    /// after the repair it could never verify it again.
    fn renamed(&self, dir: &Path, from: &Path, to: &Path) {
        let Some(record) = &self.record else {
            return;
        };
        let name = |p: &Path| p.file_name().and_then(|n| n.to_str()).map(str::to_owned);
        let (Some(from), Some(to)) = (name(from), name(to)) else {
            return;
        };
        if record.renamed(dir, &from, &to) {
            if let Err(e) = record.save_blocking() {
                tracing::debug!("could not save resume data: {e}");
            }
        }
    }

    /// Files renamed by the heuristic deobfuscation pass.
    fn run_deobfuscation(&self, download_dir: &Path, useful_name: &str) -> usize {
        match super::deobfuscate::deobfuscate_files(download_dir, useful_name, &mut |from, to| {
            self.renamed(download_dir, from, to)
        }) {
            Ok(renamed) => renamed,
            Err(e) => {
                tracing::debug!("Deobfuscation failed: {}", e);
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::Nzb;

    fn nzb() -> Nzb {
        r#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><file poster="p" date="1700000000" subject="&quot;a.rar&quot; yEnc (1/1)"><groups><group>a.b</group></groups><segments><segment bytes="100" number="1">x1@t</segment></segments></file></nzb>"#
            .parse()
            .unwrap()
    }

    fn processor(record: &Arc<JobRecord>, job: &Arc<JobCtx>) -> PostProcessor {
        PostProcessor::new(PostProcessingConfig::default())
            .with_job(job.clone())
            .with_record(Some(record.clone()))
    }

    /// A stop that lands after PAR2 succeeded (and deleted its files) must
    /// not lose the verdict: a resumed job has nothing else to go on.
    #[tokio::test]
    async fn a_par2_success_is_saved_even_after_a_stop() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rar"), b"repaired").unwrap();
        let (record, _) = JobRecord::open(dir.path(), &nzb(), false);
        let job = JobCtx::detached();
        job.cancel();

        processor(&record, &job)
            .save_par2_verdict(dir.path(), Par2Status::Success)
            .await;
        assert!(record.par2_still_verified(dir.path()));
        let (again, _) = JobRecord::open(dir.path(), &nzb(), false);
        assert!(again.par2_still_verified(dir.path()), "not saved to disk");
    }

    /// A failure after a stop may only mean PAR2 was stopped: an earlier
    /// verdict stands.
    #[tokio::test]
    async fn a_par2_failure_after_a_stop_is_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rar"), b"data").unwrap();
        let (record, _) = JobRecord::open(dir.path(), &nzb(), false);
        record.set_par2_verified(dir.path(), true);
        let job = JobCtx::detached();
        job.cancel();

        processor(&record, &job)
            .save_par2_verdict(dir.path(), Par2Status::Failed)
            .await;
        assert!(record.par2_still_verified(dir.path()));
    }
}
