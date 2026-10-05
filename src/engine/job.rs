//! Job orchestration: one NZB from parse to finished folder (moved out of the
//! CLI so the app and the CLI share one code path), and post-processing-only
//! reprocessing.
//!
//! Order: validate, parse, create the folder, open the resume record, check
//! free space, connect, optional availability scan (with the unrepairable
//! policy), download (data first, recovery on demand), then PAR2, extraction
//! and renaming. A stop at any point finishes the job as `Stopped` with the
//! data and the resume sidecar kept, and `start()` on the folder continues
//! where it was (see [`super::sidecar`]): finished articles and files are not
//! fetched again, finished phases are skipped, a PAR2 verification that
//! passed on unchanged files is not repeated, and archives already extracted
//! are not extracted again. Post-processing only ever starts once every
//! download phase has finished: it renames and deletes files, which a
//! download still to be resumed must find where it left them.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use super::context::JobCtx;
use super::disk;
use super::sidecar::{JobRecord, Opened};
use super::types::{
    AvailabilityInfo, FileReport, JobPhase, JobRequest, JobSummary, OnUnrepairable, Outcome,
    OutputFile, Par2Report, Preflight, Verdict,
};
use super::EngineInner;
use crate::config::Config;
use crate::download::{AvailabilityReport, DownloadResult, Downloader, Nzb};
use crate::error::{ConfigError, DlNzbError, NzbError};
use crate::nntp::NntpPoolExt;
use crate::patterns::{self, rar as rar_patterns};
use crate::processing::deobfuscate::name_from_title;
use crate::processing::{Par2Status, PostProcessingOutcome, PostProcessor};
use crate::util::{blocking, format_percent};

/// Download and post-process one NZB.
pub(crate) async fn run(
    engine: Arc<EngineInner>,
    ctx: Arc<JobCtx>,
    request: JobRequest,
) -> JobSummary {
    let started = Instant::now();
    let mut run = JobRun::new(request.output_dir.clone());
    let result = run.download(&engine, &ctx, &request).await;
    run.finish(result, &ctx, started).await
}

/// Post-process an existing job folder (no download).
pub(crate) async fn reprocess(
    engine: Arc<EngineInner>,
    ctx: Arc<JobCtx>,
    output_dir: PathBuf,
    passwords: Vec<String>,
) -> JobSummary {
    let started = Instant::now();
    let mut run = JobRun::new(output_dir.clone());
    let result = run.reprocess(&engine, &ctx, &output_dir, passwords).await;
    run.finish(result, &ctx, started).await
}

/// What a job accumulates on the way to its summary.
struct JobRun {
    summary: JobSummary,
    results: Vec<DownloadResult>,
    post: Option<PostProcessingOutcome>,
    post_error: Option<String>,
    /// Message when the pre-flight scan stopped the job.
    unrepairable: Option<String>,
    /// The job's resume record (the folder's sidecar).
    record: Option<Arc<JobRecord>>,
    /// Every download phase has finished (in this run or an earlier one).
    download_done: bool,
    /// Post-processing ran to its end (not stopped).
    post_done: bool,
    /// PAR2's verdict in this run: verified (or repaired), or damaged beyond
    /// repair. `None` when it had nothing to say (off, no PAR2, stopped).
    par2_verdict: Option<bool>,
    /// PAR2 was skipped because it verified the unchanged files in an earlier run.
    par2_verified_earlier: bool,
    /// PAR2 repair was turned on when post-processing ran.
    par2_repair_on: bool,
}

impl JobRun {
    fn new(output_dir: PathBuf) -> Self {
        Self {
            summary: JobSummary::new(Outcome::Completed, output_dir),
            results: Vec::new(),
            post: None,
            post_error: None,
            unrepairable: None,
            record: None,
            download_done: false,
            post_done: false,
            par2_verdict: None,
            par2_verified_earlier: false,
            par2_repair_on: false,
        }
    }

