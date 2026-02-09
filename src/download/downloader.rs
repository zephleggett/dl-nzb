use bytes::Bytes;
use futures::stream::{self, StreamExt};
use indicatif::ProgressBar;

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};

use super::nzb::{Nzb, NzbFile};
use crate::config::Config;
use crate::error::{DlNzbError, DownloadError};
use crate::nntp::{NntpPool, NntpPoolBuilder, NntpPoolExt, SegmentRequest};
use crate::progress;

type Result<T> = std::result::Result<T, DlNzbError>;

// Configuration constants
/// Number of articles to check per batch during availability checking
const AVAILABILITY_BATCH_SIZE: usize = 5;

/// Minimum retry delay in milliseconds
const MIN_RETRY_DELAY_MS: u64 = 100;

/// Base connection retry delay in milliseconds
const CONNECTION_RETRY_BASE_DELAY_MS: u64 = 200;

/// Maximum filename length in bytes (accounts for filesystem limits)
const MAX_FILENAME_LENGTH: usize = 240;

/// How often to print connection wait status (every N attempts)
const CONNECTION_WAIT_STATUS_INTERVAL: u32 = 5;

/// How often to update file progress message (every N files)
const FILE_PROGRESS_UPDATE_INTERVAL: usize = 5;

/// Result of downloading a file
#[derive(Debug)]
pub struct DownloadResult {
    pub filename: String,
    pub path: PathBuf,
    pub size: u64,
    pub segments_downloaded: usize,
    pub segments_failed: usize,
    pub download_time: Duration,
    pub average_speed: f64,              // MB/s
    pub failed_message_ids: Vec<String>, // Track failed segments for potential retry
}

/// Optimized downloader using connection pooling and direct file writes
pub struct Downloader {
    pool: NntpPool,
}

impl Downloader {
    /// Create a new downloader with connection pool
    pub async fn new(config: Config) -> Result<Self> {
        // Convert connections count to usize, clamping to reasonable limits
        let max_connections = (config.usenet.connections as usize).min(usize::MAX);

        let pool = NntpPoolBuilder::new(config.usenet.clone())
            .max_size(max_connections)
            .max_concurrent_connections(config.tuning.max_concurrent_connections)
            .build()?;

        Ok(Self { pool })
    }

    /// Check article availability before downloading using parallel connections
    /// Returns (available_count, missing_count, sample_size, missing_first_segment_ids)
    /// The missing set contains first segment message IDs - if first segment is missing,
    /// the whole file should be skipped (all segments likely expired together)
    pub async fn check_availability(
        &self,
        nzb: &Nzb,
    ) -> Result<(usize, usize, usize, std::collections::HashSet<String>)> {
        use std::collections::HashSet;

        let all_files: Vec<&NzbFile> = nzb.files().iter().collect();
        if all_files.is_empty() {
            return Ok((0, 0, 0, HashSet::new()));
        }

        // Collect first segment from ALL files for checking, using each file's own group
        let sample_requests: Vec<SegmentRequest> = all_files
            .iter()
            .filter_map(|file| {
                let group = file.groups.group.first().map(|g| g.name.clone())?;
                file.segments.segment.first().map(|segment| SegmentRequest {
                    message_id: segment.message_id.clone(),
                    group,
                    segment_number: segment.number,
                })
            })
            .collect();

        if sample_requests.is_empty() {
            return Ok((0, 0, 0, HashSet::new()));
        }

        // Split into batches and check in parallel using available connections
        // Smaller batches = more parallelism for faster results
        let num_connections = self.pool.status().max_size; // Use all available connections

        let batches: Vec<Vec<SegmentRequest>> = sample_requests
            .chunks(AVAILABILITY_BATCH_SIZE)
            .map(|c| c.to_vec())
            .collect();

        let pool = self.pool.clone();
        let batch_futures = batches.into_iter().map(|batch| {
            let pool = pool.clone();
            async move {
                match pool.get_connection().await {
                    Ok(mut conn) => conn.check_articles_exist(&batch).await.unwrap_or_else(|e| {
                        eprintln!("  \x1b[33m⚠ Article check failed: {}\x1b[0m", e);
                        Vec::new()
                    }),
                    Err(e) => {
                        eprintln!("  \x1b[33m⚠ Connection failed during availability check: {}\x1b[0m", e);
                        Vec::new()
                    }
                }
            }
        });

        // Process batches in parallel
        let all_results: Vec<Vec<(String, bool)>> = stream::iter(batch_futures)
            .buffer_unordered(num_connections)
            .collect()
            .await;

        // Flatten results and collect missing first-segment message IDs
        let mut available = 0;
        let mut checked = 0;
        let mut missing_first_segments: HashSet<String> = HashSet::new();

        for results in all_results {
            for (msg_id, exists) in results {
                checked += 1;
                if exists {
                    available += 1;
                } else {
                    missing_first_segments.insert(msg_id);
                }
            }
        }

        let total = checked;
        let missing = total - available;

        Ok((available, missing, total, missing_first_segments))
    }

