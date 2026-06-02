//! NZB downloader.
//!
//! Segments are written at the offset reported by yEnc `=ypart`, so the
//! on-disk layout has no gaps regardless of how the NZB labelled segment
//! sizes. Workers share an `Arc<AsyncMutex<UnboundedReceiver<SegmentJob>>>`;
//! the lock is briefly held only while pulling a job. Retries flow back
//! through a feedback channel to the coordinator, which re-enqueues them
//! and closes the job channel when all work has settled.

use bytes::Bytes;
use futures::stream::{self, StreamExt};
use human_bytes::human_bytes;
use indicatif::ProgressBar;
use tokio::sync::mpsc::{self, Sender};
use tokio::time::MissedTickBehavior;

use std::cmp::Reverse;
use std::collections::{HashSet, VecDeque};
use std::fs::File as StdFile;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::nzb::{Nzb, NzbFile};
use crate::config::Config;
use crate::error::{DlNzbError, DownloadError};
use crate::nntp::{
    ArticleOutcome, NntpPool, NntpPoolBuilder, NntpPoolExt, PooledConnection, SegmentRequest,
};
use crate::progress;

type Result<T> = std::result::Result<T, DlNzbError>;

/// Per-file result reported back to the caller.
#[derive(Debug)]
pub struct DownloadResult {
    pub filename: String,
    pub path: PathBuf,
    /// Final size of the file on disk after truncation (decoded bytes).
    pub size: u64,
    pub segments_downloaded: usize,
    pub segments_failed: usize,
}

/// Aggregate outcome of a download. Speed reporting uses
/// `actual_wire_bytes / transfer_duration`, which reflects plaintext bytes
/// pulled from the socket and lines up much more closely with what external
/// network monitors and other NZB clients display.
pub struct DownloadOutcome {
    pub files: Vec<DownloadResult>,
    pub progress_bar: ProgressBar,
    pub transfer_duration: Duration,
    /// Plaintext bytes received across all connections during the transfer
    /// window. Includes yEnc framing/escape sequences, NNTP command/response
    /// overhead, and any retry traffic — the sum of `segment.bytes` from the
    /// NZB is the encoded payload and runs ~2-4% below this in the steady state.
    pub actual_wire_bytes: u64,
}

/// Result of a pre-flight `STAT`-all availability scan. Byte tallies use the
/// NZB's encoded segment sizes, which are proportional to PAR2 block counts, so
/// comparing missing data bytes against available recovery bytes is a good
/// repairability estimate (a recovery block reconstructs one same-sized data
/// block). It's an estimate, not a guarantee: a present article can still fail
/// its yEnc CRC at download time.
#[derive(Debug, Default)]
pub struct AvailabilityReport {
    pub total_data_bytes: u64,
    pub missing_data_bytes: u64,
    pub total_par2_bytes: u64,
    pub available_par2_bytes: u64,
    /// All missing article ids — handed to the downloader so they aren't fetched.
    pub missing_ids: HashSet<String>,
    /// Names of data files with at least one CONFIRMED-missing segment.
    pub missing_files: Vec<String>,
    /// Subset of `missing_files` that are essential (not .nfo/.sfv/.srr).
    pub missing_essential_files: Vec<String>,
    pub has_par2: bool,
    /// Some segments could not be STAT-checked (a batch errored). Their bytes are
    /// counted pessimistically in `missing_data_bytes`/`available_par2_bytes`, so
    /// the percentages are a conservative floor.
    pub scan_incomplete: bool,
}

impl AvailabilityReport {
    pub fn data_complete(&self) -> bool {
        self.missing_data_bytes == 0
    }

    pub fn data_completion_percent(&self) -> f64 {
        if self.total_data_bytes == 0 {
            100.0
        } else {
            (self
                .total_data_bytes
                .saturating_sub(self.missing_data_bytes)) as f64
                / self.total_data_bytes as f64
                * 100.0
        }
    }

    /// True if every missing data file is non-essential (.nfo/.sfv/.srr only).
    pub fn only_nonessential_missing(&self) -> bool {
        !self.missing_files.is_empty() && self.missing_essential_files.is_empty()
    }

    /// Whether the available PAR2 recovery can likely repair the missing data.
    /// Recovery blocks are the same size as data blocks, so available recovery
    /// bytes must cover the missing data bytes; a 10% headroom absorbs block-
    /// alignment over-counting.
    pub fn likely_repairable(&self) -> bool {
        if self.missing_data_bytes == 0 {
            return true;
        }
        if !self.has_par2 {
            return false;
        }
        self.available_par2_bytes as f64 >= self.missing_data_bytes as f64 * 1.1
    }
}

pub struct Downloader {
    pool: NntpPool,
    /// Concurrent connection budget. The number of workers we spawn during a
    /// download, and the warm-up target.
    connections: usize,
}

impl Downloader {
    pub async fn new(config: Config) -> Result<Self> {
        let connections = config.usenet.connections as usize;
        let pool = NntpPoolBuilder::new(config.usenet.clone())
            .max_concurrent_connections(config.tuning.max_concurrent_connections)
            .build()?;

        // Eagerly establish at least one connection so credential / TLS errors
        // surface immediately rather than midway through downloading.
        let warmed = pool.warm_up(1).await;
        if warmed == 0 {
            return Err(DownloadError::PoolExhausted.into());
        }

        Ok(Self { pool, connections })
    }

