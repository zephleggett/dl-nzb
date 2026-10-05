//! NZB downloader.
//!
//! Segments are written at the offset reported by yEnc `=ypart`, so the
//! on-disk layout has no gaps regardless of how the NZB labelled segment
//! sizes. Articles are distributed through a lock-free MPMC queue; retries
//! flow back through a feedback channel to the coordinator, which re-enqueues
//! them and closes the queue when all work has settled.
//!
//! Every download runs inside a job context ([`JobCtx`]): workers race their
//! work against its cancellation token (so a stop never waits out a read
//! timeout), obey its pause signal, attribute wire bytes to the job, and the
//! writer reports progress through it. For an engine job the writer also
//! records every article it wrote (and every one given up) in the job's
//! resume record, and a resumed download skips what that record says is
//! already on disk. No terminal output happens here.

use bytes::Bytes;
use tokio::sync::mpsc::{self, Sender};
use tokio_util::sync::CancellationToken;

use std::cmp::Reverse;
use std::collections::{HashSet, VecDeque};
use std::fs::File as StdFile;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::nzb::{Nzb, NzbFile};
use crate::config::Config;
use crate::engine::context::JobCtx;
use crate::engine::sidecar::{JobRecord, Recovery, SegmentSet};
use crate::engine::JobPhase;
use crate::error::DlNzbError;
use crate::nntp::{
    ArticleOutcome, NntpPool, NntpPoolBuilder, NntpPoolExt, PooledConnection, SegmentRequest,
};
use crate::patterns::partial_path;

type Result<T> = std::result::Result<T, DlNzbError>;

/// Per-file result reported back to the caller.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub filename: String,
    pub path: PathBuf,
    /// Final size of the file on disk after truncation (decoded bytes).
    pub size: u64,
    /// Articles the NZB lists for this file.
    pub segments_total: usize,
    pub segments_downloaded: usize,
    pub segments_failed: usize,
    /// True iff every downloaded segment of this file carried a yEnc checksum
    /// that matched on the wire — i.e. the file is fully integrity-verified
    /// without a PAR2 re-hash. False if any segment was size-checked only.
    pub all_segments_crc_verified: bool,
}

/// Aggregate outcome of a download. Speed reporting uses
/// `actual_wire_bytes / transfer_duration`, which reflects plaintext bytes
/// pulled from the socket and lines up much more closely with what external
/// network monitors and other NZB clients display.
pub struct DownloadOutcome {
    pub files: Vec<DownloadResult>,
    /// From the moment the workers start (each then connects) to the
    /// last article written; summed over phases by the on-demand download.
    pub transfer_duration: Duration,
    /// Plaintext bytes received across all connections during the transfer
    /// window. Includes yEnc framing/escape sequences, NNTP command/response
    /// overhead, and any retry traffic — the sum of `segment.bytes` from the
    /// NZB is the encoded payload and runs ~2-4% below this in the steady state.
    pub actual_wire_bytes: u64,
}

impl DownloadOutcome {
    /// Files settled without the network (nothing left to fetch, or finished
    /// in an earlier session): no transfer to report.
    fn local(files: Vec<DownloadResult>) -> Self {
        Self {
            files,
            transfer_duration: Duration::ZERO,
            actual_wire_bytes: 0,
        }
    }
}

/// Result of a pre-flight `STAT`-all availability scan. Byte tallies use the
/// NZB's encoded segment sizes, which are proportional to PAR2 block counts, so
/// comparing missing data bytes against available recovery bytes is a good
/// repairability estimate (a recovery block reconstructs one same-sized data
/// block). It's an estimate, not a guarantee: a present article can still fail
/// its yEnc CRC at download time.
#[derive(Debug, Default)]
pub struct AvailabilityReport {
    /// Articles checked (every segment of every file with a newsgroup).
    pub articles_total: u64,
    pub total_data_bytes: u64,
    pub missing_data_bytes: u64,
    pub available_par2_bytes: u64,
    /// All missing article ids — handed to the downloader so they aren't fetched.
    pub missing_ids: HashSet<String>,
    /// Names of data files with at least one CONFIRMED-missing segment.
    pub missing_files: Vec<String>,
    pub has_par2: bool,
    /// Some segments could not be STAT-checked (no connection could, or the
    /// server's reply said nothing). Their bytes are counted pessimistically
    /// in `missing_data_bytes`/`available_par2_bytes`, so the percentages are
    /// a conservative floor.
    pub scan_incomplete: bool,
    /// Data bytes the scan could not check (included in `missing_data_bytes`).
    pub unknown_data_bytes: u64,
    /// PAR2 bytes the scan could not check (left out of `available_par2_bytes`).
    pub unknown_par2_bytes: u64,
}

impl AvailabilityReport {
    /// True if every missing data file is non-essential (.nfo/.sfv/.srr only).
    pub fn only_nonessential_missing(&self) -> bool {
        !self.missing_files.is_empty()
            && self
                .missing_files
                .iter()
                .all(|name| crate::patterns::is_auxiliary_name(name))
    }

    /// Whether the available PAR2 recovery can likely repair the missing data.
    pub fn likely_repairable(&self) -> bool {
        self.covers(self.missing_data_bytes, self.available_par2_bytes)
    }

    /// Whether the recovery data could cover what is confirmed missing if
    /// every article the scan couldn't check is there. The same as
    /// [`likely_repairable`](Self::likely_repairable) when the scan checked
    /// everything; when only this holds, the unchecked articles decide.
    pub fn possibly_repairable(&self) -> bool {
        self.covers(
            self.missing_data_bytes
                .saturating_sub(self.unknown_data_bytes),
            self.available_par2_bytes + self.unknown_par2_bytes,
        )
    }

    /// Whether `available` recovery bytes can repair `missing` data bytes.
    /// Recovery blocks are the same size as data blocks, so they must cover
    /// the missing bytes; a 10% headroom absorbs block-alignment
    /// over-counting.
    fn covers(&self, missing: u64, available: u64) -> bool {
        missing == 0 || (self.has_par2 && available as f64 >= missing as f64 * 1.1)
    }
}

/// The connections a phase's workers (download or scan) hold and have held.
/// A worker that can't connect while another has (the provider allows fewer
/// connections than configured) leaves the work to the others.
#[derive(Default)]
struct Census {
    /// Workers holding a connection right now.
    connected: AtomicUsize,
    /// Connections workers have checked out so far.
    acquired: AtomicU64,
}

impl Census {
    /// Connections checked out so far, for
    /// [`others_connected`](Self::others_connected).
    fn acquired(&self) -> u64 {
        self.acquired.load(Ordering::Acquire)
    }

    /// Whether another worker reached the server: one holds a connection
    /// now, or got one since `acquired_before` was read.
    fn others_connected(&self, acquired_before: u64) -> bool {
        self.connected.load(Ordering::Acquire) > 0
            || self.acquired.load(Ordering::Acquire) > acquired_before
    }

    /// Count a connection just checked out, held until the guard is dropped.
    fn hold(&self) -> Holding<'_> {
        self.connected.fetch_add(1, Ordering::AcqRel);
        self.acquired.fetch_add(1, Ordering::AcqRel);
        Holding(&self.connected)
    }
}

/// A connection counted in [`Census::connected`] while this lives.
struct Holding<'a>(&'a AtomicUsize);

impl Drop for Holding<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The availability scan's `STAT` batches still to send, shared by its
/// workers, and how many of them reached the server.
struct StatQueue {
    batches: std::sync::Mutex<VecDeque<StatBatch>>,
    census: Census,
}

struct StatBatch {
    requests: Vec<SegmentRequest>,
    /// Times a connection failed while sending or reading it.
    failures: u8,
}

/// One batch's results: each article's id and whether it exists (`None`:
/// unknown).
type StatResults = Vec<(String, Option<bool>)>;

/// How many times a scan worker tries for a connection while no other has one.
const STAT_CONNECT_ATTEMPTS: u32 = 3;
/// The first wait between those tries (it doubles).
const STAT_RETRY_DELAY: Duration = Duration::from_millis(100);
/// How many connections a batch is tried on before its articles count as
/// unknown.
const STAT_BATCH_TRIES: u8 = 2;

impl StatQueue {
    fn new(batches: impl Iterator<Item = Vec<SegmentRequest>>) -> Self {
        Self {
            batches: std::sync::Mutex::new(
                batches
                    .map(|requests| StatBatch {
                        requests,
                        failures: 0,
                    })
                    .collect(),
            ),
            census: Census::default(),
        }
    }

    fn pop(&self) -> Option<StatBatch> {
        self.batches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }

    fn push(&self, batch: StatBatch) {
        self.batches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(batch);
    }
}

/// Every article of `batch` as unknown.
fn unknown(batch: StatBatch) -> StatResults {
    batch
        .requests
        .into_iter()
        .map(|r| (r.message_id, None))
        .collect()
}

/// One worker of the availability scan: takes a connection and sends `STAT`
/// batches from `queue` until it is empty, handing each batch's results to
/// `results`. Like a download worker, it uses only the connections it can
/// get: one that can't connect while another worker has (the provider allows
/// fewer connections than configured) leaves the batches to the others, as a
/// `STAT` never sent says nothing about an article. Only when no worker can
/// connect do the batches left count as unknown. A connection that fails
/// mid-batch is replaced, and the batch tried once more.
async fn stat_worker(
    pool: &NntpPool,
    job: &JobCtx,
    queue: &StatQueue,
    results: mpsc::UnboundedSender<StatResults>,
) {
    'connect: loop {
        let acquired_before = queue.census.acquired();
        let mut attempt = 0u32;
        let mut conn = loop {
            match pool.get_connection().await {
                Ok(conn) => break conn,
                Err(e) => {
                    tracing::debug!("STAT connection failed: {}", e);
                    attempt += 1;
                    if queue.census.others_connected(acquired_before) {
                        return;
                    }
                    if attempt >= STAT_CONNECT_ATTEMPTS {
                        while let Some(batch) = queue.pop() {
                            let _ = results.send(unknown(batch));
                        }
                        return;
                    }
                    tokio::time::sleep(backoff_delay(STAT_RETRY_DELAY, attempt as usize)).await;
                }
            }
        };
        let _held = queue.census.hold();
        while let Some(mut batch) = queue.pop() {
            match conn.check_articles_exist(&batch.requests).await {
                Ok(checked) => {
                    job.add_wire_bytes(conn.take_bytes_read());
                    let _ = results.send(checked);
                }
                Err(e) => {
                    tracing::debug!("STAT batch failed: {}", e);
                    batch.failures += 1;
                    if batch.failures < STAT_BATCH_TRIES {
                        queue.push(batch);
                    } else {
                        let _ = results.send(unknown(batch));
                    }
                    // The connection is poisoned: it is dropped, and another taken.
                    continue 'connect;
                }
            }
        }
        return;
    }
}