    /// Download all files from an NZB, returns results and progress bar for reuse
    /// Starts downloading immediately - 430 responses are handled inline during download
    pub async fn download_nzb(
        &self,
        nzb: &Nzb,
        config: Config,
        skip_message_ids: Option<&std::collections::HashSet<String>>,
    ) -> Result<(Vec<DownloadResult>, ProgressBar)> {
        config.ensure_dirs()?;

        // Download all files - no pre-filtering, handle 430s inline
        let all_files: Vec<&NzbFile> = nzb.files().iter().collect();

        if all_files.is_empty() {
            return Err(DownloadError::InsufficientSegments {
                available: 0,
                required: 1,
            }
            .into());
        }

        // Calculate total bytes for available files only
        let total_bytes: u64 = all_files
            .iter()
            .flat_map(|f| &f.segments.segment)
            .map(|s| s.bytes)
            .sum();

        let total_files = all_files.len();
        let progress_bar =
            progress::create_progress_bar(total_bytes, progress::ProgressStyle::Download);
        progress_bar.set_message(format!("({}/{})", 0, total_files));

        let results = self
            .download_files_with_workers(&all_files, progress_bar.clone(), config, skip_message_ids)
            .await?;

        // Finish the progress bar with clean formatting
        let total_downloaded: u64 = results.iter().map(|r| r.size).sum();
        let failed_files = results.iter().filter(|r| r.segments_failed > 0).count();

        progress_bar.set_position(total_downloaded.min(total_bytes));

        if failed_files == 0 {
            progress_bar.finish_with_message(format!(
                "({}/{})",
                all_files.len(),
                all_files.len()
            ));

            // Print download summary on new line with color
            println!(
                "  └─ \x1b[32m✓ Downloaded {}\x1b[0m",
                human_bytes::human_bytes(total_downloaded as f64)
            );
        } else {
            progress_bar.finish_with_message(format!(
                "({}/{})",
                all_files.len(),
                all_files.len()
            ));

            println!(
                "  └─ \x1b[33m! Downloaded {} ({} file{} with errors)\x1b[0m",
                human_bytes::human_bytes(total_downloaded as f64),
                failed_files,
                if failed_files == 1 { "" } else { "s" }
            );
        }

        Ok((results, progress_bar))
    }