    /// Pre-flight availability scan: `STAT` **every** segment (pipelined across
    /// all connections) to learn exactly which articles are missing before
    /// committing to a download. Returns a byte-accurate [`AvailabilityReport`]
    /// — the missing-id set (so missing articles aren't fetched) plus the data
    /// vs PAR2-recovery byte tallies needed to estimate repairability.
    ///
    /// Replaces the old first-segment-only heuristic: a file with its first
    /// segment present but later segments missing is now detected, and a file
    /// missing only its first segment still downloads its remaining segments.
    pub async fn check_all_availability(&self, nzb: &Nzb) -> Result<AvailabilityReport> {
        let files: Vec<&NzbFile> = nzb.files().iter().collect();

        struct FileAcc {
            name: String,
            is_par2: bool,
            essential: bool,
            total_bytes: u64,
            /// Confirmed-absent bytes (STAT said 430). Drives the skip set and
            /// the displayed "missing files" list.
            missing_bytes: u64,
            /// Bytes whose STAT could not be determined (batch error). Counted
            /// pessimistically in the repairability verdict but NOT skipped.
            unknown_bytes: u64,
            has_missing: bool,
        }

        let mut file_accs: Vec<FileAcc> = Vec::with_capacity(files.len());
        // message_id -> (file index, encoded bytes)
        let mut meta: std::collections::HashMap<String, (usize, u64)> =
            std::collections::HashMap::new();
        let mut requests: Vec<SegmentRequest> = Vec::new();

        for file in &files {
            let filename = Nzb::get_filename_from_subject(&file.subject)
                .unwrap_or_else(|| file.subject.clone());
            let is_par2 = crate::patterns::par2::is_par2_file(Path::new(&filename));
            let lower = filename.to_lowercase();
            let essential =
                !(lower.ends_with(".nfo") || lower.ends_with(".sfv") || lower.ends_with(".srr"));
            let group = file.groups.group.first().map(|g| g.name.clone());
            let idx = file_accs.len();
            let total_bytes: u64 = file.segments.segment.iter().map(|s| s.bytes).sum();
            let no_group = group.is_none();
            if let Some(group) = group {
                for seg in &file.segments.segment {
                    meta.insert(seg.message_id.clone(), (idx, seg.bytes));
                    requests.push(SegmentRequest {
                        message_id: seg.message_id.clone(),
                        group: group.clone(),
                    });
                }
            }
            file_accs.push(FileAcc {
                name: filename,
                is_par2,
                essential,
                total_bytes,
                // A file with no newsgroup can't be fetched — count it as missing.
                missing_bytes: if no_group { total_bytes } else { 0 },
                unknown_bytes: 0,
                has_missing: no_group,
            });
        }

        // STAT every segment, pipelined in large batches across all connections.
        const STAT_BATCH: usize = 256;
        let parallelism = self.connections.max(1);
        let batches: Vec<Vec<SegmentRequest>> =
            requests.chunks(STAT_BATCH).map(|c| c.to_vec()).collect();
        let pool = self.pool.clone();
        // Per-article STAT status: Some(true)=present, Some(false)=absent (430),
        // None=unknown (the batch errored). Unknown is NOT marked absent — those
        // articles are still attempted during download — but it is counted
        // pessimistically in the repairability verdict so a STAT failure can
        // never make an unrepairable set look repairable.
        let batch_futures = batches.into_iter().map(|batch| {
            let pool = pool.clone();
            async move {
                match pool.get_connection().await {
                    Ok(mut conn) => match conn.check_articles_exist(&batch).await {
                        Ok(r) => r.into_iter().map(|(id, ex)| (id, Some(ex))).collect(),
                        Err(e) => {
                            tracing::debug!("STAT batch failed: {}", e);
                            batch.into_iter().map(|r| (r.message_id, None)).collect()
                        }
                    },
                    Err(e) => {
                        tracing::debug!("STAT pool failure: {}", e);
                        batch.into_iter().map(|r| (r.message_id, None)).collect()
                    }
                }
            }
        });
        let all_results: Vec<Vec<(String, Option<bool>)>> = stream::iter(batch_futures)
            .buffer_unordered(parallelism)
            .collect()
            .await;

        let mut missing_ids: HashSet<String> = HashSet::new();
        let mut scan_incomplete = false;
        for batch in all_results {
            for (msg_id, status) in batch {
                match status {
                    Some(true) => {}
                    Some(false) => {
                        if let Some(&(idx, bytes)) = meta.get(&msg_id) {
                            file_accs[idx].missing_bytes += bytes;
                            file_accs[idx].has_missing = true;
                        }
                        missing_ids.insert(msg_id);
                    }
                    None => {
                        scan_incomplete = true;
                        if let Some(&(idx, bytes)) = meta.get(&msg_id) {
                            file_accs[idx].unknown_bytes += bytes;
                        }
                    }
                }
            }
        }

        let mut report = AvailabilityReport {
            missing_ids,
            scan_incomplete,
            ..Default::default()
        };
        for fa in &file_accs {
            // Pessimistic for the verdict: confirmed-absent + unknown both count
            // as "not available".
            let unavailable = fa.missing_bytes + fa.unknown_bytes;
            if fa.is_par2 {
                report.has_par2 = true;
                report.total_par2_bytes += fa.total_bytes;
                report.available_par2_bytes += fa.total_bytes.saturating_sub(unavailable);
            } else {
                report.total_data_bytes += fa.total_bytes;
                report.missing_data_bytes += unavailable;
                // Only list files with CONFIRMED-missing segments (don't cry wolf
                // on a transient STAT error).
                if fa.has_missing {
                    report.missing_files.push(fa.name.clone());
                    if fa.essential {
                        report.missing_essential_files.push(fa.name.clone());
                    }
                }
            }
        }
        Ok(report)
    }