    async fn download(
        &mut self,
        engine: &Arc<EngineInner>,
        ctx: &Arc<JobCtx>,
        request: &JobRequest,
    ) -> Result<(), DlNzbError> {
        let config = engine.config();
        config.validate_for_download()?;
        let output_dir = &request.output_dir;
        require_absolute(output_dir)?;

        // Reading the NZB and the folder's sidecar is file work: off the
        // async threads.
        let (nzb_path, dir, fsync) = (
            request.nzb_path.clone(),
            output_dir.clone(),
            config.tuning.fsync_on_finalize,
        );
        let (nzb, opened) = blocking(move || -> Result<_, DlNzbError> {
            let nzb = Nzb::from_file(&nzb_path)?;
            if nzb.files().is_empty() {
                return Err(NzbError::Empty.into());
            }
            std::fs::create_dir_all(&dir)?;
            let opened = JobRecord::open(&dir, &nzb, fsync);
            Ok((nzb, opened))
        })
        .await?;
        let record = self.take_record(ctx, opened);
        // The job's name, for renaming: the request's title, else the NZB's
        // (a title that makes no file name, ".." say, doesn't count); never
        // the folder's (the caller may have de-duplicated it). Kept in the
        // sidecar for `reprocess`.
        let name = request
            .title
            .clone()
            .filter(|t| name_from_title(t).is_some())
            .unwrap_or_else(|| super::inspect::nzb_title(&nzb, &request.nzb_path));
        record.set_title(&name);

        // An earlier session downloaded everything: straight to
        // post-processing, without connecting (and with nothing to download,
        // no free-space check: its files may be renamed or deleted by now).
        if record.download_complete() {
            self.download_done = true;
            self.results = record.results(output_dir, |_, attempted| attempted);
            ctx.add_recorded(&self.results);
        } else {
            self.fetch(engine, ctx, request, &config, &nzb, &record)
                .await?;
            if !self.download_done {
                return Ok(());
            }
        }

        // The user's passwords are tried first (newest first), then the NZB's own.
        self.post_process(
            &config,
            ctx,
            output_dir,
            Some(&name),
            request.passwords.clone(),
            nzb.meta().passwords.clone(),
        )
        .await;
        Ok(())
    }

    /// Download what `record` says is left of `nzb`: connect, scan when the
    /// request asks, then fetch. Sets `download_done` once every download
    /// phase has finished; it stays unset when the job was stopped, the scan
    /// found it unrepairable, or a phase lost the server.
    async fn fetch(
        &mut self,
        engine: &Arc<EngineInner>,
        ctx: &Arc<JobCtx>,
        request: &JobRequest,
        config: &Config,
        nzb: &Nzb,
        record: &Arc<JobRecord>,
    ) -> Result<(), DlNzbError> {
        let output_dir = &request.output_dir;
        disk::check_free_space(output_dir, nzb, config, request.free_space_hint, record)?;
        if ctx.is_cancelled() {
            return Ok(());
        }

        // Connect: one connection up front so a wrong password, unknown host or
        // TLS problem ends the job immediately with its real cause.
        ctx.set_phase(JobPhase::Connecting);
        let pool = engine.pool_for(config)?;
        let Some(first) = first_connection(&pool, &config.usenet, ctx).await else {
            return Ok(());
        };
        let first = first?;
        ctx.add_wire_bytes(first.take_bytes_read());
        drop(first);
        let downloader = Downloader::for_job(pool, config.usenet.connections as usize, ctx.clone());

        // Pre-flight availability scan. Not when continuing a download: going
        // ahead was decided when it started.
        let scan = !record.has_progress()
            && match request.preflight {
                Preflight::Always => true,
                Preflight::Never => false,
                Preflight::Auto => !nzb.has_par2(),
            };
        let mut skip_message_ids = None;
        if scan {
            ctx.set_phase(JobPhase::Checking);
            let scan_started = Instant::now();
            let report = downloader.check_all_availability(nzb).await;
            self.summary.check_secs = scan_started.elapsed().as_secs_f64();
            if ctx.is_cancelled() {
                return Ok(());
            }
            let info = availability_info(&report);
            ctx.availability(info.clone());
            self.summary.availability = Some(info.clone());
            if info.verdict == Verdict::Unrepairable
                && request.on_unrepairable == OnUnrepairable::Stop
            {
                self.unrepairable = Some(unrepairable_message(&report));
                return Ok(());
            }
            if !report.missing_ids.is_empty() {
                skip_message_ids = Some(report.missing_ids);
            }
        }

        // Download: data and the PAR2 index first, recovery volumes only if
        // something is missing. The sidecar is written before the first
        // article, kept current while articles land, and saved once more at
        // the end (also after a stop or an error).
        let mut download_config = config.clone();
        download_config.download.dir = output_dir.clone();
        if let Err(e) = record.save().await {
            tracing::debug!("could not save resume data: {e}");
            ctx.warn(
                "Could not save resume data in the job folder, so a stopped download would start over.",
            );
        }
        let saver = record.spawn_saver(ctx);
        let outcome = downloader
            .download_nzb_resuming(
                nzb,
                download_config,
                skip_message_ids.as_ref(),
                config.post_processing.download_all_par2,
                Some(record),
            )
            .await;
        saver.abort();
        if let Err(e) = record.save().await {
            tracing::debug!("could not save resume data: {e}");
        }
        let outcome = outcome?;
        self.summary.download_secs = outcome.transfer_duration.as_secs_f64();
        self.summary.wire_bytes = outcome.actual_wire_bytes;
        self.results = outcome.files;
        if ctx.is_cancelled() {
            return Ok(());
        }
        // A phase that lost the server didn't finish: its files are fetched
        // again on resume, so nothing may rename or delete them before then
        // (`judge` fails such a job with the connection's error).
        self.download_done = record.download_complete();
        Ok(())
    }