    /// Download multiple files using a worker pool architecture
    ///
    /// This implements a three-stage pipeline:
    /// 1. Main task: Prepares segment batches and sends to workers
    /// 2. Download workers: Fetch segments from NNTP servers in parallel
    /// 3. Writer task: Writes downloaded segments to disk
    ///
    /// The pipeline uses channels for communication:
    /// - batch_tx → workers: Segment batches to download
    /// - write_tx → writer: Downloaded data or failure notifications
    ///
    /// Memory is controlled by channel capacities to prevent unbounded growth.
    /// Files are processed largest-first to maximize initial throughput.
    async fn download_files_with_workers(
        &self,
        files: &[&NzbFile],
        progress_bar: ProgressBar,
        config: Config,
        skip_message_ids: Option<&std::collections::HashSet<String>>,
    ) -> Result<Vec<DownloadResult>> {
        let total_files = files.len();
        let completed_count = Arc::new(AtomicUsize::new(0));
        let mut results = Vec::new();
        let mut file_states: Vec<Arc<FileState>> = Vec::new();

        let max_concurrent_files = config.memory.max_concurrent_files.max(1);
        let file_limit = Arc::new(Semaphore::new(max_concurrent_files));

        let pipeline_size = config.tuning.pipeline_size.max(1);
        let batch_capacity = (config
            .memory
            .max_segments_in_memory
            .saturating_div(pipeline_size)
            .max(1))
        .max(config.usenet.connections as usize);
        let write_capacity = config.memory.max_segments_in_memory.max(1);
        let retry_attempts = config.usenet.retry_attempts as usize;
        let retry_delay = Duration::from_millis(config.usenet.retry_delay);

        let (batch_tx, batch_rx) = mpsc::channel::<SegmentBatch>(batch_capacity);
        let (write_tx, write_rx) = mpsc::channel::<WriteJob>(write_capacity);

        let writer_handle = tokio::spawn(run_writer(
            write_rx,
            progress_bar.clone(),
            completed_count.clone(),
            total_files,
        ));

        // Create worker pool - one worker per connection
        let worker_count = (config.usenet.connections as usize).min(usize::MAX);
        let shared_rx = Arc::new(tokio::sync::Mutex::new(batch_rx));
        let mut worker_handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let rx = shared_rx.clone();
            let pool = self.pool.clone();
            let write_tx = write_tx.clone();
            let progress = progress_bar.clone();
            let wait_timeout = Duration::from_secs(config.tuning.connection_wait_timeout);
            worker_handles.push(tokio::spawn(download_worker(
                rx,
                pool,
                write_tx,
                progress,
                wait_timeout,
                retry_attempts,
                retry_delay,
            )));
        }
        drop(write_tx);

        // Sort files by size (largest first) to maximize initial throughput
        let mut sorted_files: Vec<&NzbFile> = files.to_vec();
        sorted_files.sort_by_key(|f| Reverse(f.segments.segment.len()));