    /// Run a full NZB download. Returns the per-file results plus the
    /// transfer-window stats (duration and actual plaintext bytes pulled from
    /// the socket) needed for an accurate throughput display.
    pub async fn download_nzb(
        &self,
        nzb: &Nzb,
        config: Config,
        skip_message_ids: Option<&HashSet<String>>,
    ) -> Result<DownloadOutcome> {
        config.ensure_dirs()?;

        let files: Vec<&NzbFile> = nzb.files().iter().collect();
        if files.is_empty() {
            return Err(DownloadError::InsufficientSegments {
                available: 0,
                required: 1,
            }
            .into());
        }

        // Compute the *decoded* total. We don't know the exact decoded size
        // until each segment lands, so use the encoded total as an upper bound
        // for the progress bar and let the writer report bytes-decoded as it
        // proceeds. Reaching ~88-95% then "finishing" is normal — the bar is
        // explicitly settled to 100% on completion.
        let total_encoded: u64 = files
            .iter()
            .flat_map(|f| &f.segments.segment)
            .map(|s| s.bytes)
            .sum();

        let live_speed_bps = Arc::new(AtomicU64::new(0));
        let progress_bar =
            progress::create_download_progress_bar(total_encoded, live_speed_bps.clone());
        progress_bar.set_message(format!("(0/{})", files.len()));

        // Sampler task that publishes the live socket-read rate (with light
        // EMA smoothing) for the progress bar to display. Aborted on exit so
        // we never outlive the bar.
        let bytes_counter = self.pool.bytes_read_counter();
        let sampler = tokio::spawn(speed_sampler(bytes_counter.clone(), live_speed_bps));

        // Pre-warm pool to saturate connection budget before we start producing
        // segment jobs. Doing this here (rather than in `new`) lets the user
        // see live feedback if it's slow.
        let warmed = self.pool.warm_up(self.connections).await;
        tracing::debug!(
            "Pool warmed: {}/{} connections established",
            warmed,
            self.connections
        );

        let (results, transfer_duration, actual_wire_bytes) = self
            .run_download(
                &files,
                progress_bar.clone(),
                config,
                skip_message_ids,
                bytes_counter,
            )
            .await?;

        sampler.abort();

        // Settle the progress bar.
        let total_downloaded: u64 = results.iter().map(|r| r.size).sum();
        // Split committed bytes into payload data vs PAR2 recovery so the summary
        // is honest about how much of the download is the actual file(s) vs
        // recovery overhead (and so the wire/committed gap reads as framing, not
        // waste).
        let par2_downloaded: u64 = results
            .iter()
            .filter(|r| crate::patterns::par2::is_par2_file(&r.path))
            .map(|r| r.size)
            .sum();
        let data_downloaded = total_downloaded.saturating_sub(par2_downloaded);
        let failed_files = results.iter().filter(|r| r.segments_failed > 0).count();
        progress_bar.set_length(total_encoded.max(total_downloaded));
        progress_bar.set_position(progress_bar.length().unwrap_or(total_encoded));
        progress_bar.finish_with_message(format!("({}/{})  ", files.len(), files.len()));

        if !crate::output_mode::is_quiet() {
            let speed_suffix = format_speed_suffix(actual_wire_bytes, transfer_duration);
            let par2_suffix = if par2_downloaded > 0 {
                format!(" + {} PAR2", human_bytes(par2_downloaded as f64))
            } else {
                String::new()
            };
            if failed_files == 0 {
                println!(
                    "  └─ \x1b[32m✓ Downloaded {}{}{}\x1b[0m",
                    human_bytes(data_downloaded as f64),
                    par2_suffix,
                    speed_suffix,
                );
            } else {
                println!(
                    "  └─ \x1b[33m! Downloaded {}{} ({} file{} with errors){}\x1b[0m",
                    human_bytes(data_downloaded as f64),
                    par2_suffix,
                    failed_files,
                    if failed_files == 1 { "" } else { "s" },
                    speed_suffix,
                );
            }
        }

        Ok(DownloadOutcome {
            files: results,
            progress_bar,
            transfer_duration,
            actual_wire_bytes,
        })
    }

    /// Download an NZB with PAR2 recovery volumes deferred until needed.
    ///
    /// Phase 1 downloads the data files plus the PAR2 index; if every data
    /// segment arrives intact (each is yEnc-CRC verified on the wire), the
    /// recovery volumes are never fetched. Only when a data segment is missing
    /// or corrupt does Phase 2 download the recovery volumes for repair. With
    /// `download_all_par2 = true`, or when there is nothing to defer, this is a
    /// plain full download.
    pub async fn download_nzb_on_demand(
        &self,
        nzb: &Nzb,
        config: Config,
        skip_message_ids: Option<&HashSet<String>>,
        download_all_par2: bool,
    ) -> Result<DownloadOutcome> {
        // A "deferred" file is a PAR2 recovery volume (`.volNN+MM.par2`); the
        // PAR2 index (no `.vol`) stays in Phase 1 so we can verify/deobfuscate
        // even when the payload is intact.
        let is_deferred = |f: &NzbFile| {
            Nzb::get_filename_from_subject(&f.subject)
                .map(|n| {
                    let p = Path::new(&n);
                    crate::patterns::par2::is_par2_file(p)
                        && !crate::patterns::par2::is_main_par2(p)
                })
                .unwrap_or(false)
        };
        let deferred_count = nzb.files().iter().filter(|f| is_deferred(f)).count();

        if download_all_par2 || deferred_count == 0 {
            return self.download_nzb(nzb, config, skip_message_ids).await;
        }

        let phase1 = nzb.subset(|f| !is_deferred(f));
        let outcome1 = self
            .download_nzb(&phase1, config.clone(), skip_message_ids)
            .await?;

        // On Ctrl+C during Phase 1, don't launch a pointless Phase 2.
        if crate::shutdown::is_requested() {
            return Ok(outcome1);
        }

        // Fetch recovery if any data segment is missing/corrupt OR the PAR2 index
        // itself failed in Phase 1 — a fetched recovery volume carries the same
        // Main/FileDesc metadata, restoring verification and name recovery.
        let data_failed = outcome1
            .files
            .iter()
            .any(|r| !crate::patterns::par2::is_par2_file(&r.path) && r.segments_failed > 0);
        let index_failed = outcome1
            .files
            .iter()
            .any(|r| crate::patterns::par2::is_par2_file(&r.path) && r.segments_failed > 0);

        if !data_failed && !index_failed {
            if !crate::output_mode::is_quiet() {
                let saved: u64 = nzb
                    .files()
                    .iter()
                    .filter(|f| is_deferred(f))
                    .flat_map(|f| &f.segments.segment)
                    .map(|s| s.bytes)
                    .sum();
                println!(
                    "  └─ \x1b[90mℹ Data complete — skipped {} of PAR2 recovery\x1b[0m",
                    human_bytes(saved as f64)
                );
            }
            return Ok(outcome1);
        }

        if !crate::output_mode::is_quiet() {
            println!(
                "  \x1b[33m↻ Missing/corrupt data — fetching PAR2 recovery for repair…\x1b[0m"
            );
        }
        let phase2 = nzb.subset(is_deferred);
        let outcome2 = self.download_nzb(&phase2, config, skip_message_ids).await?;

        let mut files = outcome1.files;
        files.extend(outcome2.files);
        Ok(DownloadOutcome {
            files,
            progress_bar: outcome2.progress_bar,
            transfer_duration: outcome1.transfer_duration + outcome2.transfer_duration,
            actual_wire_bytes: outcome1.actual_wire_bytes + outcome2.actual_wire_bytes,
        })
    }