pub struct Downloader {
    pool: NntpPool,
    /// Concurrent connection budget. The number of workers we spawn during a
    /// download, each with its own connection.
    connections: usize,
    /// The job this downloader works for: cancellation, pause, progress.
    job: Arc<JobCtx>,
}

impl Downloader {
    /// A standalone downloader with its own pool, outside any engine job
    /// (nothing observes it and it is never paused or stopped).
    pub async fn new(config: Config) -> Result<Self> {
        let connections = config.usenet.connections as usize;
        let pool = NntpPoolBuilder::new(config.usenet.clone())
            .max_concurrent_connections(config.tuning.max_concurrent_connections)
            .build()?;

        // Eagerly establish one connection so credential / DNS / TLS errors
        // surface immediately, with their real cause, rather than midway
        // through downloading. Dropping it returns it to the pool.
        drop(pool.get_connection().await?);

        Ok(Self::for_job(pool, connections, JobCtx::detached()))
    }

    /// A downloader working for an engine job, on the engine's shared pool.
    pub(crate) fn for_job(pool: NntpPool, connections: usize, job: Arc<JobCtx>) -> Self {
        Self {
            pool,
            connections: connections.max(1),
            job,
        }
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
    pub async fn check_all_availability(&self, nzb: &Nzb) -> AvailabilityReport {
        // Bail immediately if the job was already stopped.
        if self.job.is_cancelled() {
            return AvailabilityReport {
                scan_incomplete: true,
                ..Default::default()
            };
        }
        let files: Vec<&NzbFile> = nzb.files().iter().collect();

        struct FileAcc {
            name: String,
            is_par2: bool,
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
        let mut missing_ids: HashSet<String> = HashSet::new();
        let mut malformed = 0u64;

        for file in &files {
            let filename = file.filename.clone();
            let is_par2 = file.is_par2();
            let group = file.groups.first().cloned();
            let idx = file_accs.len();
            let total_bytes = file.bytes();
            let no_group = group.is_none();
            // A file with no newsgroup can't be fetched — count it as missing.
            let mut acc = FileAcc {
                name: filename,
                is_par2,
                total_bytes,
                missing_bytes: if no_group { total_bytes } else { 0 },
                unknown_bytes: 0,
                has_missing: no_group,
            };
            if let Some(group) = group {
                for seg in &file.segments {
                    if !seg.has_valid_id() {
                        // Never sent; it can't be fetched either.
                        malformed += 1;
                        acc.missing_bytes += seg.bytes;
                        acc.has_missing = true;
                        missing_ids.insert(seg.message_id.clone());
                        continue;
                    }
                    meta.insert(seg.message_id.clone(), (idx, seg.bytes));
                    requests.push(SegmentRequest {
                        message_id: seg.message_id.clone(),
                        group: group.clone(),
                    });
                }
            }
            file_accs.push(acc);
        }

        // STAT every segment, pipelined across all connections. Size each batch
        // to roughly one pipelined round-trip per connection (capped) so the
        // whole scan is a few round-trips even for tens of thousands of segments,
        // instead of fixed 256-article batches (many more round-trips).
        let parallelism = self.connections.max(1);
        let stat_batch = requests.len().div_ceil(parallelism).clamp(128, 512);
        let to_check = requests.len() as u64;
        let articles_total = to_check + malformed;
        let queue = StatQueue::new(requests.chunks(stat_batch).map(|c| c.to_vec()));
        // Per-article STAT status: Some(true)=present, Some(false)=absent
        // (430/423), None=unknown (no connection could check it, or the
        // server's reply said nothing about the article). Unknown is NOT
        // marked absent: those articles are still attempted during download,
        // and they never make the verdict "unrepairable" on their own (see
        // `possibly_repairable`).
        let (results_tx, mut results_rx) = mpsc::unbounded_channel();
        let workers = futures::future::join_all(
            (0..parallelism)
                .map(|_| stat_worker(&self.pool, &self.job, &queue, results_tx.clone())),
        );
        drop(results_tx);

        let mut scan_incomplete = false;
        let mut checked = 0u64;
        // Tally batch results as they arrive. Racing the workers against the
        // job's cancellation aborts the scan at once on a stop; breaking
        // drops them, which cancels any STATs still in flight (their
        // connections are discarded, never recycled mid-reply).
        let mut workers = std::pin::pin!(workers);
        let mut workers_done = false;
        'scan: loop {
            tokio::select! {
                biased;
                _ = self.job.cancelled() => {
                    scan_incomplete = true;
                    break 'scan;
                }
                item = results_rx.recv() => {
                    let Some(batch) = item else { break 'scan };
                    checked += batch.len() as u64;
                    if to_check > 0 {
                        self.job.set_fraction(checked as f64 / to_check as f64);
                    }
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
                _ = &mut workers, if !workers_done => workers_done = true,
            }
        }

        let mut report = AvailabilityReport {
            articles_total,
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
                report.available_par2_bytes += fa.total_bytes.saturating_sub(unavailable);
                report.unknown_par2_bytes += fa.unknown_bytes;
            } else {
                report.total_data_bytes += fa.total_bytes;
                report.missing_data_bytes += unavailable;
                report.unknown_data_bytes += fa.unknown_bytes;
                // Only list files with CONFIRMED-missing segments (don't cry wolf
                // on a transient STAT error).
                if fa.has_missing {
                    report.missing_files.push(fa.name.clone());
                }
            }
        }
        report
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
        self.download_phase(nzb, &config, skip_message_ids, JobPhase::Downloading, None)
            .await
    }

    /// Download one phase's file set (the whole NZB, the data-first subset, or
    /// the deferred recovery volumes), reporting progress under `phase`. With a
    /// job record, files continue from what earlier sessions left on disk and
    /// every article written is recorded.
    async fn download_phase(
        &self,
        nzb: &Nzb,
        config: &Config,
        skip_message_ids: Option<&HashSet<String>>,
        phase: JobPhase,
        record: Option<&Arc<JobRecord>>,
    ) -> Result<DownloadOutcome> {
        std::fs::create_dir_all(&config.download.dir)?;

        let files: Vec<&NzbFile> = nzb.files().iter().collect();
        // Decided before the phase starts, so its first progress already
        // counts what is on disk.
        let starts: Vec<FileStart> = files
            .iter()
            .map(|f| prepare_file(f, &config.download.dir, record))
            .collect();

        // Progress is counted in the NZB's per-article sizes: the total is
        // known up front and matches the size the user was shown, and each
        // article advances it as it settles, so the phase ends at exactly 100%.
        let total_encoded: u64 = files.iter().map(|f| f.bytes()).sum();
        let resumed_bytes: u64 = files
            .iter()
            .zip(&starts)
            .map(|(f, s)| s.done_bytes(f))
            .sum();
        let resumed_files = starts
            .iter()
            .filter(|s| matches!(s, FileStart::Complete { .. }))
            .count() as u32;
        self.job.set_download_phase(
            phase,
            resumed_bytes,
            total_encoded,
            resumed_files,
            files.len() as u32,
        );
        if files.is_empty() {
            return Ok(DownloadOutcome::local(Vec::new()));
        }

        // No pool warm-up first: each worker connects on its own and starts
        // as soon as its connection is up, so the first articles flow while
        // the rest connect, and one slow connection holds up only its worker
        // (waiting for all of them stalled whole jobs for 30 s).
        let outcome = self
            .run_download(&files, starts, config, skip_message_ids, record)
            .await;
        self.job.emit_progress_if_changed();
        Ok(outcome)
    }

    /// Run a phase, or, when an earlier session finished it (the record says
    /// so and has every one of its files settled; a different
    /// `download_all_par2` can change the phase's file set), report it from
    /// the record without touching the network.
    async fn resume_phase(
        &self,
        nzb: &Nzb,
        config: &Config,
        skip_message_ids: Option<&HashSet<String>>,
        phase: JobPhase,
        record: Option<&Arc<JobRecord>>,
    ) -> Result<DownloadOutcome> {
        let names: HashSet<&str> = nzb.files().iter().map(|f| f.filename.as_str()).collect();
        let finished = record.filter(|r| {
            let phase_done = match phase {
                JobPhase::DownloadingRecovery => r.recovery() == Recovery::Done,
                _ => r.data_done(),
            };
            phase_done && r.all_settled(names.iter().copied())
        });
        let Some(record) = finished else {
            return self
                .download_phase(nzb, config, skip_message_ids, phase, record)
                .await;
        };
        let files = record.results(&config.download.dir, |name, _| names.contains(name));
        self.job.add_recorded(&files);
        Ok(DownloadOutcome::local(files))
    }

    /// The phase that just ran reached its end: not stopped, and no worker
    /// lost the server (articles failed that way deserve another try, so the
    /// phase must run again on resume).
    fn phase_finished(&self) -> bool {
        !self.job.is_cancelled() && self.job.connection_error().is_none()
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
        self.download_nzb_resuming(nzb, config, skip_message_ids, download_all_par2, None)
            .await
    }

    /// [`download_nzb_on_demand`](Self::download_nzb_on_demand) for an engine
    /// job: continues from what earlier sessions left (phases already finished
    /// are not run again) and records its progress in `record`.
    pub(crate) async fn download_nzb_resuming(
        &self,
        nzb: &Nzb,
        config: Config,
        skip_message_ids: Option<&HashSet<String>>,
        download_all_par2: bool,
        record: Option<&Arc<JobRecord>>,
    ) -> Result<DownloadOutcome> {
        // Keep the SMALLEST par2 file (the index: it carries the Main +
        // FileDescription packets but minimal recovery data) in Phase 1 and
        // defer the larger recovery volumes. Size-based, so it also works for
        // obfuscated releases whose par2 files lack the `.volNN+MM` marker.
        // Files are identified by their (unique) assigned filename.
        let keep_index: Option<String> = nzb
            .files()
            .iter()
            .filter(|f| f.is_par2())
            .min_by_key(|f| f.bytes())
            .map(|f| f.filename.clone());
        let is_deferred =
            |f: &NzbFile| f.is_par2() && keep_index.as_deref() != Some(f.filename.as_str());
        let deferred_count = nzb.files().iter().filter(|f| is_deferred(f)).count();

        // The record of a phase that ran to its end, to mark it finished.
        let finished =
            |record: Option<&Arc<JobRecord>>| record.filter(|_| self.phase_finished()).cloned();

        if download_all_par2 || deferred_count == 0 {
            let outcome = self
                .resume_phase(
                    nzb,
                    &config,
                    skip_message_ids,
                    JobPhase::Downloading,
                    record,
                )
                .await?;
            if let Some(record) = finished(record) {
                record.set_data_done();
                record.set_recovery(Recovery::NotNeeded);
            }
            return Ok(outcome);
        }

        let phase1 = nzb.subset(|f| !is_deferred(f));
        let outcome1 = self
            .resume_phase(
                &phase1,
                &config,
                skip_message_ids,
                JobPhase::Downloading,
                record,
            )
            .await?;
        if let Some(record) = finished(record) {
            record.set_data_done();
        }

        // A stop during Phase 1 must not launch a pointless Phase 2, and nor
        // must a lost server: its failed articles are not missing, and Phase 1
        // did not finish, so a resume runs it again and decides then.
        if self.job.is_cancelled() || self.job.connection_error().is_some() {
            return Ok(outcome1);
        }

        // Fetch recovery if any data segment is missing/corrupt OR the PAR2 index
        // itself failed in Phase 1 — a fetched recovery volume carries the same
        // Main/FileDesc metadata, restoring verification and name recovery.
        if outcome1.files.iter().all(|r| r.segments_failed == 0) {
            tracing::debug!(
                "Data complete; skipped {} recovery volume(s)",
                deferred_count
            );
            if let Some(record) = finished(record) {
                record.set_recovery(Recovery::NotNeeded);
            }
            return Ok(outcome1);
        }

        let phase2 = nzb.subset(is_deferred);
        let outcome2 = self
            .resume_phase(
                &phase2,
                &config,
                skip_message_ids,
                JobPhase::DownloadingRecovery,
                record,
            )
            .await?;
        if let Some(record) = finished(record) {
            record.set_recovery(Recovery::Done);
        }

        let mut files = outcome1.files;
        files.extend(outcome2.files);
        Ok(DownloadOutcome {
            files,
            transfer_duration: outcome1.transfer_duration + outcome2.transfer_duration,
            actual_wire_bytes: outcome1.actual_wire_bytes + outcome2.actual_wire_bytes,
        })
    }

    async fn run_download(
        &self,
        files: &[&NzbFile],
        starts: Vec<FileStart>,
        config: &Config,
        skip_message_ids: Option<&HashSet<String>>,
        record: Option<&Arc<JobRecord>>,
    ) -> DownloadOutcome {
        let configured_depth = config.tuning.pipeline_depth.max(1);
        let decode_retry_cap = config.tuning.decode_retry_cap.max(1);
        // Transient (connection-level / 412) retries get a more generous budget
        // than article-level retries — a dropped connection usually isn't the
        // article's fault — but it is bounded so a strict or half-dead server
        // can't livelock the download.
        let max_transient_retries = (config.usenet.retry_attempts as usize)
            .max(1)
            .saturating_mul(5);
        let retry_delay = Duration::from_millis(config.usenet.retry_delay);
        let fsync = config.tuning.fsync_on_finalize;

        // Build per-file state (reported in NZB order) and each file's list of
        // articles still to fetch. Articles the pre-flight scan found missing
        // are holes and settle immediately; articles an earlier session wrote
        // are already on disk (and already counted in the phase's progress).
        let mut file_states: Vec<Arc<FileState>> = Vec::with_capacity(files.len());
        let mut work: Vec<FileWork> = Vec::new();
        let mut finalize_now: Vec<Arc<FileHandle>> = Vec::new();

        // Every file's articles are sorted out before anything is done about
        // them, so the `.partial` files the phase writes can all be opened at
        // once (one blocking task, not one each). Each is pre-allocated
        // (sparse) to the file's encoded size, an upper bound of its decoded
        // size; finalize truncates to the highest byte written. A resumed
        // file is reopened without truncation so its finished articles
        // survive.
        let sorted: Vec<Articles> = files
            .iter()
            .zip(&starts)
            .map(|(file, start)| Articles::sort(file, start, skip_message_ids))
            .collect();
        let to_open = files
            .iter()
            .zip(&sorted)
            .filter(|(file, articles)| articles.needs_file(file))
            .map(|(file, articles)| {
                let path = partial_path(&config.download.dir.join(&file.filename));
                (path, file.bytes(), articles.finished == 0)
            })
            .collect();
        let mut opened = create_preallocated_files(to_open).await.into_iter();

        for ((file, start), articles) in files.iter().zip(starts).zip(sorted) {
            let filename = file.filename.clone();
            let final_path = config.download.dir.join(&filename);
            let partial_path = partial_path(&final_path);
            let listed = file.segments.len();
            self.job.add_articles_total(listed as u64);
            let slot = record.and_then(|r| r.slot(&filename));

            let max_byte = match start {
                FileStart::Complete { size } => {
                    // Finished and renamed in an earlier session.
                    file_states.push(Arc::new(FileState::already_complete(
                        filename, final_path, listed, size,
                    )));
                    continue;
                }
                FileStart::Partial { max_byte, .. } => max_byte,
                FileStart::Fresh => 0,
            };
            let Articles {
                active,
                holes,
                malformed,
                finished,
                done_bytes,
                hole_bytes,
            } = articles;
            if malformed == 1 {
                self.job.warn(format!(
                    "1 article of {filename} has a malformed message ID in the NZB, so it counts as missing."
                ));
            } else if malformed > 1 {
                self.job.warn(format!(
                    "{malformed} articles of {filename} have malformed message IDs in the NZB, so they count as missing."
                ));
            }

            // Given-up articles are recorded too, so a job that finished
            // downloading still knows which files came out incomplete.
            let record_failed = |indices: &mut dyn Iterator<Item = u32>| {
                if let (Some(record), Some(slot)) = (record, slot) {
                    indices.for_each(|index| record.failed(slot, index));
                }
            };
            record_failed(&mut holes.iter().copied());

            let group: Arc<str> = match file.groups.first() {
                Some(g) => Arc::from(g.as_str()),
                None => {
                    self.job
                        .warn(format!("{filename} lists no newsgroup, so it was skipped."));
                    record_failed(&mut active.iter().map(|a| a.0));
                    file_states.push(self.unavailable_file(filename, final_path, file, done_bytes));
                    continue;
                }
            };

            if active.is_empty() && finished == 0 {
                file_states.push(self.unavailable_file(filename, final_path, file, 0));
                continue;
            }

            // Opened above (`Articles::needs_file` picks the files that get
            // this far).
            let resuming = finished > 0;
            let partial_file = match opened.next().expect("opened above") {
                Ok(f) => Arc::new(f),
                Err(e) => {
                    tracing::debug!("could not create {}: {e}", partial_path.display());
                    self.job.warn(format!(
                        "Could not create {filename}: {}.",
                        crate::error::write_problem(&e)
                    ));
                    record_failed(&mut active.iter().map(|a| a.0));
                    file_states.push(self.unavailable_file(filename, final_path, file, done_bytes));
                    continue;
                }
            };
            if let (Some(record), Some(slot)) = (record, slot) {
                record.attach(slot, partial_file.clone());
            }

            self.job.add_bytes_done(hole_bytes);
            self.job.add_articles_failed(holes.len() as u64);
            let state = FileState::new(
                filename,
                final_path,
                partial_path,
                active.len(),
                listed,
                slot,
            );
            // Pre-flight holes are failures (so the file reports incomplete and
            // on-demand recovery kicks in) but are NOT part of `segments_total`:
            // only fetched articles settle, so finalization still triggers.
            state.segments_failed.store(holes.len(), Ordering::Relaxed);
            state.segments_downloaded.store(finished, Ordering::Relaxed);
            state.max_byte_position.store(max_byte, Ordering::Relaxed);
            // Articles written in an earlier session can't vouch for
            // themselves any more (see `engine::sidecar`): PAR2 verifies.
            if resuming {
                state.all_crc_verified.store(false, Ordering::Relaxed);
            }
            let state = Arc::new(state);
            file_states.push(state.clone());
            let handle = Arc::new(FileHandle {
                file: partial_file,
                state,
                placement: Placement::new(file),
            });
            if active.is_empty() {
                // Everything left was finished last time; only the rename remains.
                finalize_now.push(handle);
            } else {
                work.push(FileWork {
                    handle,
                    group,
                    articles: active,
                });
            }
        }

        for handle in finalize_now {
            finalize_file(handle, fsync, record.cloned()).await;
            self.job.add_files_done(1);
        }

        if work.is_empty() {
            return DownloadOutcome::local(file_states.iter().map(|s| s.snapshot()).collect());
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
        //
        // Articles are queued largest file first so big work hits the workers
        // immediately; consecutive dequeues tend to share a group, but ANY
        // worker can take ANY article, so no connection idles while work
        // remains and the tail is one in-flight window per connection.
        let (job_tx, job_rx) = flume::unbounded::<ArticleJob>();
        work.sort_by_key(|w| Reverse(w.articles.len()));
        let (mut total_articles, mut total_bytes) = (0usize, 0u64);
        for w in work {
            for (index, message_id, encoded_bytes) in w.articles {
                total_articles += 1;
                total_bytes += encoded_bytes;
                let _ = job_tx.send(ArticleJob {
                    file: w.handle.clone(),
                    group: w.group.clone(),
                    index,
                    message_id,
                    encoded_bytes,
                    decode_attempts: 0,
                    transient_attempts: 0,
                    suspect: false,
                    connections_lost: 0,
                    sent_after_loss: false,
                });
            }
        }

        // Adaptive pipeline depth: keep ~4 MB of BODY requests in flight per
        // connection so a high-RTT link stays bandwidth-bound (throughput is
        // capped by depth*avg_article / RTT until it saturates). Bounded to
        // [configured, 32] so RAM and retry granularity stay sane; small
        // articles raise the depth, large ones keep it modest.
        let avg_article = total_bytes / (total_articles.max(1) as u64);
        let adaptive_depth = if avg_article > 0 {
            ((4 * 1024 * 1024) / avg_article).clamp(4, 32) as usize
        } else {
            configured_depth
        };
        let pipeline_depth = configured_depth.max(adaptive_depth);

        let (feedback_tx, mut feedback_rx) = mpsc::unbounded_channel::<WorkerFeedback>();
        // Bounded write channel: a few decoded segments per connection is enough
        // to cover decode-burst jitter and one write latency; beyond that just
        // pins RAM without feeding the writer faster (it drains at disk speed).
        let write_capacity = (self.connections.max(1) * 4).max(32);
        let (write_tx, write_rx) = mpsc::channel::<WriteJob>(write_capacity);

        let writer_handle = tokio::spawn(run_writer(
            write_rx,
            self.job.clone(),
            fsync,
            record.cloned(),
        ));

        // The transfer window opens as the workers start (and connect) so it
        // counts every byte they read, and closes at the last article written.
        let window_start = Instant::now();
        let wire_at_start = self.job.wire_bytes();

        let worker_count = self.connections.max(1);
        let queue_closed = CancellationToken::new();
        let losses = Arc::new(LossTally::new(total_articles));
        let ctx = Arc::new(WorkerCtx {
            pool: self.pool.clone(),
            job: self.job.clone(),
            job_rx,
            feedback_tx,
            write_tx,
            pipeline_depth,
            decode_retry_cap,
            max_transient_retries,
            retry_delay,
            census: Census::default(),
            queue_closed: queue_closed.clone(),
            losses: losses.clone(),
        });
        let mut worker_handles = Vec::with_capacity(worker_count);
        for worker_id in 0..worker_count {
            let ctx = ctx.clone();
            worker_handles.push(tokio::spawn(worker_loop(worker_id, ctx)));
        }
        drop(ctx);

        // Coordinate completions and retry re-queues. When `pending` hits 0,
        // drop `job_tx` so workers exit. A stop ends coordination at once: the
        // workers abandon their work (see `worker_loop`) and incomplete files
        // stay `.partial`, since only fully settled files are finalized.
        let mut pending = total_articles;
        while pending > 0 {
            let feedback = tokio::select! {
                biased;
                _ = self.job.cancelled() => break,
                feedback = feedback_rx.recv() => feedback,
            };
            match feedback {
                Some(WorkerFeedback::Settled) => {
                    pending -= 1;
                }
                Some(WorkerFeedback::Retry(job)) => {
                    if job_tx.send(job).is_err() {
                        // Receiver gone — workers must have exited.
                        break;
                    }
                }
                None => {
                    // All workers exited without finishing all jobs (shouldn't
                    // happen under normal flow). Break and let cleanup proceed.
                    break;
                }
            }
        }
        drop(job_tx);
        queue_closed.cancel();

        for handle in worker_handles {
            if let Err(e) = handle.await {
                tracing::error!("Download worker panicked: {}", e);
            }
        }
        let (transfer_duration, actual_wire_bytes) = match writer_handle.await {
            Ok(Some(last)) => (
                last.at.duration_since(window_start),
                last.wire_bytes.saturating_sub(wire_at_start),
            ),
            Ok(None) => (Duration::ZERO, 0),
            Err(e) => {
                tracing::error!("Writer task panicked: {}", e);
                (Duration::ZERO, 0)
            }
        };
        // Recovery data is in reach when the NZB has PAR2 (its index is
        // always in the first phase): only then can an article that keeps
        // losing the connection be left to PAR2 as missing.
        let repairable = files.iter().any(|f| f.is_par2());
        self.judge_connection_failures(
            &file_states,
            &losses,
            total_articles,
            repairable,
            &config.usenet.server,
        );

        DownloadOutcome {
            files: file_states.iter().map(|s| s.snapshot()).collect(),
            transfer_duration,
            actual_wire_bytes,
        }
    }

    /// Judge the phase's lost connections. Articles given up because the
    /// server never got them across are not missing: those whose retries
    /// ran out before their request could be sent (the connection failed
    /// selecting the group or sending), those given up on a flaky server
    /// ([`LossTally::flaky`]), and, when the server is to blame, those that
    /// lost the connection every time they were fetched on their own
    /// ([`lost_window`]). The server is to blame when it turned out flaky
    /// (an article given up before that showed was no more at fault than
    /// the rest), when several articles were given up that way and nothing
    /// requested after the first loss came through, or when they are most
    /// of the phase's articles; otherwise they are articles that kill the
    /// connection (a few, among articles that arrive): missing, for PAR2 to
    /// repair. When the articles not missing are at least half of what
    /// failed in the phase (or the server is to blame), the job records a
    /// lost connection, like workers that could not reconnect at all, so the
    /// phase doesn't count as finished (a resume fetches them again) and the
    /// job fails resumably with `ErrorKind::Connect`. Not after a stop.
    fn judge_connection_failures(
        &self,
        file_states: &[Arc<FileState>],
        losses: &LossTally,
        articles: usize,
        repairable: bool,
        server: &str,
    ) {
        let unsent = losses.unsent.load(Ordering::Acquire);
        let killers = losses.killers.load(Ordering::Acquire);
        let flaky_given_up = losses.flaky_given_up.load(Ordering::Acquire);
        if unsent + killers + flaky_given_up == 0 || self.job.is_cancelled() {
            return;
        }
        let arrived_since = losses.arrived_since.load(Ordering::Acquire);
        let server_to_blame = losses.flaky()
            || (killers >= 2 && (arrived_since == 0 || killers.saturating_mul(2) > articles));
        // Without PAR2, giving an article up as missing helps nobody and can't
        // be resumed, and in a small job a server's random resets can hit one
        // article three times before it looks flaky: there the losses count as
        // the connection's, and decide the verdict when they are most of what
        // failed (articles the server says it hasn't still make it "missing").
        let killers_lost = server_to_blame || !repairable;
        let lost = unsent + flaky_given_up + if killers_lost { killers } else { 0 };
        let failed: usize = file_states
            .iter()
            .map(|s| s.segments_failed.load(Ordering::Relaxed))
            .sum();
        let other = failed.saturating_sub(lost);
        tracing::debug!(
            "{killers} article(s) lost the connection every time on their own, \
             {flaky_given_up} on a flaky server, {unsent} could not be sent, \
             {arrived_since} arrived since the first loss; {other} failed otherwise"
        );
        if server_to_blame {
            self.job
                .add_connection_failures((killers + flaky_given_up) as u64);
        }
        if lost > 0 && (server_to_blame || lost >= other) && self.job.connection_error().is_none() {
            self.job.record_connection_error(&DlNzbError::Job {
                kind: crate::error::ErrorKind::Connect,
                message: format!("The connection to {server} kept dropping."),
            });
        }
    }

    /// A file that can't be fetched at all (no newsgroup, every article
    /// missing, or its file couldn't be created): settled as fully failed.
    /// `counted` is what the phase's progress already includes for it.
    fn unavailable_file(
        &self,
        filename: String,
        final_path: PathBuf,
        file: &NzbFile,
        counted: u64,
    ) -> Arc<FileState> {
        let listed = file.segments.len();
        self.job
            .add_bytes_done(file.bytes().saturating_sub(counted));
        self.job.add_articles_failed(listed as u64);
        self.job.add_files_done(1);
        Arc::new(FileState::placeholder_failed(filename, final_path, listed))
    }
}

/// How a file starts in this session, from what earlier ones left.
enum FileStart {
    /// From scratch.
    Fresh,
    /// Finished and renamed in an earlier session: nothing to do.
    Complete { size: u64 },
    /// `<name>.partial` already holds the articles in `done`.
    Partial { done: SegmentSet, max_byte: u64 },
}

impl FileStart {
    /// NZB bytes of the file's articles already on disk.
    fn done_bytes(&self, file: &NzbFile) -> u64 {
        match self {
            FileStart::Fresh => 0,
            FileStart::Complete { .. } => file.bytes(),
            FileStart::Partial { done, .. } => file
                .segments
                .iter()
                .enumerate()
                .filter(|(i, _)| done.contains(*i as u32))
                .map(|(_, s)| s.bytes)
                .sum(),
        }
    }
}

/// A file's articles as this phase finds them: still to fetch, missing
/// (holes, settled at once) or written in an earlier session.
#[derive(Default)]
struct Articles {
    /// (position in the file's segment list, message id, encoded bytes)
    active: Vec<(u32, String, u64)>,
    holes: Vec<u32>,
    /// Holes because their message id is malformed.
    malformed: usize,
    /// Written in an earlier session.
    finished: usize,
    done_bytes: u64,
    hole_bytes: u64,
}

impl Articles {
    /// Sort the articles of `file`, which starts as `start` (a file finished
    /// in an earlier session has none to sort). Articles in `missing` (the
    /// pre-flight scan's) are holes.
    fn sort(file: &NzbFile, start: &FileStart, missing: Option<&HashSet<String>>) -> Self {
        let done = match start {
            FileStart::Complete { .. } => return Self::default(),
            FileStart::Partial { done, .. } => Some(done),
            FileStart::Fresh => None,
        };
        let mut sorted = Self::default();
        for (index, seg) in file.segments.iter().enumerate() {
            let index = index as u32;
            if done.is_some_and(|d| d.contains(index)) {
                sorted.finished += 1;
                sorted.done_bytes += seg.bytes;
            } else if !seg.has_valid_id() {
                // Never sent (it could carry extra commands): missing.
                sorted.malformed += 1;
                sorted.holes.push(index);
                sorted.hole_bytes += seg.bytes;
            } else if missing.is_some_and(|s| s.contains(&seg.message_id)) {
                // Drop only the individually-missing segments; every present
                // segment still contributes data so PAR2 has the most blocks
                // to repair from.
                sorted.holes.push(index);
                sorted.hole_bytes += seg.bytes;
            } else {
                sorted
                    .active
                    .push((index, seg.message_id.clone(), seg.bytes));
            }
        }
        sorted
    }

    /// Whether `file` gets a `.partial` to write: it lists a newsgroup, and
    /// has articles to fetch or written in an earlier session.
    fn needs_file(&self, file: &NzbFile) -> bool {
        !file.groups.is_empty() && (!self.active.is_empty() || self.finished > 0)
    }
}

/// Decide how `file` starts from what the job record says earlier sessions
/// left, checked against the folder: a finalized file must still be there
/// (one finalized with articles missing is reopened so they are tried again),
/// and a `.partial` must still exist and reach the highest byte recorded.
/// Anything that doesn't check out starts over, and the record forgets it.
fn prepare_file(file: &NzbFile, dir: &Path, record: Option<&Arc<JobRecord>>) -> FileStart {
    let Some((record, slot)) = record.and_then(|r| r.slot(&file.filename).map(|s| (r, s))) else {
        return FileStart::Fresh;
    };
    let Some(prior) = record.prior(slot) else {
        return FileStart::Fresh;
    };
    let final_path = dir.join(&file.filename);
    let partial_path = partial_path(&final_path);
    let file_len = |path: &Path| {
        std::fs::metadata(path)
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len())
    };
    let listed = file.segments.len() as u32;

    if prior.finalized {
        if let Some(size) = file_len(&final_path) {
            if prior.done.count() == listed {
                return FileStart::Complete { size };
            }
            // Finalized with articles missing (given up, or unreachable when
            // the server went away): back to `.partial` to fetch them again.
            if !prior.done.is_empty()
                && size >= prior.max_byte
                && std::fs::rename(&final_path, &partial_path).is_ok()
            {
                record.reopen_file(slot);
                return FileStart::Partial {
                    done: prior.done,
                    max_byte: prior.max_byte,
                };
            }
        }
    } else if !prior.done.is_empty()
        && file_len(&partial_path).is_some_and(|len| len >= prior.max_byte)
    {
        record.reopen_file(slot);
        return FileStart::Partial {
            done: prior.done,
            max_byte: prior.max_byte,
        };
    }
    record.reset_file(slot);
    FileStart::Fresh
}

enum WorkerFeedback {
    /// Article reached a terminal state (Ok or permanent failure).
    Settled,
    /// Article should be retried (transient wire error, or a bounded decode retry).
    Retry(ArticleJob),
}

struct WorkerCtx {
    pool: NntpPool,
    job: Arc<JobCtx>,
    job_rx: flume::Receiver<ArticleJob>,
    feedback_tx: mpsc::UnboundedSender<WorkerFeedback>,
    write_tx: Sender<WriteJob>,
    /// How many `BODY` requests each connection keeps in flight (sliding window).
    pipeline_depth: usize,
    /// Max retries for a yEnc decode failure before giving up on the article.
    decode_retry_cap: u8,
    /// Max retries for transient (connection-level / 412) failures.
    max_transient_retries: usize,
    retry_delay: Duration,
    /// The workers' connections ([`Held`]).
    census: Census,
    /// Cancelled when the coordinator closes the queue: wakes spare workers.
    queue_closed: CancellationToken,
    /// The phase's lost connections ([`Downloader::judge_connection_failures`]).
    losses: Arc<LossTally>,
}

/// A download phase's lost connections, for judging them at its end.
#[derive(Default)]
struct LossTally {
    /// Connections lost with requests in flight.
    connections: AtomicU64,
    /// Articles requested after the first of those losses that arrived.
    arrived_since: AtomicUsize,
    /// Articles given up because the connection was lost every time they
    /// were fetched on their own ([`lost_window`]).
    killers: AtomicUsize,
    /// Articles given up because their connection failed before their
    /// request could be sent, every time.
    unsent: AtomicUsize,
    /// Distinct articles that lost a connection on their own at least once.
    lost_alone: AtomicUsize,
    /// Of those, the ones that arrived afterwards.
    lost_alone_arrived: AtomicUsize,
    /// How many of `lost_alone` the phase takes before calling the server
    /// flaky ([`LossTally::flaky`]).
    lost_alone_max: usize,
    /// Articles given up after [`CONNECTIONS_LOST_FLAKY_MAX`] losses on their
    /// own on a flaky server: lost to the connection, not missing.
    flaky_given_up: AtomicUsize,
}

impl LossTally {
    fn new(articles: usize) -> Self {
        Self {
            lost_alone_max: (articles / 100).max(2),
            ..Self::default()
        }
    }

    /// Whether the server drops connections at random rather than on a few
    /// articles: more distinct articles lost the connection on their own
    /// than `max(2, 1% of the phase's articles)`, and at least one of them
    /// arrived later (an article that kills the connection never does).
    /// Losses that concentrate on a few articles that never arrive are those
    /// articles' fault; they are given up as missing, for PAR2 to repair.
    fn flaky(&self) -> bool {
        self.lost_alone.load(Ordering::Acquire) > self.lost_alone_max
            && self.lost_alone_arrived.load(Ordering::Acquire) > 0
    }
}

/// How many connections an article may lose while it is the only request in
/// flight before it is given up as missing.
const CONNECTIONS_LOST_MAX: u8 = 3;

/// How long a suspect's `BODY` reply may take to start (others get a
/// minute): it is the only request on its connection ([`lost_window`]), and
/// a server answers a lone `BODY` within seconds, as it does `GROUP` (15 s
/// too). A timeout counts as one more lost connection. It bounds a reset
/// that never reached us: macOS ignores one that arrives behind data it
/// hasn't acknowledged and answers with a "challenge ACK" (RFC 5961), sent
/// at most a few times a second, so when connections drop in quick
/// succession some go silent instead of failing. A request in a full window
/// can be caught the same way and still waits out the minute; that is left
/// alone, since a slow server's busy window can take that long.
const SUSPECT_HEAD_TIMEOUT: Duration = Duration::from_secs(15);

/// The same on a flaky server ([`LossTally::flaky`]), where an article
/// that lost the connection on its own is no more to blame than any other:
/// it goes back to the queue until this many, then is given up as lost to
/// the connection (the job fails resumably), never as missing. It only
/// bounds an article that kills the connection on a server that is flaky
/// too: at 50% resets, about 1 in 2,000 articles that loses the connection
/// on its own goes on to lose it this often.
const CONNECTIONS_LOST_FLAKY_MAX: u8 = 12;

/// A worker's connection, counted in the [`Census`] while held (dropped
/// with it, so a stopped or finished worker is uncounted too), and its bytes
/// counted toward the job's speed as they arrive.
struct Held<'a> {
    /// Dropped before the connection is.
    _holding: Holding<'a>,
    conn: PooledConnection,
    job: &'a JobCtx,
    counter: Option<Arc<AtomicU64>>,
}

impl<'a> Held<'a> {
    fn new(conn: PooledConnection, ctx: &'a WorkerCtx) -> Self {
        let holding = ctx.census.hold();
        let counter = conn.bytes_counter();
        if let Some(counter) = &counter {
            ctx.job.attach_counter(counter.clone());
        }
        Self {
            _holding: holding,
            conn,
            job: &ctx.job,
            counter,
        }
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        if let Some(counter) = &self.counter {
            self.job.detach_counter(counter);
        }
    }
}

impl std::ops::Deref for Held<'_> {
    type Target = PooledConnection;
    fn deref(&self) -> &PooledConnection {
        &self.conn
    }
}