        for file in sorted_files {
            let raw_filename = Nzb::get_filename_from_subject(&file.subject)
                .unwrap_or_else(|| format!("unknown_file_{}", file.date));
            let filename = sanitize_filename(&raw_filename, &format!("unknown_file_{}", file.date));

            let output_path = config.download.dir.join(&filename);

            // Calculate expected size
            let expected_size: u64 = file.segments.segment.iter().map(|s| s.bytes).sum();

            let group = match file.groups.group.first() {
                Some(g) => g.name.clone(),
                None => {
                    eprintln!("Missing group for {}, skipping", filename);
                    results.push(DownloadResult {
                        filename,
                        path: output_path,
                        size: 0,
                        segments_downloaded: 0,
                        segments_failed: file.segments.segment.len(),
                        download_time: Duration::from_secs(0),
                        average_speed: 0.0,
                        failed_message_ids: Vec::new(),
                    });
                    let count = completed_count.fetch_add(1, Ordering::Relaxed) + 1;
                    if count % FILE_PROGRESS_UPDATE_INTERVAL == 0 || count == total_files {
                        progress_bar.set_message(format!("({}/{})", count, total_files));
                    }
                    continue;
                }
            };

            let permit = match file_limit.clone().acquire_owned().await {
                Ok(p) => p,
                Err(_) => break,
            };

            let std_file = match create_preallocated_file(&output_path, expected_size).await {
                Ok(file) => file,
                Err(e) => {
                    eprintln!("Failed to create {}: {}", output_path.display(), e);
                    drop(permit);
                    results.push(DownloadResult {
                        filename,
                        path: output_path,
                        size: 0,
                        segments_downloaded: 0,
                        segments_failed: file.segments.segment.len(),
                        download_time: Duration::from_secs(0),
                        average_speed: 0.0,
                        failed_message_ids: Vec::new(),
                    });
                    let count = completed_count.fetch_add(1, Ordering::Relaxed) + 1;
                    if count % FILE_PROGRESS_UPDATE_INTERVAL == 0 || count == total_files {
                        progress_bar.set_message(format!("({}/{})", count, total_files));
                    }
                    continue;
                }
            };

            // Count only segments that will actually be processed (excluding skipped)
            let active_segments = if let Some(skip_ids) = skip_message_ids {
                file.segments
                    .segment
                    .iter()
                    .filter(|s| !skip_ids.contains(&s.message_id))
                    .count()
            } else {
                file.segments.segment.len()
            };

            // If all segments are skipped, skip this file entirely
            if active_segments == 0 {
                drop(permit);
                let count = completed_count.fetch_add(1, Ordering::Relaxed) + 1;
                if count % 5 == 0 || count == total_files {
                    progress_bar.set_message(format!("({}/{})", count, total_files));
                }
                continue;
            }

            let state = Arc::new(FileState::new(
                filename.clone(),
                output_path.clone(),
                active_segments,
                permit,
            ));
            file_states.push(state.clone());

            let handle = Arc::new(FileHandle {
                file: Arc::new(std_file),
                state,
            });

            let mut offset = 0u64;
            let mut batch_segments: Vec<SegmentMeta> = Vec::with_capacity(pipeline_size);

            for segment in &file.segments.segment {
                // Skip segments known to be missing from availability check
                if let Some(skip_ids) = skip_message_ids {
                    if skip_ids.contains(&segment.message_id) {
                        offset = offset.saturating_add(segment.bytes);
                        continue;
                    }
                }

                batch_segments.push(SegmentMeta {
                    message_id: segment.message_id.clone(),
                    segment_number: segment.number,
                    offset,
                });
                offset = offset.saturating_add(segment.bytes);

                if batch_segments.len() == pipeline_size {
                    let batch = SegmentBatch {
                        file: handle.clone(),
                        group: group.clone(),
                        segments: std::mem::take(&mut batch_segments),
                    };

                    if batch_tx.send(batch).await.is_err() {
                        break;
                    }
                }
            }

            if !batch_segments.is_empty() {
                let batch = SegmentBatch {
                    file: handle.clone(),
                    group: group.clone(),
                    segments: batch_segments,
                };

                if batch_tx.send(batch).await.is_err() {
                    break;
                }
            }
        }

        drop(batch_tx);

        // Wait for all workers to complete
        for (idx, handle) in worker_handles.into_iter().enumerate() {
            if let Err(e) = handle.await {
                eprintln!("  \x1b[33m⚠ Worker {} failed: {}\x1b[0m", idx, e);
            }
        }

        // Wait for writer to complete
        if let Err(e) = writer_handle.await {
            eprintln!("  \x1b[33m⚠ Writer task failed: {}\x1b[0m", e);
        }

        for state in file_states {
            let end_time = state
                .end_time
                .lock()
                .ok()
                .and_then(|t| *t)
                .unwrap_or_else(Instant::now);
            let download_time = end_time.duration_since(state.start_time);
            let bytes_written = state.bytes_written.load(Ordering::Relaxed);
            let average_speed = if download_time.as_secs() > 0 {
                (bytes_written as f64 / 1024.0 / 1024.0) / download_time.as_secs_f64()
            } else {
                0.0
            };

            let failed_ids = state
                .failed_message_ids
                .lock()
                .map(|ids| ids.clone())
                .unwrap_or_default();

            let segments_failed = state.segments_failed.load(Ordering::Relaxed);

            results.push(DownloadResult {
                filename: state.filename.clone(),
                path: state.path.clone(),
                size: bytes_written,
                segments_downloaded: state.segments_downloaded.load(Ordering::Relaxed),
                segments_failed,
                download_time,
                average_speed,
                failed_message_ids: failed_ids,
            });
        }