    async fn run_download(
        &self,
        files: &[&NzbFile],
        progress_bar: ProgressBar,
        config: Config,
        skip_message_ids: Option<&HashSet<String>>,
        bytes_counter: Arc<AtomicU64>,
    ) -> Result<(Vec<DownloadResult>, Duration, u64)> {
        let pipeline_depth = config.tuning.pipeline_depth.max(1);
        let decode_retry_cap = config.tuning.decode_retry_cap.max(1);
        // Transient (connection-level / 412) retries get a more generous budget
        // than article-level retries — a dropped connection usually isn't the
        // article's fault — but it is bounded so a strict or half-dead server
        // can't livelock the download.
        let max_transient_retries = (config.usenet.retry_attempts as usize)
            .max(1)
            .saturating_mul(5);
        let retry_delay = Duration::from_millis(config.usenet.retry_delay);

        // Sort files largest first so big work hits the workers immediately.
        let mut sorted_files: Vec<&NzbFile> = files.to_vec();
        sorted_files.sort_by_key(|f| Reverse(f.segments.segment.len()));

        // Build per-file state and the flat, article-granularity job list. Jobs
        // are emitted in (largest-file-first, segment-order), so consecutive
        // dequeues tend to share a group, but ANY worker can take ANY article —
        // no connection sits idle while work remains, and the tail is at most
        // one in-flight window per connection instead of a whole 50-segment job.
        let mut file_states: Vec<Arc<FileState>> = Vec::new();
        let mut jobs: Vec<ArticleJob> = Vec::new();
        let total_files = sorted_files.len();

        for file in sorted_files {
            let raw_filename = Nzb::get_filename_from_subject(&file.subject)
                .unwrap_or_else(|| format!("unknown_file_{}", file.date));
            let filename = sanitize_filename(&raw_filename, &format!("unknown_file_{}", file.date));
            let final_path = config.download.dir.join(&filename);
            let partial_path = with_extra_extension(&final_path, "partial");

            let group: Arc<str> = match file.groups.group.first() {
                Some(g) => Arc::from(g.name.as_str()),
                None => {
                    eprintln!("Missing group for {}, skipping", filename);
                    file_states.push(Arc::new(FileState::placeholder_failed(
                        filename,
                        final_path,
                        file.segments.segment.len(),
                    )));
                    continue;
                }
            };

            // Drop only the individually-missing segments flagged by the
            // pre-flight scan; every present segment still contributes its data
            // so PAR2 has the maximum number of blocks to repair from. (A file
            // is no longer voided wholesale just because its first segment is
            // gone.)
            let is_skipped = |id: &str| skip_message_ids.is_some_and(|s| s.contains(id));
            let active_segments: Vec<(String, u64)> = file
                .segments
                .segment
                .iter()
                .filter(|s| !is_skipped(&s.message_id))
                .map(|s| (s.message_id.clone(), s.bytes))
                .collect();

            if active_segments.is_empty() {
                file_states.push(Arc::new(FileState::placeholder_failed(
                    filename,
                    final_path,
                    file.segments.segment.len(),
                )));
                continue;
            }

            // Use the encoded total as the file pre-allocation upper bound.
            // The writer truncates to the highest decoded offset on completion.
            let encoded_total: u64 = active_segments.iter().map(|(_, b)| b).sum();
            let partial_file = match create_preallocated_file(&partial_path, encoded_total).await {
                Ok(f) => Arc::new(f),
                Err(e) => {
                    eprintln!("Failed to create {}: {}", partial_path.display(), e);
                    file_states.push(Arc::new(FileState::placeholder_failed(
                        filename,
                        final_path,
                        file.segments.segment.len(),
                    )));
                    continue;
                }
            };

            let state = FileState::new(filename, final_path, partial_path, active_segments.len());
            // Pre-flight-skipped segments are genuine holes in the file: seed
            // them as failures so the file reports as incomplete and the PAR2
            // on-demand path knows recovery is needed. They are NOT added to
            // `segments_total` (only attempted segments settle), so finalization
            // still completes.
            let skipped_count = file.segments.segment.len() - active_segments.len();
            if skipped_count > 0 {
                state
                    .segments_failed
                    .store(skipped_count, Ordering::Relaxed);
            }
            let state = Arc::new(state);
            file_states.push(state.clone());

            let handle = Arc::new(FileHandle {
                file: partial_file,
                state: state.clone(),
            });

            for (message_id, encoded_bytes) in active_segments {
                jobs.push(ArticleJob {
                    file: handle.clone(),
                    group: group.clone(),
                    message_id,
                    encoded_bytes,
                    decode_attempts: 0,
                    transient_attempts: 0,
                });
            }
        }

        if jobs.is_empty() {
            return Ok((
                file_states.iter().map(|s| s.snapshot()).collect(),
                Duration::ZERO,
                0,
            ));
        }

        // Article-granularity work distribution:
        //   - `job_tx`/`job_rx` is a lock-free MPMC queue (flume); every worker
        //     pulls directly with no shared mutex.
        //   - Workers report each article's terminal outcome to the coordinator:
        //     `Settled` (Ok or permanent failure) or `Retry(job)` (transient
        //     wire error, or a bounded decode retry). Only the coordinator owns
        //     `job_tx`, so re-queues are centralized and completion is exact.
        //   - When `pending` hits zero, `job_tx` is dropped, closing the queue
        //     so idle workers wake and exit cleanly.
        let total_articles = jobs.len();
        let (job_tx, job_rx) = flume::unbounded::<ArticleJob>();
        let (feedback_tx, mut feedback_rx) = mpsc::unbounded_channel::<WorkerFeedback>();
        // Bounded write channel sized for decode bursts (independent of pipeline
        // depth) so workers block if the writer falls behind, capping RAM held
        // in decoded `Bytes`.
        let write_capacity = (self.connections.max(1) * 64).max(64);
        let (write_tx, write_rx) = mpsc::channel::<WriteJob>(write_capacity);

        for job in jobs.drain(..) {
            let _ = job_tx.send(job);
        }

        let writer_handle = tokio::spawn(run_writer(
            write_rx,
            progress_bar.clone(),
            total_files,
            bytes_counter,
        ));

        let worker_count = self.connections.max(1);
        let ctx = Arc::new(WorkerCtx {
            pool: self.pool.clone(),
            job_rx,
            feedback_tx,
            write_tx,
            progress: progress_bar.clone(),
            pipeline_depth,
            decode_retry_cap,
            max_transient_retries,
            retry_delay,
        });
        let mut worker_handles = Vec::with_capacity(worker_count);
        for worker_id in 0..worker_count {
            let ctx = ctx.clone();
            worker_handles.push(tokio::spawn(worker_loop(worker_id, ctx)));
        }
        drop(ctx);

        // Coordinate completions and retry re-queues. When `pending` hits 0,
        // drop `job_tx` so workers exit.
        let mut pending = total_articles;
        let mut job_tx_opt = Some(job_tx);
        while pending > 0 {
            match feedback_rx.recv().await {
                Some(WorkerFeedback::Settled) => {
                    pending -= 1;
                }
                Some(WorkerFeedback::Retry(job)) => {
                    if let Some(tx) = job_tx_opt.as_ref() {
                        if tx.send(job).is_err() {
                            // Receiver gone — workers must have exited.
                            break;
                        }
                    } else {
                        // Already closed; treat as settled.
                        pending -= 1;
                    }
                }
                None => {
                    // All workers exited without finishing all jobs (shouldn't
                    // happen under normal flow). Break and let cleanup proceed.
                    break;
                }
            }
        }
        drop(job_tx_opt.take());

        for handle in worker_handles {
            if let Err(e) = handle.await {
                tracing::error!("Download worker panicked: {}", e);
            }
        }
        let (transfer_duration, actual_wire_bytes) = match writer_handle.await {
            Ok(Some(window)) => (
                window.end.duration_since(window.start),
                window.bytes_at_end.saturating_sub(window.bytes_at_start),
            ),
            Ok(None) => (Duration::ZERO, 0),
            Err(e) => {
                tracing::error!("Writer task panicked: {}", e);
                (Duration::ZERO, 0)
            }
        };

        Ok((
            file_states.iter().map(|s| s.snapshot()).collect(),
            transfer_duration,
            actual_wire_bytes,
        ))
    }
}