    /// Keep the folder's resume record as [`JobRecord::open`] found it. A
    /// sidecar left by another NZB, or one that can't be read, is not
    /// trusted: the job starts fresh (and says so).
    fn take_record(
        &mut self,
        ctx: &JobCtx,
        (record, opened): (Arc<JobRecord>, Opened),
    ) -> Arc<JobRecord> {
        match opened {
            Opened::Fresh | Opened::Resumed => {}
            Opened::Mismatch => ctx.warn(
                "The folder holds resume data from a different NZB, so this download starts from the beginning.",
            ),
            Opened::Unreadable(why) => ctx.warn(format!(
                "The folder's resume data could not be used because {why}, so this download starts from the beginning."
            )),
        }
        self.record = Some(record.clone());
        record
    }

    async fn reprocess(
        &mut self,
        engine: &Arc<EngineInner>,
        ctx: &Arc<JobCtx>,
        output_dir: &Path,
        passwords: Vec<String>,
    ) -> Result<(), DlNzbError> {
        let config = engine.config();
        require_absolute(output_dir)?;
        self.results = existing_files(output_dir)?;
        // The folder's sidecar (whatever NZB it is for) tells whether PAR2
        // already verified these files, and which articles never arrived: a
        // file with holes is not whole because it is on disk, and with no
        // PAR2 to fill them the job must not end Completed (deleting the
        // record, the only thing that knows about the holes).
        self.record = JobRecord::open_existing(output_dir);
        let mut finished = true;
        if let Some(record) = &self.record {
            // A download that never finished (stopped, or the server went
            // away): the data files it never finished or never started are
            // holes too. Recovery volumes it hadn't fetched aren't: they
            // may never have been needed.
            finished = record.download_complete();
            let recorded = record.results(output_dir, |name, attempted| {
                attempted || (!finished && !patterns::par2::is_par2_file(Path::new(name)))
            });
            ctx.add_recorded(&recorded);
            with_recorded_failures(&mut self.results, recorded);
        }
        // `<name>.partial` with no `<name>`: a file the download never
        // finished, with or without a record.
        for unfinished in unfinished_files(output_dir) {
            finished = false;
            if !self
                .results
                .iter()
                .any(|r| r.filename == unfinished.filename)
            {
                ctx.add_recorded(std::slice::from_ref(&unfinished));
                self.results.push(unfinished);
            }
        }
        // Post-processing renames and deletes files, which the download
        // must find where it left them when `start()` finishes it: an
        // unfinished download ends "did not finish" (resumable while the
        // sidecar is there), never Completed.
        self.download_done = finished;
        if !finished {
            return Ok(());
        }
        // The job's name as `start()` recorded it (only a sidecar from before
        // titles were kept, or none at all, leaves the folder's name).
        let name = self.record.as_ref().and_then(|r| r.title());
        self.post_process(
            &config,
            ctx,
            output_dir,
            name.as_deref(),
            passwords,
            Vec::new(),
        )
        .await;
        Ok(())
    }