impl std::ops::DerefMut for Held<'_> {
    fn deref_mut(&mut self) -> &mut PooledConnection {
        &mut self.conn
    }
}

/// How long a spare worker (the server takes no more connections, but other
/// workers have theirs) first waits before asking for one again; each refusal
/// in a row doubles it, up to [`SPARE_WAIT_MAX`].
const SPARE_WAIT_FIRST: Duration = Duration::from_secs(1);
const SPARE_WAIT_MAX: Duration = Duration::from_secs(30);

// --- Job and state types ---

/// One article (segment) of work. Articles are the unit of distribution AND of
/// retry: a transient failure re-queues only this one article, never a batch.
struct ArticleJob {
    file: Arc<FileHandle>,
    group: Arc<str>,
    /// Position in the file's segment list (the job record's key).
    index: u32,
    message_id: String,
    /// Encoded size from the NZB; the writer advances the job's progress by
    /// this once the article reaches a terminal state (success or permanent
    /// failure).
    encoded_bytes: u64,
    /// Bounded retries for decode (CRC/size) failures.
    decode_attempts: u8,
    /// Bounded retries for transient (connection-level / 412) failures. Given a
    /// generous budget because these usually mean connection flakiness rather
    /// than a bad article, but bounded so a strict/half-dead server can't
    /// livelock the download.
    transient_attempts: u8,
    /// Its reply was being read when a connection was lost: from then on it
    /// is fetched on its own ([`lost_window`]).
    suspect: bool,
    /// Connections lost while it was the only request in flight.
    connections_lost: u8,
    /// Requested after the phase's first lost connection.
    sent_after_loss: bool,
}