enum WorkerFeedback {
    /// Article reached a terminal state (Ok or permanent failure).
    Settled,
    /// Article should be retried (transient wire error, or a bounded decode retry).
    Retry(ArticleJob),
}

struct WorkerCtx {
    pool: NntpPool,
    job_rx: flume::Receiver<ArticleJob>,
    feedback_tx: mpsc::UnboundedSender<WorkerFeedback>,
    write_tx: Sender<WriteJob>,
    progress: ProgressBar,
    /// How many `BODY` requests each connection keeps in flight (sliding window).
    pipeline_depth: usize,
    /// Max retries for a yEnc decode failure before giving up on the article.
    decode_retry_cap: u8,
    /// Max retries for transient (connection-level / 412) failures.
    max_transient_retries: usize,
    retry_delay: Duration,
}

// --- Job and state types ---

/// One article (segment) of work. Articles are the unit of distribution AND of
/// retry: a transient failure re-queues only this one article, never a batch.
struct ArticleJob {
    file: Arc<FileHandle>,
    group: Arc<str>,
    message_id: String,
    /// Encoded size from the NZB; the writer increments the progress bar by this
    /// once the article reaches a terminal state (success or permanent failure).
    encoded_bytes: u64,
    /// Bounded retries for decode (CRC/size) failures.
    decode_attempts: u8,
    /// Bounded retries for transient (connection-level / 412) failures. Given a
    /// generous budget because these usually mean connection flakiness rather
    /// than a bad article, but bounded so a strict/half-dead server can't
    /// livelock the download.
    transient_attempts: u8,
}

struct FileHandle {
    file: Arc<StdFile>,
    state: Arc<FileState>,
}

struct FileState {
    filename: String,
    final_path: PathBuf,
    partial_path: PathBuf,
    segments_total: usize,
    segments_downloaded: AtomicUsize,
    segments_failed: AtomicUsize,
    segments_settled: AtomicUsize,
    max_byte_position: AtomicU64,
    /// Set once when the partial file has been renamed (or deleted on full
    /// failure). Idempotency guard for `finalize_file`.
    finalized: AtomicBool,
}

impl FileState {
    fn new(
        filename: String,
        final_path: PathBuf,
        partial_path: PathBuf,
        segments_total: usize,
    ) -> Self {
        Self {
            filename,
            final_path,
            partial_path,
            segments_total,
            segments_downloaded: AtomicUsize::new(0),
            segments_failed: AtomicUsize::new(0),
            segments_settled: AtomicUsize::new(0),
            max_byte_position: AtomicU64::new(0),
            finalized: AtomicBool::new(false),
        }
    }

    fn placeholder_failed(filename: String, final_path: PathBuf, total: usize) -> Self {
        let s = Self::new(filename, final_path.clone(), final_path, total);
        s.segments_failed.store(total, Ordering::Relaxed);
        s.segments_settled.store(total, Ordering::Relaxed);
        s.finalized.store(true, Ordering::Relaxed);
        s
    }

    /// Increment the settle count and return true exactly once when it reaches
    /// `segments_total`. The single atomic exchange means no two callers can
    /// observe the transition simultaneously.
    fn mark_settled(&self) -> bool {
        let prev = self.segments_settled.fetch_add(1, Ordering::AcqRel);
        prev + 1 == self.segments_total
    }

    fn snapshot(&self) -> DownloadResult {
        DownloadResult {
            filename: self.filename.clone(),
            path: self.final_path.clone(),
            size: self.max_byte_position.load(Ordering::Relaxed),
            segments_downloaded: self.segments_downloaded.load(Ordering::Relaxed),
            segments_failed: self.segments_failed.load(Ordering::Relaxed),
        }
    }
}

enum WriteJob {
    Write {
        file: Arc<FileHandle>,
        offset: u64,
        data: Bytes,
        encoded_size: u64,
    },
    Failed {
        file: Arc<FileHandle>,
        message_id: String,
        encoded_size: u64,
    },
}

// --- Worker ---