        Ok(results)
    }
}

#[derive(Clone)]
struct SegmentMeta {
    message_id: String,
    segment_number: u32,
    offset: u64,
}

struct SegmentBatch {
    file: Arc<FileHandle>,
    group: String,
    segments: Vec<SegmentMeta>,
}

struct FileHandle {
    file: Arc<std::fs::File>,
    state: Arc<FileState>,
}

struct FileState {
    filename: String,
    path: PathBuf,
    segments_total: usize,
    start_time: Instant,
    end_time: Mutex<Option<Instant>>,
    segments_done: AtomicUsize,
    segments_downloaded: AtomicUsize,
    segments_failed: AtomicUsize,
    bytes_written: AtomicU64,
    max_byte_position: AtomicU64,
    failed_message_ids: Mutex<Vec<String>>,
    file_limit_permit: Mutex<Option<OwnedSemaphorePermit>>,
}

impl FileState {
    fn new(
        filename: String,
        path: PathBuf,
        segments_total: usize,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            filename,
            path,
            segments_total,
            start_time: Instant::now(),
            end_time: Mutex::new(None),
            segments_done: AtomicUsize::new(0),
            segments_downloaded: AtomicUsize::new(0),
            segments_failed: AtomicUsize::new(0),
            bytes_written: AtomicU64::new(0),
            max_byte_position: AtomicU64::new(0),
            failed_message_ids: Mutex::new(Vec::new()),
            file_limit_permit: Mutex::new(Some(permit)),
        }
    }

    fn mark_done(&self) -> bool {
        let done = self.segments_done.fetch_add(1, Ordering::AcqRel) + 1;
        if done == self.segments_total {
            if let Ok(mut end_time) = self.end_time.lock() {
                if end_time.is_none() {
                    *end_time = Some(Instant::now());
                }
            }
            if let Ok(mut permit) = self.file_limit_permit.lock() {
                permit.take();
            }
            return true;
        }
        false
    }

    /// Finalize file download and truncate to actual written size
    /// Returns true if this was the final segment and file was finalized
    fn finalize_if_done(
        &self,
        file_handle: Arc<std::fs::File>,
        completed_files: &AtomicUsize,
        total_files: usize,
        progress_bar: &ProgressBar,
    ) -> bool {
        if !self.mark_done() {
            return false;
        }

        // Truncate file to the highest written byte position
        // (segments are written at offsets based on encoded sizes,
        // so max_byte_position tracks the actual extent of written data)
        let final_size = self.max_byte_position.load(Ordering::Relaxed);
        if final_size > 0 {
            let _ = tokio::task::spawn_blocking(move || file_handle.set_len(final_size));
        }

        let count = completed_files.fetch_add(1, Ordering::Relaxed) + 1;
        if count % FILE_PROGRESS_UPDATE_INTERVAL == 0 || count == total_files {
            progress_bar.set_message(format!("({}/{})", count, total_files));
        }

        true
    }
}

enum WriteJob {
    Write {
        file: Arc<FileHandle>,
        offset: u64,
        data: Bytes,
        message_id: String,
    },
    Failed {
        file: Arc<FileHandle>,
        message_id: String,
    },
}

/// Calculate exponential backoff delay with a minimum floor
fn calculate_backoff(base_delay: Duration, attempt: usize) -> Duration {
    let delay = base_delay.max(Duration::from_millis(MIN_RETRY_DELAY_MS));
    delay.checked_mul(1 << attempt.min(4)).unwrap_or(delay)
}