struct FileHandle {
    file: Arc<StdFile>,
    state: Arc<FileState>,
    /// How far into the file its articles may place their data.
    placement: Placement,
}

/// How far into a file its articles may place their data. Each article says
/// where its part goes (`=ypart`); one placed far past the end would grow the
/// file (sparsely, past the free-space check) to wherever it says. The limit
/// comes from the NZB alone, never from an article (not the one being judged,
/// nor any placed before it): a part may end at most at
///
/// `max(16 x the file's NZB sizes added up, 4 MiB x its highest part number) + 4 MiB`
///
/// where the highest part number counts at least the parts the NZB lists and
/// at most twice as many. The NZB's sizes may be under-reported, 0, the
/// decoded size, or miss an entry; its numbering still says how many parts
/// there are, and a yEnc part is rarely over 1 MiB, so honest parts fit with
/// room to spare. A `=ybegin size=` inside the limit is fine (the part already
/// lies inside it, see `yenc::decode_article`); one past it raises nothing.
///
/// What that bounds: a hostile article in an ordinary NZB (parts of 256 KB
/// or more) can make its file at most about 16 times what the NZB says, and a
/// hostile NZB only as large as it claims (its sizes, or 4 MiB per part
/// number).
struct Placement {
    /// The byte a part may end at, at most.
    limit: u64,
}