/// A download worker owns one connection and runs a continuous sliding window of
/// up to `pipeline_depth` in-flight `BODY` requests, refilling as each response
/// is drained so the connection never idles between articles (no drain-then-
/// refill gap). Articles are pulled from the shared MPMC queue, so a worker that
/// empties its window immediately steals more work — no connection sits idle
/// while any article remains, and the tail is bounded by the window, not by a
/// whole 50-segment job.
async fn worker_loop(worker_id: usize, ctx: Arc<WorkerCtx>) {
    let depth = ctx.pipeline_depth.max(1);
    let mut connection: Option<PooledConnection> = None;
    let mut group_ready = false;
    let mut inflight: VecDeque<ArticleJob> = VecDeque::with_capacity(depth);

    loop {
        // Graceful shutdown: fail everything we hold and drain the queue so file
        // states settle (and finalize) instead of hanging the coordinator.
        if crate::shutdown::is_requested() {
            for job in inflight.drain(..) {
                fail_article(&job, &ctx).await;
            }
            while let Ok(job) = ctx.job_rx.try_recv() {
                fail_article(&job, &ctx).await;
            }
            if let Some(c) = connection.take() {
                c.release();
            }
            break;
        }

        // Acquire a connection if we don't have one.
        if connection.is_none() {
            match acquire_connection(&ctx.pool, ctx.retry_delay, &ctx.progress, worker_id).await {
                Some(c) => {
                    connection = Some(c);
                    group_ready = false;
                }
                None => {
                    // Connectivity is gone. Fail anything we hold plus one queued
                    // article so the download terminates rather than hanging,
                    // then retry acquiring on the next iteration.
                    for job in inflight.drain(..) {
                        fail_article(&job, &ctx).await;
                    }
                    match ctx.job_rx.try_recv() {
                        Ok(job) => fail_article(&job, &ctx).await,
                        Err(flume::TryRecvError::Empty) => match ctx.job_rx.recv_async().await {
                            Ok(job) => fail_article(&job, &ctx).await,
                            Err(_) => break,
                        },
                        Err(flume::TryRecvError::Disconnected) => break,
                    }
                    continue;
                }
            }
        }

        let mut dead = false;
        let mut bail = false;

        // Fill the sliding window. Block only when nothing is in flight (an idle
        // worker waits for work); otherwise top up non-blocking and go drain.
        while inflight.len() < depth {
            let next = if inflight.is_empty() {
                ctx.job_rx.recv_async().await.ok()
            } else {
                ctx.job_rx.try_recv().ok()
            };
            let Some(job) = next else {
                if inflight.is_empty() {
                    // Queue closed and nothing in flight — this worker is done.
                    if let Some(c) = connection.take() {
                        c.release();
                    }
                    return;
                }
                break;
            };

            if crate::shutdown::is_requested() {
                // Hand the job to the shutdown drainer at the top of the loop.
                inflight.push_back(job);
                bail = true;
                break;
            }

            // Select the group once per connection (best-effort; `BODY <id>` is
            // group-independent on compliant servers).
            if !group_ready {
                let _ = connection.as_mut().unwrap().ensure_group(&job.group).await;
                group_ready = true;
                if connection.as_ref().unwrap().is_poisoned() {
                    retry_transient(job, &ctx).await;
                    dead = true;
                    break;
                }
            }

            if connection
                .as_mut()
                .unwrap()
                .send_body(&job.message_id)
                .await
                .is_err()
            {
                retry_transient(job, &ctx).await;
                dead = true;
                break;
            }
            inflight.push_back(job);
        }

        if bail {
            continue;
        }

        // Flush queued requests, then drain exactly one response (the oldest
        // in-flight) before looping to refill — this keeps the window full.
        if !dead {
            if connection.as_mut().unwrap().flush().await.is_err() {
                dead = true;
            } else if let Some(job) = inflight.pop_front() {
                let conn = connection.as_mut().unwrap();
                let outcome = conn.read_body_outcome(&job.message_id).await;
                let poisoned = conn.is_poisoned();
                match outcome {
                    ArticleOutcome::Transient { .. } => {
                        if poisoned {
                            // The connection died mid-stream; it and the rest of
                            // the window are re-queued and re-acquired below.
                            dead = true;
                        } else {
                            // Connection alive but the server refused the BODY
                            // (e.g. 412 "no newsgroup selected"): force a group
                            // re-selection so a recoverable case recovers. The
                            // counted retry below stops a strict server that
                            // never serves from looping forever.
                            group_ready = false;
                        }
                        retry_transient(job, &ctx).await;
                    }
                    other => handle_outcome(job, other, &ctx).await,
                }
            }
        }

        if dead {
            // Drop the dead connection and re-queue any still-in-flight requests
            // (their unread responses are unreliable) for a fresh connection.
            if let Some(c) = connection.take() {
                c.release();
            }
            group_ready = false;
            for job in inflight.drain(..) {
                retry_transient(job, &ctx).await;
            }
        }
    }
}

/// Settle an article as a permanent failure: record it for the file and tell
/// the coordinator this article is done.
async fn fail_article(job: &ArticleJob, ctx: &WorkerCtx) {
    let _ = ctx
        .write_tx
        .send(WriteJob::Failed {
            file: job.file.clone(),
            message_id: job.message_id.clone(),
            encoded_size: job.encoded_bytes,
        })
        .await;
    let _ = ctx.feedback_tx.send(WorkerFeedback::Settled);
}

/// Re-queue an article after a transient (connection-level / 412) failure,
/// counting it against a generous budget. Re-enqueue is immediate (no inline
/// sleep that would stall the connection's pipeline window); pacing comes from
/// connection re-acquisition and the group re-selection round-trip.
async fn retry_transient(mut job: ArticleJob, ctx: &WorkerCtx) {
    job.transient_attempts = job.transient_attempts.saturating_add(1);
    if job.transient_attempts as usize > ctx.max_transient_retries {
        fail_article(&job, ctx).await;
    } else {
        let _ = ctx.feedback_tx.send(WorkerFeedback::Retry(job));
    }
}