    /// `name` is the job's (for renaming), `passwords` the user's (tried
    /// first, newest first), `nzb_passwords` the NZB's own.
    async fn post_process(
        &mut self,
        config: &Config,
        ctx: &Arc<JobCtx>,
        output_dir: &Path,
        name: Option<&str>,
        passwords: Vec<String>,
        nzb_passwords: Vec<String>,
    ) {
        let pp = &config.post_processing;
        self.par2_repair_on = pp.auto_par2_repair;
        if !(pp.auto_par2_repair || pp.auto_extract_rar || pp.deobfuscate_file_names) {
            self.post_done = true;
            return;
        }
        let started = Instant::now();
        // PAR2 verified these files in an earlier run and none has changed
        // since (volumes deleted after their extraction aside): it isn't run
        // again.
        let verified_earlier = pp.auto_par2_repair
            && self
                .record
                .as_ref()
                .is_some_and(|r| r.par2_still_verified(output_dir));
        let mut processor = PostProcessor::new(pp.clone())
            .with_job(ctx.clone())
            .with_passwords(passwords)
            .with_nzb_passwords(nzb_passwords)
            .with_record(self.record.clone())
            .with_par2_verified_earlier(verified_earlier);
        if let Some(name) = name {
            processor = processor.with_name(name);
        }
        let finished = match processor.process(output_dir, &self.results).await {
            Ok(outcome) => {
                let finished = !ctx.is_cancelled();
                if finished {
                    self.par2_verdict = match outcome.par2_status {
                        Par2Status::Success => Some(true),
                        Par2Status::Failed => Some(false),
                        Par2Status::NoPar2Files => None,
                    };
                }
                self.par2_verified_earlier = verified_earlier
                    && outcome.par2_status == Par2Status::Success
                    && !outcome.par2_ran;
                self.post = Some(outcome);
                finished
            }
            Err(e) => {
                self.post_error = Some(e.user_message());
                !ctx.is_cancelled()
            }
        };
        self.post_done = finished;
        self.summary.post_secs = started.elapsed().as_secs_f64();
    }

    /// Bring the sidecar up to date for a job that ended any other way than
    /// `Completed` (which removes it, leaving a clean folder). Returns whether
    /// a sidecar remains.
    async fn settle_sidecar(&mut self, outcome: Outcome) -> bool {
        let Some(record) = self.record.take() else {
            return false;
        };
        if outcome == Outcome::Completed {
            record.remove().await;
            return false;
        }
        if !record.on_disk() {
            // Nothing was ever recorded (e.g. the job ended before
            // downloading): leave no sidecar behind.
            return false;
        }
        if let Some(verified) = self.par2_verdict {
            let dir = self.summary.output_dir.clone();
            let stamped = record.clone();
            let _ = tokio::task::spawn_blocking(move || stamped.set_par2_verified(&dir, verified))
                .await;
        }
        if let Err(e) = record.save().await {
            tracing::debug!("could not save resume data: {e}");
        }
        record.on_disk()
    }