/// How many times its NZB size a file may grow to (sizes may be under-reported).
const PLACEMENT_SIZE_FACTOR: u64 = 16;
/// The largest part believed per part number, and the slack on top.
const PLACEMENT_PART_MAX: u64 = 4 << 20;

impl Placement {
    fn new(file: &NzbFile) -> Self {
        let segments = &file.segments;
        let listed = segments.len() as u64;
        let parts = segments
            .iter()
            .map(|s| u64::from(s.number))
            .max()
            .unwrap_or(0)
            .clamp(listed, listed.saturating_mul(2));
        let by_size = file.bytes().saturating_mul(PLACEMENT_SIZE_FACTOR);
        let by_number = parts.saturating_mul(PLACEMENT_PART_MAX);
        Self {
            limit: by_size.max(by_number).saturating_add(PLACEMENT_PART_MAX),
        }
    }

    /// Whether a part may end at byte `end` (see [`Placement`]).
    fn admit(&self, end: u64) -> bool {
        end <= self.limit
    }
}

/// One file's articles still to fetch, before they become `ArticleJob`s.
struct FileWork {
    handle: Arc<FileHandle>,
    group: Arc<str>,
    /// (position in the file's segment list, message id, encoded bytes)
    articles: Vec<(u32, String, u64)>,
}