/// Apply a non-transient article outcome: write good data, permanently fail
/// missing articles, and bounded-retry decode failures. (Transient outcomes are
/// handled in the worker loop, where the connection's poison state is known.)
async fn handle_outcome(mut job: ArticleJob, outcome: ArticleOutcome, ctx: &WorkerCtx) {
    match outcome {
        ArticleOutcome::Ok { offset, data, .. } => {
            let _ = ctx
                .write_tx
                .send(WriteJob::Write {
                    file: job.file.clone(),
                    offset,
                    data,
                    encoded_size: job.encoded_bytes,
                })
                .await;
            let _ = ctx.feedback_tx.send(WorkerFeedback::Settled);
        }
        ArticleOutcome::Missing { .. } => {
            fail_article(&job, ctx).await;
        }
        ArticleOutcome::DecodeFailed { .. } => {
            job.decode_attempts = job.decode_attempts.saturating_add(1);
            if job.decode_attempts >= ctx.decode_retry_cap {
                fail_article(&job, ctx).await;
            } else {
                // Re-queue immediately; the cap bounds the loop without an inline
                // sleep that would stall the other in-flight responses.
                let _ = ctx.feedback_tx.send(WorkerFeedback::Retry(job));
            }
        }
        ArticleOutcome::Transient { .. } => {
            // Unreachable: handled by the worker loop. Re-queue defensively.
            retry_transient(job, ctx).await;
        }
    }
}

async fn acquire_connection(
    pool: &NntpPool,
    retry_delay: Duration,
    progress: &ProgressBar,
    worker_id: usize,
) -> Option<crate::nntp::PooledConnection> {
    let mut attempt: u32 = 0;
    let start = Instant::now();
    loop {
        // Bail promptly on Ctrl+C instead of riding out the full retry/backoff
        // (or a 30s pool-connect timeout) — the caller's `None` branch drains
        // and fails its jobs so the download terminates without the 10s backstop.
        if crate::shutdown::is_requested() {
            return None;
        }
        match pool.get_connection().await {
            Ok(conn) => return Some(conn),
            Err(e) => {
                attempt = attempt.saturating_add(1);
                tracing::debug!("Worker {} connection acquire failed: {}", worker_id, e);

                if attempt >= 6 || crate::shutdown::is_requested() {
                    return None;
                }
                if attempt % 3 == 0 && !progress.is_hidden() {
                    progress.println(format!(
                        "  \x1b[90m⏳ Worker {} waiting for connection ({:.0}s)\x1b[0m",
                        worker_id,
                        start.elapsed().as_secs_f64()
                    ));
                }
                let delay = backoff_delay(retry_delay, attempt as usize);
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn backoff_delay(base: Duration, attempt: usize) -> Duration {
    let factor = 1u64 << attempt.min(4) as u64;
    let computed = base.saturating_mul(factor as u32);
    let max = Duration::from_secs(8);
    if computed > max {
        max
    } else if computed < Duration::from_millis(100) {
        Duration::from_millis(100)
    } else {
        computed
    }
}

// --- Writer ---

/// Transfer window captured by the writer: timestamps and counter snapshots
/// taken when the first and last `WriteJob` are processed. The byte delta
/// gives an accurate denominator for the average speed because it counts
/// plaintext socket bytes (yEnc framing, escapes, NNTP overhead — everything
/// except TLS/TCP/IP headers).
struct TransferWindow {
    start: Instant,
    end: Instant,
    bytes_at_start: u64,
    bytes_at_end: u64,
}

/// Returns the transfer window or `None` if no segments were processed
/// (empty NZB or shutdown before any segment arrived).
async fn run_writer(
    mut rx: mpsc::Receiver<WriteJob>,
    progress_bar: ProgressBar,
    total_files: usize,
    bytes_counter: Arc<AtomicU64>,
) -> Option<TransferWindow> {
    let mut completed_files = 0usize;
    let mut window: Option<TransferWindow> = None;
    let mut finalize_tasks = tokio::task::JoinSet::new();
    while let Some(job) = rx.recv().await {
        let now = Instant::now();
        let bytes = bytes_counter.load(Ordering::Relaxed);
        match &mut window {
            Some(w) => {
                w.end = now;
                w.bytes_at_end = bytes;
            }
            None => {
                window = Some(TransferWindow {
                    start: now,
                    end: now,
                    bytes_at_start: bytes,
                    bytes_at_end: bytes,
                });
            }
        }

        let (file, encoded_size) = match job {
            WriteJob::Write {
                file,
                offset,
                data,
                encoded_size,
            } => {
                handle_write(&file, offset, data).await;
                (file, encoded_size)
            }
            WriteJob::Failed {
                file,
                message_id,
                encoded_size,
            } => {
                tracing::debug!("Failed segment {} in {}", message_id, file.state.filename);
                file.state.segments_failed.fetch_add(1, Ordering::Relaxed);
                (file, encoded_size)
            }
        };

        progress_bar.inc(encoded_size);

        if file.state.mark_settled() {
            completed_files += 1;
            progress_bar.set_message(format!("({}/{})", completed_files, total_files));
            // Finalize off the writer task: the `set_len` + `sync_data` +
            // `rename` chain is slow (tens of ms) and would otherwise stall
            // writes to *other* files queued behind us.
            finalize_tasks.spawn(finalize_file(file, progress_bar.clone()));
        }
    }
    while finalize_tasks.join_next().await.is_some() {}
    window
}

/// Periodic sampler that publishes the live socket-read rate to the progress
/// bar. Uses an exponential moving average so the displayed number doesn't
/// jitter every 250 ms; `instant_weight` of 0.4 means each new sample is 40%
/// of the published value, the previous EMA 60%.
async fn speed_sampler(bytes_counter: Arc<AtomicU64>, live_speed_bps: Arc<AtomicU64>) {
    const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
    const INSTANT_WEIGHT: f64 = 0.4;

    let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval.tick().await; // first tick fires immediately, skip it

    let mut last_t = Instant::now();
    let mut last_b = bytes_counter.load(Ordering::Relaxed);
    let mut ema: f64 = 0.0;
    loop {
        interval.tick().await;
        let now = Instant::now();
        let cur = bytes_counter.load(Ordering::Relaxed);
        let dt = now.duration_since(last_t).as_secs_f64();
        if dt >= 0.05 {
            let instant = (cur.saturating_sub(last_b) as f64) / dt;
            ema = if ema == 0.0 {
                instant
            } else {
                INSTANT_WEIGHT * instant + (1.0 - INSTANT_WEIGHT) * ema
            };
            live_speed_bps.store(ema.to_bits(), Ordering::Relaxed);
        }
        last_t = now;
        last_b = cur;
    }
}

async fn handle_write(file: &Arc<FileHandle>, offset: u64, data: Bytes) {
    let data_len = data.len() as u64;
    let file_handle = file.file.clone();
    let path = file.state.partial_path.clone();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let written = write_at(file_handle.as_ref(), data.as_ref(), offset)?;
        if written != data_len as usize {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "short write",
            ));
        }
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => {
            file.state
                .segments_downloaded
                .fetch_add(1, Ordering::Relaxed);
            file.state
                .max_byte_position
                .fetch_max(offset + data_len, Ordering::Relaxed);
        }
        Ok(Err(e)) => {
            file.state.segments_failed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("write to {}: {}", path.display(), e);
        }
        Err(e) => {
            file.state.segments_failed.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("write task panicked for {}: {}", path.display(), e);
        }
    }
}

/// Truncate the partial file to the highest written byte and rename it to its
/// final name. If nothing was written, remove the partial file.
async fn finalize_file(file: Arc<FileHandle>, progress_bar: ProgressBar) {
    // On Ctrl+C, leave the file as `<name>.partial` rather than truncating it to
    // the bytes received so far and renaming it to its final name — that would
    // present a silently-incomplete file as complete. A later run can resume it.
    if crate::shutdown::is_requested() {
        return;
    }
    if file.state.finalized.swap(true, Ordering::AcqRel) {
        return;
    }
    let max_byte = file.state.max_byte_position.load(Ordering::Relaxed);
    let file_handle = file.file.clone();
    let partial = file.state.partial_path.clone();
    let final_path = file.state.final_path.clone();
    let filename = file.state.filename.clone();
    let segments_failed = file.state.segments_failed.load(Ordering::Relaxed);

    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        if max_byte == 0 {
            // Nothing was written; remove the empty partial.
            drop(file_handle);
            let _ = std::fs::remove_file(&partial);
            return Ok(());
        }
        file_handle.set_len(max_byte)?;
        file_handle.sync_data()?;
        drop(file_handle);
        if final_path.exists() {
            std::fs::remove_file(&final_path)?;
        }
        std::fs::rename(&partial, &final_path)?;
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => {
            if segments_failed > 0 && !progress_bar.is_hidden() {
                progress_bar.println(format!(
                    "  \x1b[33m⚠ {} ({} missing segments)\x1b[0m",
                    filename, segments_failed
                ));
            }
        }
        Ok(Err(e)) => {
            tracing::warn!("Finalize failed for {}: {}", filename, e);
        }
        Err(e) => {
            tracing::warn!("Finalize task panicked for {}: {}", filename, e);
        }
    }
}