    async fn finish(
        mut self,
        result: Result<(), DlNzbError>,
        ctx: &JobCtx,
        started: Instant,
    ) -> JobSummary {
        let summary = &mut self.summary;
        summary.elapsed_secs = started.elapsed().as_secs_f64();
        summary.articles_total = ctx.articles_total();
        summary.articles_failed = ctx.articles_failed();
        summary.nzb_files = self
            .results
            .iter()
            .map(|r| FileReport {
                name: r.filename.clone(),
                path: r.path.clone(),
                bytes: r.size,
                articles_total: r.segments_total as u64,
                articles_failed: r.segments_failed as u64,
            })
            .collect();
        summary.data_bytes = self
            .results
            .iter()
            .filter(|r| !patterns::par2::is_par2_file(&r.path))
            .map(|r| r.size)
            .sum();

        if let Some(post) = &self.post {
            summary.par2 = par2_report(post);
            if self.par2_verified_earlier {
                summary.par2.skipped_reason =
                    Some("PAR2 verified these files in an earlier run.".to_string());
            }
            summary.archives_extracted = post.rar.archives_extracted as u32;
            summary.archives_failed = post.rar.archives_failed as u32;
            summary.files_renamed = post.files_renamed as u32;
        } else {
            summary.par2.skipped_reason = Some("Post-processing did not run.".into());
        }

        // A stop that lands once everything has run to its end (post-processing
        // returned before it) stops nothing: the job ends as it would have,
        // so a Completed job leaves no sidecar, and a Stopped one always has
        // work left for `start()`.
        let all_done = self.download_done && self.post_done;
        let ending = if ctx.is_cancelled() && !all_done {
            Ending::new(Outcome::Stopped, "Stopped; the downloaded data was kept.")
        } else if let Err(e) = &result {
            Ending::failed(e.kind(), e.user_message())
        } else if let Some(message) = self.unrepairable.take() {
            Ending::new(Outcome::Unrepairable, message)
        } else {
            self.judge(ctx)
        };

        // Resumable: stopped or failed, with the sidecar in place and work
        // left that `start()` would pick up (downloading, or post-processing
        // that didn't reach its end).
        let sidecar_kept = self.settle_sidecar(ending.outcome).await;
        let work_left = !all_done;
        let summary = &mut self.summary;
        summary.outcome = ending.outcome;
        summary.message = ending.message;
        summary.error_kind = ending.error_kind;
        summary.resumable = matches!(ending.outcome, Outcome::Stopped | Outcome::Failed)
            && sidecar_kept
            && work_left;

        let dir = summary.output_dir.clone();
        let extracted = self
            .post
            .as_ref()
            .map(|p| p.rar.extracted.clone())
            .unwrap_or_default();
        if let Some(files) =
            blocking(move || dir.is_dir().then(|| list_output_files(&dir, &extracted))).await
        {
            summary.files = files;
        }
        self.summary
    }

    /// Outcome of a job that ran to the end.
    fn judge(&self, ctx: &JobCtx) -> Ending {
        // A download phase that didn't finish (a worker lost the server) is
        // never a success, whatever else went right: resuming fetches the
        // rest, and post-processing waited for that.
        if !self.download_done {
            return match ctx.connection_error() {
                Some((kind, message)) => Ending::failed(kind, message),
                None => Ending::new(Outcome::Failed, "The download did not finish."),
            };
        }
        let par2_ok = self
            .post
            .as_ref()
            .is_some_and(|p| p.par2_status == Par2Status::Success);
        // PAR2 repair is exactly how download-phase failures are recovered, so
        // a verified/repaired payload is whole regardless of wire hiccups.
        // Otherwise the download is still fine if the only failures are in
        // files the user doesn't need (.nfo/.sfv/.srr).
        let download_ok = par2_ok
            || self
                .results
                .iter()
                .all(|r| r.segments_failed == 0 || patterns::is_auxiliary_name(&r.filename));

        if !download_ok {
            // Workers that could not reach the server at all: report that
            // (resumable: the download phase didn't count as finished, so a
            // resume fetches those articles again) rather than "articles
            // missing".
            if let Some((kind, message)) = ctx.connection_error() {
                return Ending::failed(kind, message);
            }
            // Articles the server never got across aren't missing.
            let missing = format_percent(
                self.summary
                    .articles_failed
                    .saturating_sub(ctx.connection_failures()),
                self.summary.articles_total.max(1),
            );
            let reason = match &self.post {
                Some(p) if p.par2_ran => "PAR2 could not repair them",
                _ if !self.has_par2() => "there is no recovery data to repair them",
                _ if !self.par2_repair_on => "PAR2 repair is turned off",
                // Deleted, by `delete_par2_after_repair` in an earlier run or
                // by hand.
                _ if !self.par2_on_disk() => "the PAR2 files needed to repair them are gone",
                _ => "PAR2 could not repair them",
            };
            return Ending::new(
                Outcome::Failed,
                format!("{missing} of articles are missing and {reason}."),
            );
        }

        // An archive no password opened: the download is whole and stays in
        // place for `reprocess` with the right password. Never a success.
        if let Some(post) = &self.post {
            if post.rar.archives_encrypted > 0 {
                let message = if post.rar.user_passwords_failed {
                    "The password didn't work."
                } else {
                    "This archive needs a password."
                };
                return Ending::new(Outcome::NeedsPassword, message);
            }
        }

        let issue = if let Some(error) = &self.post_error {
            Some(format!("Post-processing failed: {}", error))
        } else if let Some(post) = &self.post {
            if post.par2_status == Par2Status::Failed {
                Some("PAR2 verification failed.".to_string())
            } else if post.rar.archives_failed > 0 {
                Some(if post.rar.archives_failed == 1 {
                    "1 archive could not be extracted.".to_string()
                } else {
                    format!(
                        "{} archives could not be extracted.",
                        post.rar.archives_failed
                    )
                })
            } else {
                None
            }
        } else {
            None
        };
        match issue {
            Some(message) => Ending::new(Outcome::CompletedWithIssues, message),
            None => Ending {
                outcome: Outcome::Completed,
                message: None,
                error_kind: None,
            },
        }
    }