struct FileState {
    filename: String,
    final_path: PathBuf,
    partial_path: PathBuf,
    /// The file's position in the job record, when the job keeps one.
    slot: Option<usize>,
    /// Articles this run fetches; settling all of them finalizes the file.
    segments_total: usize,
    /// Articles the NZB lists for the file (for reporting).
    segments_listed: usize,
    segments_downloaded: AtomicUsize,
    segments_failed: AtomicUsize,
    segments_settled: AtomicUsize,
    max_byte_position: AtomicU64,
    /// Cleared if any successful segment lacked a matched wire checksum.
    all_crc_verified: AtomicBool,
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
        segments_listed: usize,
        slot: Option<usize>,
    ) -> Self {
        Self {
            filename,
            final_path,
            partial_path,
            slot,
            segments_total,
            segments_listed,
            segments_downloaded: AtomicUsize::new(0),
            segments_failed: AtomicUsize::new(0),
            segments_settled: AtomicUsize::new(0),
            max_byte_position: AtomicU64::new(0),
            all_crc_verified: AtomicBool::new(true),
            finalized: AtomicBool::new(false),
        }
    }

    fn placeholder_failed(filename: String, final_path: PathBuf, total: usize) -> Self {
        let s = Self::new(filename, final_path.clone(), final_path, total, total, None);
        s.segments_failed.store(total, Ordering::Relaxed);
        s.segments_settled.store(total, Ordering::Relaxed);
        s.finalized.store(true, Ordering::Relaxed);
        s
    }

    /// A file a previous session finished and finalized (resume). Its wire
    /// checksums aren't known any more, so PAR2 will verify it for real.
    fn already_complete(filename: String, final_path: PathBuf, total: usize, size: u64) -> Self {
        let s = Self::new(filename, final_path.clone(), final_path, total, total, None);
        s.segments_downloaded.store(total, Ordering::Relaxed);
        s.segments_settled.store(total, Ordering::Relaxed);
        s.max_byte_position.store(size, Ordering::Relaxed);
        s.all_crc_verified.store(false, Ordering::Relaxed);
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
            segments_total: self.segments_listed,
            segments_downloaded: self.segments_downloaded.load(Ordering::Relaxed),
            segments_failed: self.segments_failed.load(Ordering::Relaxed),
            all_segments_crc_verified: self.all_crc_verified.load(Ordering::Relaxed),
        }
    }
}