async fn create_preallocated_file(path: &Path, size: u64) -> Result<std::fs::File> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<StdFile> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(&path)?;
        // Pre-allocate (sparse) to the upper bound of segment data. Final size
        // is set later by `set_len(max_byte_position)`.
        file.set_len(size)?;
        Ok(file)
    })
    .await
    .map_err(std::io::Error::other)?
    .map_err(DlNzbError::from)
}

fn sanitize_filename(raw: &str, fallback: &str) -> String {
    let name = std::path::Path::new(raw)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let mut sanitized = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_control() {
            continue;
        }
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => sanitized.push('_'),
            _ => sanitized.push(ch),
        }
    }
    let trimmed = sanitized.trim().trim_matches('.');
    let mut result = if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        fallback.to_string()
    } else {
        trimmed.to_string()
    };

    let max_len = 240usize;
    if result.len() > max_len {
        let mut end = 0usize;
        for (idx, _) in result.char_indices() {
            if idx > max_len {
                break;
            }
            end = idx;
        }
        if end > 0 {
            result.truncate(end);
        }
    }
    result
}

/// Append an additional extension component to a path (foo.rar -> foo.rar.partial).
fn with_extra_extension(path: &Path, extra: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(extra);
    PathBuf::from(s)
}

/// Format ` at <N> MiB/s` for the summary, or empty if `duration` is too
/// short to be meaningful (avoids division-by-near-zero artifacts).
fn format_speed_suffix(wire_bytes: u64, duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs < 0.05 || wire_bytes == 0 {
        return String::new();
    }
    let mib_per_sec = (wire_bytes as f64) / 1_048_576.0 / secs;
    format!(" at {:.1} MiB/s", mib_per_sec)
}

#[cfg(unix)]
fn write_at(file: &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(buf, offset)
}

#[cfg(windows)]
fn write_at(file: &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buf, offset)
}

#[cfg(not(any(unix, windows)))]
fn write_at(file: &std::fs::File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    use std::io::{Seek, SeekFrom, Write};
    let mut cloned = file.try_clone()?;
    cloned.seek(SeekFrom::Start(offset))?;
    cloned.write(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filename_strips_separators() {
        // file_name() strips path prefix; remaining illegal chars are replaced.
        assert_eq!(sanitize_filename("a/b<c>.mkv", "fallback"), "b_c_.mkv");
        assert_eq!(
            sanitize_filename("name:with*illegal?chars.mkv", "fb"),
            "name_with_illegal_chars.mkv"
        );
        assert_eq!(sanitize_filename("..", "fallback"), "fallback");
        assert_eq!(sanitize_filename("", "fallback"), "fallback");
        // Control characters are stripped silently.
        assert_eq!(sanitize_filename("clean.mkv", "fb"), "clean.mkv");
    }

    #[test]
    fn with_extra_extension_appends() {
        let p = PathBuf::from("/tmp/a.mkv");
        assert_eq!(
            with_extra_extension(&p, "partial"),
            PathBuf::from("/tmp/a.mkv.partial")
        );
    }

    #[test]
    fn backoff_increases_then_caps() {
        let base = Duration::from_millis(500);
        let d1 = backoff_delay(base, 1);
        let d2 = backoff_delay(base, 2);
        let d5 = backoff_delay(base, 5);
        assert!(d2 >= d1);
        assert!(d5 <= Duration::from_secs(8));
    }
}