/// Worker task that downloads segment batches from NNTP server
///
/// Each worker:
/// - Maintains a single persistent NNTP connection (reused across batches)
/// - Receives SegmentBatch messages from a shared channel
/// - Downloads segments using pipelined NNTP commands for efficiency
/// - Sends results (WriteJob) to the writer task
/// - Implements retry logic with exponential backoff on failures
///
/// Connection management:
/// - Connections are lazily acquired and reused
/// - Failed connections are dropped and re-acquired on next batch
/// - Implements timeout-based waiting when pool is exhausted
async fn download_worker(
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<SegmentBatch>>>,
    pool: NntpPool,
    write_tx: mpsc::Sender<WriteJob>,
    progress: ProgressBar,
    wait_timeout: Duration,
    retry_attempts: usize,
    retry_delay: Duration,
) {
    let mut conn = None;

    loop {
        let batch = {
            let mut locked = rx.lock().await;
            locked.recv().await
        };

        let batch = match batch {
            Some(b) => b,
            None => break,
        };

        let requests: Vec<SegmentRequest> = batch
            .segments
            .iter()
            .map(|segment| SegmentRequest {
                message_id: segment.message_id.clone(),
                group: batch.group.clone(),
                segment_number: segment.segment_number,
            })
            .collect();

        let mut attempt = 0usize;
        loop {
            if conn.is_none() {
                conn = get_connection_with_retry(&pool, wait_timeout, &progress).await;
            }

            let Some(conn_ref) = conn.as_mut() else {
                attempt += 1;
                if attempt > retry_attempts {
                    if progress.is_hidden() {
                        eprintln!("Warning: Could not get connection for batch");
                    } else {
                        progress
                            .println("  \x1b[33m⚠ Connection unavailable, batch skipped\x1b[0m");
                    }

                    for segment in &batch.segments {
                        if write_tx
                            .send(WriteJob::Failed {
                                file: batch.file.clone(),
                                message_id: segment.message_id.clone(),
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    break;
                }

                tokio::time::sleep(calculate_backoff(retry_delay, attempt)).await;
                continue;
            };

            let result = match conn_ref.download_segments_pipelined(&requests).await {
                Ok(results) => {
                    let mut remaining: HashMap<u32, &SegmentMeta> = batch
                        .segments
                        .iter()
                        .map(|segment| (segment.segment_number, segment))
                        .collect();

                    for (seg_num, data) in results {
                        let Some(segment) = remaining.remove(&seg_num) else {
                            continue;
                        };

                        if let Some(bytes) = data {
                            if write_tx
                                .send(WriteJob::Write {
                                    file: batch.file.clone(),
                                    offset: segment.offset,
                                    data: bytes,
                                    message_id: segment.message_id.clone(),
                                })
                                .await
                                .is_err()
                            {
                                return;
                            }
                        } else if write_tx
                            .send(WriteJob::Failed {
                                file: batch.file.clone(),
                                message_id: segment.message_id.clone(),
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }

                    if !remaining.is_empty() {
                        for segment in remaining.values() {
                            if write_tx
                                .send(WriteJob::Failed {
                                    file: batch.file.clone(),
                                    message_id: segment.message_id.clone(),
                                })
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }

                    Ok(())
                }
                Err(e) => Err(e),
            };

            match result {
                Ok(()) => break,
                Err(_) => {
                    conn = None;
                    attempt += 1;
                    if attempt > retry_attempts {
                        for segment in &batch.segments {
                            if write_tx
                                .send(WriteJob::Failed {
                                    file: batch.file.clone(),
                                    message_id: segment.message_id.clone(),
                                })
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        break;
                    }

                    let delay = retry_delay.max(Duration::from_millis(MIN_RETRY_DELAY_MS));
                    let backoff = delay.checked_mul(1 << attempt.min(4)).unwrap_or(delay);
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }
}

/// Attempt to acquire a connection from the pool with retry logic
///
/// Retries with exponential backoff up to the specified timeout.
/// Prints status messages periodically to inform user of delays.
/// Returns None if connection cannot be acquired within timeout.
async fn get_connection_with_retry(
    pool: &NntpPool,
    wait_timeout: Duration,
    progress: &ProgressBar,
) -> Option<crate::nntp::PooledConnection> {
    let start = Instant::now();
    let mut attempt = 0u32;

    while start.elapsed() < wait_timeout {
        match pool.get_connection().await {
            Ok(conn) => return Some(conn),
            Err(_) => {
                attempt = attempt.saturating_add(1);
                let delay = Duration::from_millis(CONNECTION_RETRY_BASE_DELAY_MS)
                    * (1 << attempt.min(4));
                tokio::time::sleep(delay).await;

                if attempt % CONNECTION_WAIT_STATUS_INTERVAL == 0 && !progress.is_hidden() {
                    progress.println(format!(
                        "  \x1b[90m⏳ Waiting for connection... ({:.0}s)\x1b[0m",
                        start.elapsed().as_secs_f64()
                    ));
                }
            }
        }
    }

    None
}

/// Writer task that handles all disk I/O operations
///
/// This task:
/// - Receives WriteJob messages from download workers
/// - Performs blocking writes using spawn_blocking to avoid blocking async runtime
/// - Tracks download progress and updates progress bar
/// - Truncates files to actual written size when complete
/// - Releases file semaphore permits when files complete
///
/// All writes use positioned I/O (pwrite/seek_write) to write segments
/// at specific offsets, allowing out-of-order segment completion.
async fn run_writer(
    mut rx: mpsc::Receiver<WriteJob>,
    progress_bar: ProgressBar,
    completed_files: Arc<AtomicUsize>,
    total_files: usize,
) {
    while let Some(job) = rx.recv().await {
        match job {
            WriteJob::Write {
                file,
                offset,
                data,
                message_id,
            } => {
                let data_len = data.len() as u64;
                let file_handle = file.file.clone();
                let write_result = tokio::task::spawn_blocking(move || {
                    let written = write_at(file_handle.as_ref(), data.as_ref(), offset)?;
                    if written != data_len as usize {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WriteZero,
                            "short write",
                        ));
                    }
                    Ok::<(), std::io::Error>(())
                })
                .await;

                let write_ok = matches!(write_result, Ok(Ok(())));

                if write_ok {
                    file.state
                        .segments_downloaded
                        .fetch_add(1, Ordering::Relaxed);
                    file.state
                        .bytes_written
                        .fetch_add(data_len, Ordering::Relaxed);
                    file.state
                        .max_byte_position
                        .fetch_max(offset + data_len, Ordering::Relaxed);
                    progress_bar.inc(data_len);
                } else {
                    file.state.segments_failed.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut ids) = file.state.failed_message_ids.lock() {
                        ids.push(message_id);
                    }
                }

                file.state.finalize_if_done(
                    file.file.clone(),
                    &completed_files,
                    total_files,
                    &progress_bar,
                );
            }
            WriteJob::Failed { file, message_id } => {
                file.state.segments_failed.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut ids) = file.state.failed_message_ids.lock() {
                    ids.push(message_id);
                }

                file.state.finalize_if_done(
                    file.file.clone(),
                    &completed_files,
                    total_files,
                    &progress_bar,
                );
            }
        }
    }
}

async fn create_preallocated_file(path: &Path, size: u64) -> Result<std::fs::File> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(&path)?;
        file.set_len(size)?;
        Ok::<std::fs::File, std::io::Error>(file)
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

    // Safely truncate to max length without breaking UTF-8 character boundaries
    if result.len() > MAX_FILENAME_LENGTH {
        // Find the character boundary at or before MAX_FILENAME_LENGTH
        let mut truncate_at = MAX_FILENAME_LENGTH;
        while truncate_at > 0 && !result.is_char_boundary(truncate_at) {
            truncate_at -= 1;
        }
        if truncate_at > 0 {
            result.truncate(truncate_at);
        }
    }

    result
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