enum WriteJob {
    Write {
        file: Arc<FileHandle>,
        index: u32,
        offset: u64,
        data: Bytes,
        encoded_size: u64,
    },
    Failed {
        file: Arc<FileHandle>,
        index: u32,
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
///
/// The whole worker is raced against the job's cancellation: a stop drops it
/// mid-await (even mid-read, without waiting out the 60/120 s read timeouts).
/// Its connection is then dropped with responses outstanding, which the pool
/// detects and closes instead of recycling.
///
/// A pause is just as prompt: whatever the worker is waiting on (a response,
/// the group selection, a new connection) is abandoned, the articles in its
/// window go back to the queue as they were (no retry spent: pausing is not
/// their fault), and its connection is closed rather than handed back, so a
/// paused job holds no socket and reads nothing. Resuming reconnects.
async fn worker_loop(worker_id: usize, ctx: Arc<WorkerCtx>) {
    let job = ctx.job.clone();
    tokio::select! {
        biased;
        _ = job.cancelled() => {}
        _ = worker_run(worker_id, &ctx) => {}
    }
}

async fn worker_run(worker_id: usize, ctx: &WorkerCtx) {
    let depth = ctx.pipeline_depth.max(1);
    let mut connection: Option<Held<'_>> = None;
    let mut spare_wait = SPARE_WAIT_FIRST;
    let mut group_ready = false;
    let mut inflight: VecDeque<ArticleJob> = VecDeque::with_capacity(depth);
    // A suspect taken while other requests were in flight: it waits for the
    // window to drain, then goes alone (see `lost_window`).
    let mut held: Option<ArticleJob> = None;
    let mut pause_rx = ctx.job.pause_rx();

    'work: loop {
        // Paused: give the window back, close the connection, and wait for
        // resume (or for the phase to end without this worker). Idle pool
        // connections go too (the job's own connection check leaves one), so
        // a paused job, like an app suspended after pausing, holds no socket.
        if *pause_rx.borrow() {
            give_back(inflight.drain(..).chain(held.take()), ctx);
            if let Some(mut held) = connection.take() {
                ctx.job.add_wire_bytes(held.take_bytes_read());
                held.discard();
            }
            ctx.pool.retain(|_, _| false);
            group_ready = false;
            tokio::select! {
                _ = ctx.queue_closed.cancelled() => return,
                resumed = pause_rx.wait_for(|paused| !*paused) => {
                    if resumed.is_err() {
                        return; // the job is gone
                    }
                }
            }
            continue;
        }

        // Acquire a connection if we don't have one.
        if connection.is_none() {
            let acquired_before = ctx.census.acquired();
            // Nothing is in flight without a connection, so once every
            // article has settled (the queue closed) this worker is done: it
            // must not hold the phase open for the rest of a round of
            // reconnect attempts to a server that has gone away. A pause
            // drops the attempt (the top of the loop waits).
            let acquired = tokio::select! {
                biased;
                _ = ctx.queue_closed.cancelled() => return,
                _ = until_paused(&mut pause_rx) => continue,
                acquired = acquire_connection(ctx, acquired_before, worker_id) => acquired,
            };
            match acquired {
                Ok(c) => {
                    connection = Some(Held::new(c, ctx));
                    group_ready = false;
                    spare_wait = SPARE_WAIT_FIRST;
                }
                // Paused meanwhile: nothing is lost, and the failure says
                // nothing about the job (the server may be back on resume).
                Err(_) if *pause_rx.borrow() => continue,
                Err(_) if ctx.census.others_connected(acquired_before) => {
                    // The server is up but takes no more connections (the
                    // provider allows fewer than configured, or another
                    // device holds some). Not the articles' fault: hand back
                    // anything held, take no work, and ask again later.
                    give_back(inflight.drain(..), ctx);
                    tokio::select! {
                        _ = ctx.queue_closed.cancelled() => return,
                        _ = tokio::time::sleep(spare_wait) => {}
                    }
                    spare_wait = (spare_wait * 2).min(SPARE_WAIT_MAX);
                    continue;
                }
                Err(e) => {
                    // No worker can reach the server: record why, so the job
                    // reports it with its real kind (authentication,
                    // unreachable, ...), and settle what we hold and
                    // everything queued as failed, so the phase ends after
                    // one round of reconnect attempts instead of one article
                    // per round (minutes of a bar creeping on with nothing
                    // downloading). The phase does not count as finished
                    // (`phase_finished`), so a resume fetches them all again.
                    // Then retry acquiring on the next iteration.
                    ctx.job.record_connection_error(&e);
                    for job in inflight.drain(..) {
                        fail_article(job, ctx).await;
                    }
                    let mut failed_queued = false;
                    loop {
                        match ctx.job_rx.try_recv() {
                            Ok(job) => {
                                fail_article(job, ctx).await;
                                failed_queued = true;
                            }
                            Err(flume::TryRecvError::Empty) => break,
                            Err(flume::TryRecvError::Disconnected) => return,
                        }
                    }
                    if !failed_queued {
                        // Nothing queued: wait for work (a retry handed back
                        // by another worker) rather than spin on reconnects.
                        // Not while paused: what paused workers hand back
                        // waits for the resume, it isn't failed.
                        let next = tokio::select! {
                            biased;
                            _ = until_paused(&mut pause_rx) => continue,
                            next = ctx.job_rx.recv_async() => next,
                        };
                        match next {
                            Ok(job) => fail_article(job, ctx).await,
                            Err(_) => break,
                        }
                    }
                    continue;
                }
            }
        }

        let mut dead = false;

        // Fill the sliding window (not while paused). Block only when nothing
        // is in flight (an idle worker waits for work, or for a pause);
        // otherwise top up non-blocking and go drain. A suspect goes alone:
        // it waits for the window to drain, and nothing is sent behind it.
        // So does a group (re-)selection: its reply would otherwise be read
        // as an article's.
        while inflight.len() < depth && !*pause_rx.borrow() {
            if inflight.front().is_some_and(|j| j.suspect) || (!group_ready && !inflight.is_empty())
            {
                break;
            }
            let mut job = if let Some(job) = held.take() {
                if !inflight.is_empty() {
                    held = Some(job);
                    break;
                }
                job
            } else if inflight.is_empty() {
                let next = tokio::select! {
                    biased;
                    _ = until_paused(&mut pause_rx) => None,
                    next = ctx.job_rx.recv_async() => Some(next),
                };
                match next {
                    // Paused while idle: the top of the loop releases the connection.
                    None => break,
                    Some(Ok(job)) => job,
                    // Queue closed and nothing in flight: this worker is done.
                    Some(Err(_)) => return,
                }
            } else {
                match ctx.job_rx.try_recv() {
                    Ok(job) => job,
                    Err(_) => break,
                }
            };
            if job.suspect && !inflight.is_empty() {
                held = Some(job);
                break;
            }

            let conn = connection.as_mut().expect("connection acquired above");
            // Select the group once per connection (best-effort; `BODY <id>` is
            // group-independent on compliant servers).
            if !group_ready {
                let paused = tokio::select! {
                    biased;
                    _ = until_paused(&mut pause_rx) => true,
                    _ = conn.ensure_group(&job.group) => false,
                };
                if paused {
                    give_back([job], ctx);
                    continue 'work;
                }
                group_ready = true;
                if conn.is_poisoned() {
                    retry_transient(job, ctx, true).await;
                    dead = true;
                    break;
                }
            }

            if conn.send_body(&job.message_id).await.is_err() {
                retry_transient(job, ctx, true).await;
                dead = true;
                break;
            }
            job.sent_after_loss = ctx.losses.connections.load(Ordering::Acquire) > 0;
            inflight.push_back(job);
        }

        // Flush queued requests, then drain exactly one response (the oldest
        // in-flight) before looping to refill — this keeps the window full.
        // A pause cuts the read short: the top of the loop gives the window
        // back and closes the connection mid-response.
        if !dead && !inflight.is_empty() {
            let conn = connection.as_mut().expect("connection acquired above");
            let oldest = &inflight[0].message_id;
            // A suspect goes alone, so its reply starts at once or not at
            // all: don't sit out the full head timeout when the connection
            // died without the reset reaching us (see `SUSPECT_HEAD_TIMEOUT`).
            let suspect = inflight[0].suspect;
            let read = async {
                conn.flush().await.ok()?;
                Some(if suspect {
                    conn.read_body_outcome_within(oldest, SUSPECT_HEAD_TIMEOUT)
                        .await
                } else {
                    conn.read_body_outcome(oldest).await
                })
            };
            let outcome = tokio::select! {
                biased;
                _ = until_paused(&mut pause_rx) => continue,
                outcome = read => outcome,
            };
            ctx.job.add_wire_bytes(conn.take_bytes_read());
            if let Some(outcome) = outcome {
                let job = inflight
                    .pop_front()
                    .expect("the response read was in flight");
                match outcome {
                    ArticleOutcome::Transient if conn.is_poisoned() => {
                        // The connection died mid-response (reset, closed, a
                        // reply out of step, a timeout): this article and the
                        // rest of the window are dealt with below.
                        inflight.push_front(job);
                        dead = true;
                    }
                    ArticleOutcome::Transient => {
                        // Connection alive but the server refused the BODY
                        // (e.g. 412 "no newsgroup selected"): force a group
                        // re-selection so a recoverable case recovers. The
                        // counted retry stops a strict server that never
                        // serves from looping forever.
                        group_ready = false;
                        retry_transient(job, ctx, false).await;
                    }
                    other => handle_outcome(job, other, ctx).await,
                }
            } else {
                // The flush failed.
                dead = true;
            }
        }

        if dead {
            // Drop the dead connection (the pool closes it) and re-queue the
            // requests in flight (their unread responses are unreliable) for
            // a fresh connection, along with a suspect waiting to go.
            connection = None;
            group_ready = false;
            give_back(held.take(), ctx);
            lost_window(inflight.drain(..).collect(), ctx).await;
        }
    }
}

/// Resolves once the job is paused; for `select!`. (Never, should the job's
/// pause signal go away: the job is gone, and the worker with it.)
async fn until_paused(pause_rx: &mut tokio::sync::watch::Receiver<bool>) {
    if pause_rx.wait_for(|paused| *paused).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Hand articles back to the queue as they are, without spending any of
/// their retries: their requests were abandoned through no fault of theirs
/// (a pause, or a connection the server would not give).
fn give_back(jobs: impl IntoIterator<Item = ArticleJob>, ctx: &WorkerCtx) {
    for job in jobs {
        let _ = ctx.feedback_tx.send(WorkerFeedback::Retry(job));
    }
}

/// Settle an article as a permanent failure: record it for the file and tell
/// the coordinator this article is done.
async fn fail_article(job: ArticleJob, ctx: &WorkerCtx) {
    let _ = ctx
        .write_tx
        .send(WriteJob::Failed {
            file: job.file,
            index: job.index,
            message_id: job.message_id,
            encoded_size: job.encoded_bytes,
        })
        .await;
    let _ = ctx.feedback_tx.send(WorkerFeedback::Settled);
}

/// Re-queue an article after a transient failure, counting it against a
/// generous budget: the server refused it on a live connection (412), or the
/// connection failed before its request could be sent (`unsent`: selecting
/// the group, or sending; an article given up that way is not missing, see
/// [`Downloader::judge_connection_failures`]). Re-enqueue is immediate (no
/// inline sleep that would stall the connection's pipeline window); pacing
/// comes from connection re-acquisition and the group re-selection
/// round-trip. A connection lost with requests in flight is
/// [`lost_window`]'s.
async fn retry_transient(mut job: ArticleJob, ctx: &WorkerCtx, unsent: bool) {
    job.transient_attempts = job.transient_attempts.saturating_add(1);
    if job.transient_attempts as usize > ctx.max_transient_retries {
        if unsent {
            ctx.losses.unsent.fetch_add(1, Ordering::AcqRel);
            ctx.job.add_connection_failures(1);
        }
        fail_article(job, ctx).await;
    } else {
        let _ = ctx.feedback_tx.send(WorkerFeedback::Retry(job));
    }
}

/// A connection was lost with `window` in flight (oldest first). The loss
/// showed while the oldest one's reply was being read, but with several in
/// flight that doesn't make it the one the server choked on: a reset can
/// discard replies that had arrived but weren't read yet (macOS does). So the
/// oldest becomes a suspect, fetched on its own from then on, and the rest go
/// back as they are; nobody's retries are spent. An article that loses the
/// connection while it is the only request in flight is to blame for it:
/// after [`CONNECTIONS_LOST_MAX`] such losses it is given up as missing, so
/// one article that always kills the connection can't hold up the job (PAR2
/// may repair it) or drag the articles beside it down with it. Unless the
/// server is flaky ([`LossTally::flaky`]): then such an article goes back
/// too, and after [`CONNECTIONS_LOST_FLAKY_MAX`] losses it is given up as
/// lost to the connection, not missing. Every loss makes a suspect or blames
/// one, so the losses are bounded. Whether the server rather than the
/// articles was to blame is judged at the phase's end
/// ([`Downloader::judge_connection_failures`]).
async fn lost_window(window: Vec<ArticleJob>, ctx: &WorkerCtx) {
    let alone = window.len() == 1;
    let mut window = window.into_iter();
    let Some(mut oldest) = window.next() else {
        return;
    };
    ctx.losses.connections.fetch_add(1, Ordering::AcqRel);
    oldest.suspect = true;
    if alone {
        oldest.connections_lost = oldest.connections_lost.saturating_add(1);
        if oldest.connections_lost == 1 {
            ctx.losses.lost_alone.fetch_add(1, Ordering::AcqRel);
        }
        let flaky = ctx.losses.flaky();
        if !flaky && oldest.connections_lost >= CONNECTIONS_LOST_MAX {
            tracing::debug!(
                "{} lost the connection {} times on its own: given up",
                oldest.message_id,
                oldest.connections_lost
            );
            ctx.losses.killers.fetch_add(1, Ordering::AcqRel);
            fail_article(oldest, ctx).await;
            return;
        }
        if flaky && oldest.connections_lost >= CONNECTIONS_LOST_FLAKY_MAX {
            tracing::debug!(
                "{} lost the connection {} times on its own, on a flaky server: given up",
                oldest.message_id,
                oldest.connections_lost
            );
            ctx.losses.flaky_given_up.fetch_add(1, Ordering::AcqRel);
            fail_article(oldest, ctx).await;
            return;
        }
    }
    let _ = ctx.feedback_tx.send(WorkerFeedback::Retry(oldest));
    give_back(window, ctx);
}

/// Apply a non-transient article outcome: write good data, permanently fail
/// missing articles, and bounded-retry decode failures. (Transient outcomes are
/// handled in the worker loop, where the connection's poison state is known.)
async fn handle_outcome(job: ArticleJob, outcome: ArticleOutcome, ctx: &WorkerCtx) {
    match outcome {
        ArticleOutcome::Ok {
            offset,
            data,
            crc_verified,
            ..
        } => {
            // The article itself says where its data goes (`=ypart`); one
            // placed far past the file's end is a bad article.
            let end = offset.saturating_add(data.len() as u64);
            if !job.file.placement.admit(end) {
                tracing::debug!(
                    "{} ends at byte {} of {}, past the {} bytes the NZB allows",
                    job.message_id,
                    end,
                    job.file.state.filename,
                    job.file.placement.limit
                );
                return retry_decode(job, ctx).await;
            }
            // Track wire-CRC coverage per file: if any segment carried no
            // checksum (size-checked only), the file isn't fully wire-verified,
            // so post-processing must run a real PAR2 verify rather than trust
            // the download. (See the skip-verify gate in PostProcessor.)
            if !crc_verified {
                job.file
                    .state
                    .all_crc_verified
                    .store(false, Ordering::Relaxed);
            }
            if job.sent_after_loss {
                ctx.losses.arrived_since.fetch_add(1, Ordering::AcqRel);
            }
            if job.connections_lost > 0 {
                ctx.losses.lost_alone_arrived.fetch_add(1, Ordering::AcqRel);
            }
            let _ = ctx
                .write_tx
                .send(WriteJob::Write {
                    file: job.file,
                    index: job.index,
                    offset,
                    data,
                    encoded_size: job.encoded_bytes,
                })
                .await;
            let _ = ctx.feedback_tx.send(WorkerFeedback::Settled);
        }
        ArticleOutcome::Missing => {
            fail_article(job, ctx).await;
        }
        ArticleOutcome::DecodeFailed => retry_decode(job, ctx).await,
        ArticleOutcome::Transient => {
            // Unreachable: handled by the worker loop. Re-queue defensively.
            retry_transient(job, ctx, true).await;
        }
    }
}

/// A bad article body (yEnc CRC/size mismatch, or data placed outside the
/// file): re-queue it a bounded number of times, then give it up.
async fn retry_decode(mut job: ArticleJob, ctx: &WorkerCtx) {
    job.decode_attempts = job.decode_attempts.saturating_add(1);
    if job.decode_attempts >= ctx.decode_retry_cap {
        fail_article(job, ctx).await;
    } else {
        // Re-queue immediately; the cap bounds the loop without an inline
        // sleep that would stall the other in-flight responses.
        let _ = ctx.feedback_tx.send(WorkerFeedback::Retry(job));
    }
}

/// Check out a connection, retrying with backoff; the last error after 6
/// failures. Gives up after the first failure when other workers have
/// connections (see [`Census::others_connected`]): the server is there,
/// and hammering it for one more connection won't help. A stop interrupts
/// the waits through the worker's cancellation race.
async fn acquire_connection(
    ctx: &WorkerCtx,
    acquired_before: u64,
    worker_id: usize,
) -> Result<PooledConnection> {
    let mut attempt: u32 = 0;
    loop {
        match ctx.pool.get_connection().await {
            Ok(conn) => {
                // Health-check traffic counts toward this job's wire bytes.
                ctx.job.add_wire_bytes(conn.take_bytes_read());
                return Ok(conn);
            }
            Err(e) => {
                attempt = attempt.saturating_add(1);
                tracing::debug!("Worker {} connection acquire failed: {}", worker_id, e);
                if attempt >= 6 || ctx.census.others_connected(acquired_before) {
                    return Err(e);
                }
                let delay = backoff_delay(ctx.retry_delay, attempt as usize);
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// `base` doubled `attempt` times (at most 16x), kept within 100 ms to 8 s.
pub(crate) fn backoff_delay(base: Duration, attempt: usize) -> Duration {
    let factor = 1u64 << attempt.min(4) as u64;
    base.saturating_mul(factor as u32)
        .clamp(Duration::from_millis(100), Duration::from_secs(8))
}

// --- Writer ---

/// When the writer processed its last `WriteJob`, and the job's wire-byte
/// count then: the end of the transfer window. Wire bytes are plaintext socket
/// bytes (yEnc framing, escapes, NNTP overhead — everything except TLS/TCP/IP
/// headers), the honest numerator for an average speed.
struct LastWrite {
    at: Instant,
    wire_bytes: u64,
}

/// Returns the last write, or `None` if no segments were processed (empty NZB
/// or a stop before any segment arrived). Keeps draining after a stop: every
/// article already decoded is written, so it's on disk for resume. The writer
/// is the only place that knows an article reached the file, so it is what
/// records it in the job record (after the write returned, never before).
async fn run_writer(
    mut rx: mpsc::Receiver<WriteJob>,
    job: Arc<JobCtx>,
    fsync_on_finalize: bool,
    record: Option<Arc<JobRecord>>,
) -> Option<LastWrite> {
    let mut last: Option<LastWrite> = None;
    let mut finalize_tasks = tokio::task::JoinSet::new();
    while let Some(write) = rx.recv().await {
        last = Some(LastWrite {
            at: Instant::now(),
            wire_bytes: job.wire_bytes(),
        });

        let (file, encoded_size) = match write {
            WriteJob::Write {
                file,
                index,
                offset,
                data,
                encoded_size,
            } => {
                handle_write(&file, index, offset, data, &job, record.as_deref()).await;
                (file, encoded_size)
            }
            WriteJob::Failed {
                file,
                index,
                message_id,
                encoded_size,
            } => {
                tracing::debug!("Failed segment {} in {}", message_id, file.state.filename);
                file.state.segments_failed.fetch_add(1, Ordering::Relaxed);
                job.add_articles_failed(1);
                if let (Some(record), Some(slot)) = (&record, file.state.slot) {
                    record.failed(slot, index);
                }
                (file, encoded_size)
            }
        };

        job.add_bytes_done(encoded_size);

        if file.state.mark_settled() {
            job.add_files_done(1);
            // Finalize off the writer task: the `set_len` + `sync_data` +
            // `rename` chain is slow (tens of ms) and would otherwise stall
            // writes to *other* files queued behind us.
            finalize_tasks.spawn(finalize_file(file, fsync_on_finalize, record.clone()));
        }
    }
    while finalize_tasks.join_next().await.is_some() {}
    last
}

async fn handle_write(
    file: &Arc<FileHandle>,
    index: u32,
    offset: u64,
    data: Bytes,
    job: &JobCtx,
    record: Option<&JobRecord>,
) {
    let data_len = data.len() as u64;
    let file_handle = file.file.clone();
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
            if let (Some(record), Some(slot)) = (record, file.state.slot) {
                record.written(slot, index, offset + data_len);
            }
            return;
        }
        Ok(Err(e)) => {
            tracing::warn!("write to {}: {}", file.state.partial_path.display(), e);
        }
        Err(e) => {
            tracing::warn!(
                "write task panicked for {}: {}",
                file.state.partial_path.display(),
                e
            );
        }
    }
    file.state.segments_failed.fetch_add(1, Ordering::Relaxed);
    job.add_articles_failed(1);
    if let (Some(record), Some(slot)) = (record, file.state.slot) {
        record.failed(slot, index);
    }
}

/// Truncate the partial file to the highest written byte and rename it to its
/// final name. If nothing was written, remove the partial file. Only called
/// once every article of the file has settled, so a stopped job never renames
/// an incomplete file to its final name: it stays `<name>.partial`. Recorded
/// as finalized once the rename is done.
async fn finalize_file(file: Arc<FileHandle>, fsync: bool, record: Option<Arc<JobRecord>>) {
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
        // fsync is opt-in: PAR2 verifies integrity and a crash just means
        // re-download, so we skip the (potentially hundreds of) fsync barriers
        // by default — a large wall-clock win on big sets / slow disks.
        if fsync {
            file_handle.sync_data()?;
        }
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
            if let (Some(record), Some(slot)) = (record, file.state.slot) {
                record.finalized(slot);
            }
            if segments_failed > 0 {
                tracing::debug!(
                    "{} finalized with {} missing segments",
                    filename,
                    segments_failed
                );
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

/// [`create_preallocated_file`] for each `(path, size, truncate)`, in order,
/// all in one blocking task.
async fn create_preallocated_files(
    files: Vec<(PathBuf, u64, bool)>,
) -> Vec<std::io::Result<StdFile>> {
    let count = files.len();
    tokio::task::spawn_blocking(move || {
        files
            .iter()
            .map(|(path, size, truncate)| create_preallocated_file(path, *size, *truncate))
            .collect()
    })
    .await
    .unwrap_or_else(|e| {
        let e = e.to_string();
        (0..count)
            .map(|_| Err(std::io::Error::other(e.clone())))
            .collect()
    })
}

/// Open (creating if needed) the `.partial` file and pre-allocate it sparsely
/// to `size`. A fresh download truncates any leftover; a resumed one keeps the
/// existing content and only grows the file.
fn create_preallocated_file(path: &Path, size: u64, truncate: bool) -> std::io::Result<StdFile> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(truncate)
        .open(path)?;
    // Final size is set later by `set_len(max_byte_position)`.
    if file.metadata()?.len() < size {
        file.set_len(size)?;
    }
    Ok(file)
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
    fn backoff_increases_then_caps() {
        let base = Duration::from_millis(500);
        let d1 = backoff_delay(base, 1);
        let d2 = backoff_delay(base, 2);
        let d5 = backoff_delay(base, 5);
        assert!(d2 >= d1);
        assert!(d5 <= Duration::from_secs(8));
    }
}