    /// The NZB has PAR2 files (whether or not they are still on disk).
    fn has_par2(&self) -> bool {
        self.results
            .iter()
            .any(|r| patterns::par2::is_par2_file(&r.path))
    }

    /// Some of the NZB's PAR2 files are still in the job folder.
    fn par2_on_disk(&self) -> bool {
        self.results
            .iter()
            .any(|r| patterns::par2::is_par2_file(&r.path) && r.path.is_file())
    }
}

/// How a job ended, before it is written into the summary.
struct Ending {
    outcome: Outcome,
    message: Option<String>,
    error_kind: Option<super::ErrorKind>,
}

impl Ending {
    fn new(outcome: Outcome, message: impl Into<String>) -> Self {
        Self {
            outcome,
            message: Some(message.into()),
            error_kind: None,
        }
    }

    /// The job failed because of an error of `kind`.
    fn failed(kind: super::ErrorKind, message: impl Into<String>) -> Self {
        Self {
            error_kind: Some(kind),
            ..Self::new(Outcome::Failed, message)
        }
    }
}

/// The job's connection check. A server that turns the connection away
/// (a 400/502 greeting, "try later" or "too many connections"; refused or
/// reset) is asked again `retry_attempts` times, waiting longer each time,
/// before the job fails: one refusal says little about the next connection.
/// Anything else (a rejected password, an unknown host, a TLS problem)
/// fails at once. `None` when the job was stopped meanwhile.
async fn first_connection(
    pool: &crate::nntp::NntpPool,
    usenet: &crate::config::UsenetConfig,
    ctx: &JobCtx,
) -> Option<Result<crate::nntp::PooledConnection, DlNzbError>> {
    let base = std::time::Duration::from_millis(usenet.retry_delay);
    let mut attempt = 0usize;
    loop {
        let result = tokio::select! {
            biased;
            _ = ctx.cancelled() => return None,
            conn = pool.get_connection() => conn,
        };
        match result {
            Err(e)
                if e.kind() == super::ErrorKind::Connect
                    && attempt < usize::from(usenet.retry_attempts) =>
            {
                attempt += 1;
                tracing::debug!("Connection check failed (try {attempt}): {e}");
                let wait = crate::download::downloader::backoff_delay(base, attempt);
                tokio::select! {
                    biased;
                    _ = ctx.cancelled() => return None,
                    _ = tokio::time::sleep(wait) => {}
                }
            }
            other => return Some(other),
        }
    }
}

fn require_absolute(dir: &Path) -> Result<(), DlNzbError> {
    if dir.is_absolute() {
        Ok(())
    } else {
        Err(ConfigError::InvalidPath {
            path: dir.to_path_buf(),
            reason: "the job folder must be an absolute path".into(),
        }
        .into())
    }
}

fn par2_report(post: &PostProcessingOutcome) -> Par2Report {
    let verified_ok = post.par2_status == Par2Status::Success;
    let skipped_reason = if post.par2_ran {
        None
    } else if verified_ok {
        Some("Every article matched its checksum during download.".to_string())
    } else {
        Some("There were no PAR2 files to verify with, or PAR2 repair is turned off.".to_string())
    };
    Par2Report {
        ran: post.par2_ran,
        verified_ok,
        damaged_blocks: post.par2_damaged_blocks,
        repaired_blocks: post.par2_repaired_blocks,
        repaired: post.par2_ran && verified_ok && post.par2_repaired_blocks > 0,
        skipped_reason,
    }
}

fn availability_info(report: &AvailabilityReport) -> AvailabilityInfo {
    let verdict = if report.missing_files.is_empty() {
        if report.scan_incomplete {
            Verdict::Unknown
        } else {
            Verdict::Complete
        }
    } else if report.only_nonessential_missing() {
        Verdict::Complete
    } else if report.likely_repairable() {
        Verdict::Repairable
    } else if report.possibly_repairable() {
        // What is confirmed missing could be repaired: the articles the scan
        // couldn't check decide, and they are still downloaded.
        Verdict::Unknown
    } else {
        Verdict::Unrepairable
    };
    AvailabilityInfo {
        articles_total: report.articles_total,
        articles_missing: report.missing_ids.len() as u64,
        missing_bytes: report.missing_data_bytes,
        recovery_bytes: report.available_par2_bytes,
        verdict,
    }
}

/// "9% of articles are missing and there is not enough recovery data."
fn unrepairable_message(report: &AvailabilityReport) -> String {
    // What is confirmed missing (that alone is beyond repair).
    let missing = report
        .missing_data_bytes
        .saturating_sub(report.unknown_data_bytes);
    let missing = format_percent(missing, report.total_data_bytes.max(1));
    if report.has_par2 {
        format!("{missing} of articles are missing and there is not enough recovery data.")
    } else {
        format!("{missing} of articles are missing and there is no recovery data.")
    }
}

/// The files already in a job folder, as download results for reprocessing.
/// No wire checksums are known, so PAR2 verifies them for real.
fn existing_files(dir: &Path) -> Result<Vec<DownloadResult>, DlNzbError> {
    let mut results = Vec::new();
    for entry in std::fs::read_dir(dir)?.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || name.starts_with('.') || name.ends_with(patterns::PARTIAL_EXT) {
            continue;
        }
        results.push(DownloadResult {
            filename: name,
            path: entry.path(),
            size: meta.len(),
            segments_total: 0,
            segments_downloaded: 0,
            segments_failed: 0,
            all_segments_crc_verified: false,
        });
    }
    results.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(results)
}

/// Files the download never finished: a `<name>.partial` with no `<name>`
/// beside it, as results with every article missing (how many isn't known
/// without the record; one stands for them).
fn unfinished_files(dir: &Path) -> Vec<DownloadResult> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<DownloadResult> = entries
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let name = entry
                .file_name()
                .to_str()?
                .strip_suffix(patterns::PARTIAL_EXT)?
                .to_string();
            let path = dir.join(&name);
            if name.is_empty() || name.starts_with('.') || path.exists() {
                return None;
            }
            let size = entry.metadata().ok().filter(|m| m.is_file())?.len();
            Some(DownloadResult {
                filename: name,
                path,
                size,
                segments_total: 1,
                segments_downloaded: 0,
                segments_failed: 1,
                all_segments_crc_verified: false,
            })
        })
        .collect();
    files.sort_by(|a, b| a.filename.cmp(&b.filename));
    files
}

/// Bring what the job record knows about each downloaded file (its article
/// counts, and so the articles that never arrived) into `results`, the files
/// found in the folder. A recorded file with missing articles that is no
/// longer there under its name (renamed, deleted) still counts: its holes
/// were never filled.
fn with_recorded_failures(results: &mut Vec<DownloadResult>, recorded: Vec<DownloadResult>) {
    for file in recorded {
        match results.iter_mut().find(|r| r.filename == file.filename) {
            Some(found) => {
                found.segments_total = file.segments_total;
                found.segments_downloaded = file.segments_downloaded;
                found.segments_failed = file.segments_failed;
            }
            None if file.segments_failed > 0 => results.push(file),
            None => {}
        }
    }
}

/// User-facing files in the job folder (recursively, names relative to it):
/// everything except hidden files, recovery and release-metadata files,
/// `.partial` leftovers, and the volumes of archives that were extracted.
fn list_output_files(dir: &Path, extracted: &[PathBuf]) -> Vec<OutputFile> {
    const MAX_FILES: usize = 10_000;
    const MAX_DEPTH: usize = 8;
    let extracted_bases: Vec<String> = extracted
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .map(|n| rar_patterns::extract_base_name(n).unwrap_or(n).to_string())
        .collect();

    let mut files = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !file_type.is_file()
                || patterns::is_auxiliary_name(&name)
                || extracted_bases
                    .iter()
                    .any(|base| rar_patterns::is_same_archive(base, &name))
            {
                continue;
            }
            let relative = path
                .strip_prefix(dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(name);
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push(OutputFile {
                name: relative,
                bytes,
            });
            if files.len() >= MAX_FILES {
                break;
            }
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job folder with a saved resume record (one file, one article).
    async fn folder_with_record() -> (tempfile::TempDir, Arc<JobRecord>) {
        let nzb: Nzb = r#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><file poster="p" date="1700000000" subject="&quot;a.bin&quot; yEnc (1/1)"><groups><group>a.b</group></groups><segments><segment bytes="100" number="1">x1@t</segment></segments></file></nzb>"#
            .parse()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), b"data").unwrap();
        let (record, _) = JobRecord::open(dir.path(), &nzb, false);
        record.save().await.unwrap();
        (dir, record)
    }

    /// A stop that lands after post-processing returned (between its end and
    /// the job's) stops nothing: the job is Completed and leaves no sidecar.
    /// It used to end Stopped, not resumable, with a stale sidecar.
    #[tokio::test]
    async fn a_stop_after_the_last_work_ends_the_job_as_it_would_have() {
        let (dir, record) = folder_with_record().await;
        let mut run = JobRun::new(dir.path().to_path_buf());
        run.record = Some(record);
        run.download_done = true;
        run.post_done = true;
        let ctx = JobCtx::detached();
        ctx.cancel();
        let summary = run.finish(Ok(()), &ctx, Instant::now()).await;
        assert_eq!(summary.outcome, Outcome::Completed, "{summary:?}");
        assert!(!summary.resumable);
        assert!(!dir.path().join(super::super::sidecar::FILE_NAME).exists());
    }

    /// Stopped with work left: resumable, the sidecar kept.
    #[tokio::test]
    async fn a_stop_with_work_left_is_resumable() {
        let (dir, record) = folder_with_record().await;
        let mut run = JobRun::new(dir.path().to_path_buf());
        run.record = Some(record);
        run.download_done = true;
        run.post_done = false;
        let ctx = JobCtx::detached();
        ctx.cancel();
        let summary = run.finish(Ok(()), &ctx, Instant::now()).await;
        assert_eq!(summary.outcome, Outcome::Stopped);
        assert!(summary.resumable);
        assert!(dir.path().join(super::super::sidecar::FILE_NAME).exists());
    }

    #[test]
    fn output_listing_hides_recovery_metadata_and_extracted_volumes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        for name in [
            "Movie.mkv",
            "Movie.part01.rar",
            "Movie.part02.rar",
            "Movie.par2",
            "Movie.nfo",
            "Other.rar",
            ".dl-nzb-job.json",
            "x.mkv.partial",
        ] {
            std::fs::write(p.join(name), b"x").unwrap();
        }
        std::fs::create_dir(p.join("Sub")).unwrap();
        std::fs::write(p.join("Sub").join("a.srt"), b"x").unwrap();

        let files = list_output_files(p, &[p.join("Movie.part01.rar")]);
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Movie.mkv", "Other.rar", "Sub/a.srt"]);
    }
}
