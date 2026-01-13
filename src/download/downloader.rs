use futures::stream::{self, StreamExt};
use indicatif::ProgressBar;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex;

use super::nzb::{Nzb, NzbFile};
use crate::config::Config;
use crate::error::{DlNzbError, DownloadError};
use crate::nntp::{NntpPool, NntpPoolBuilder, NntpPoolExt, SegmentRequest};
use crate::progress;

type Result<T> = std::result::Result<T, DlNzbError>;

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
        let pool = NntpPoolBuilder::new(config.usenet.clone())
            .max_size(config.usenet.connections as usize)
            .build()?;

        Ok(Self { pool })
    }

    /// Check article availability before downloading
    /// Returns (available_count, missing_count, sample_size)
    pub async fn check_availability(&self, nzb: &Nzb) -> Result<(usize, usize, usize)> {
        let all_files: Vec<&NzbFile> = nzb.files().iter().collect();
        if all_files.is_empty() {
            return Ok((0, 0, 0));
        }

        // Get a connection for checking
        let mut conn = self.pool.get_connection().await?;

        // Sample segments from files to check availability
        let mut sample_requests: Vec<SegmentRequest> = Vec::new();
        let group = &all_files[0].groups.group[0].name;

        // Sample first segment from each file (up to 20 files)
        for file in all_files.iter().take(20) {
            if let Some(segment) = file.segments.segment.first() {
                sample_requests.push(SegmentRequest {
                    message_id: segment.message_id.clone(),
                    group: group.clone(),
                    segment_number: segment.number,
                });
            }
        }

        let results = conn.check_articles_exist(&sample_requests).await?;
        let available = results.iter().filter(|(_, exists)| *exists).count();
        let missing = results.len() - available;

        Ok((available, missing, results.len()))
    }

    /// Download all files from an NZB, returns results and progress bar for reuse
    pub async fn download_nzb(
        &self,
        nzb: &Nzb,
        config: Config,
    ) -> Result<(Vec<DownloadResult>, ProgressBar)> {
        config.ensure_dirs()?;

        // Get all files to download (no separation between main and PAR2)
        let all_files: Vec<&NzbFile> = nzb.files().iter().collect();

        if all_files.is_empty() {
            return Err(DownloadError::InsufficientSegments {
                available: 0,
                required: 1,
            }
            .into());
        }

        // Create clean progress bar using centralized progress module
        let total_bytes: u64 = all_files
            .iter()
            .flat_map(|f| &f.segments.segment)
            .map(|s| s.bytes)
            .sum();

        let total_files = all_files.len();
        let progress_bar =
            progress::create_progress_bar(total_bytes, progress::ProgressStyle::Download);
        progress_bar.set_message(format!("({}/{})", 0, total_files));

        // Download all files concurrently
        let results = self
            .download_files_concurrent_with_config(&all_files, progress_bar.clone(), config)
            .await?;

        // Finish the progress bar with clean formatting
        let total_downloaded: u64 = results.iter().map(|r| r.size).sum();
        let failed_files = results.iter().filter(|r| r.segments_failed > 0).count();

        progress_bar.set_position(total_bytes);

        if failed_files == 0 {
            progress_bar.finish_with_message(format!(
                "({}/{})  ",
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
                "({}/{})  ",
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

    /// Download multiple files concurrently with custom config
    async fn download_files_concurrent_with_config(
        &self,
        files: &[&NzbFile],
        progress_bar: ProgressBar,
        config: Config,
    ) -> Result<Vec<DownloadResult>> {
        let total_files = files.len();
        let completed_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        // Wrap config in Arc to avoid cloning per-file (Config contains strings and paths)
        let config = std::sync::Arc::new(config);

        // Sort files by size (largest first) to maximize initial throughput
        let mut sorted_files: Vec<&NzbFile> = files.iter().copied().collect();
        sorted_files.sort_by_key(|f| std::cmp::Reverse(f.segments.segment.len()));

        let download_futures = sorted_files.iter().map(|file| {
            let pool = self.pool.clone();
            let config = config.clone(); // Now clones Arc, not Config
            let file = (*file).clone();
            let progress = progress_bar.clone();
            let completed = completed_count.clone();

            async move {
                let result =
                    Self::download_file_with_pool(file, &config, pool, progress.clone()).await;

                // Update file counter (only update every 5 files to reduce overhead)
                let count = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if count % 5 == 0 || count == total_files {
                    progress.set_message(format!("({}/{})", count, total_files));
                }

                result
            }
        });

        // Process downloads with bounded concurrency to prevent pool exhaustion
        // Each file uses multiple connections for its batches, so limit concurrent files
        // to avoid total_batches = files × batches_per_file >> pool_size
        let max_concurrent_files = (config.usenet.connections as usize / 5).max(2);
        let results: Vec<Result<DownloadResult>> = stream::iter(download_futures)
            .buffer_unordered(max_concurrent_files)
            .collect()
            .await;

        // Collect successful results
        let mut successful_results = Vec::new();
        for result in results {
            match result {
                Ok(download_result) => successful_results.push(download_result),
                Err(e) => eprintln!("Download failed: {}", e),
            }
        }

        Ok(successful_results)
    }

    /// Download a single file using the connection pool
    /// Uses direct file writes with seek to avoid buffering all segments in memory
    async fn download_file_with_pool(
        file: NzbFile,
        config: &Config,
        pool: NntpPool,
        progress_bar: ProgressBar,
    ) -> Result<DownloadResult> {
        let filename = Nzb::get_filename_from_subject(&file.subject)
            .unwrap_or_else(|| format!("unknown_file_{}", file.date));

        let output_path = config.download.dir.join(&filename);

        // Calculate expected size and segment offsets
        let segment_sizes: Vec<u64> = file.segments.segment.iter().map(|s| s.bytes).collect();
        let expected_size: u64 = segment_sizes.iter().sum();

        // Pre-calculate byte offsets for each segment
        let mut segment_offsets: Vec<u64> = Vec::with_capacity(segment_sizes.len());
        let mut offset = 0u64;
        for &size in &segment_sizes {
            segment_offsets.push(offset);
            offset += size;
        }

        // Check if file already exists with correct size and valid content
        if !config.download.force_redownload {
            if let Ok(metadata) = tokio::fs::metadata(&output_path).await {
                if metadata.len() == expected_size {
                    // Verify file has real content (not zero-filled from pre-allocation)
                    let is_valid = match Self::verify_file_not_empty(&output_path).await {
                        Ok(valid) => valid,
                        Err(_) => false,
                    };

                    if is_valid {
                        if progress_bar.is_hidden() {
                            eprintln!("  Skipping complete: {}", filename);
                        } else {
                            progress_bar
                                .println(format!("  \x1b[90m↳ Skipping: {}\x1b[0m", filename));
                        }
                        return Ok(DownloadResult {
                            filename,
                            path: output_path,
                            size: expected_size,
                            segments_downloaded: file.segments.segment.len(),
                            segments_failed: 0,
                            download_time: Duration::from_secs(0),
                            average_speed: 0.0,
                            failed_message_ids: Vec::new(),
                        });
                    }
                }
            }
        }

        let start_time = Instant::now();

        // Create and pre-allocate output file
        let output_file = File::create(&output_path).await?;
        output_file.set_len(expected_size).await?;

        // Shared file handle for concurrent writes
        let shared_file = Arc::new(Mutex::new(output_file));

        // Shared statistics using atomics
        let segments_downloaded = Arc::new(AtomicUsize::new(0));
        let segments_failed = Arc::new(AtomicUsize::new(0));
        let actual_size = Arc::new(AtomicU64::new(0));
        let failed_message_ids = Arc::new(Mutex::new(Vec::new()));

        // Prepare segment downloads using pipelining
        let group = &file.groups.group[0].name;

        // Create segment requests with their offsets
        let segment_requests: Vec<(SegmentRequest, u64)> = file
            .segments
            .segment
            .iter()
            .enumerate()
            .map(|(idx, segment)| {
                (
                    SegmentRequest {
                        message_id: segment.message_id.clone(),
                        group: group.clone(),
                        segment_number: segment.number,
                    },
                    segment_offsets[idx],
                )
            })
            .collect();

        // Pipeline size: how many segments to request per connection
        let pipeline_size = config.tuning.pipeline_size;
        let num_connections = config.usenet.connections as usize;
        let connection_wait_timeout = config.tuning.connection_wait_timeout;

        // Split into batches for pipelining
        let batches: Vec<Vec<(SegmentRequest, u64)>> = segment_requests
            .chunks(pipeline_size)
            .map(|chunk| chunk.to_vec())
            .collect();

        // Download batches in parallel and write directly to file
        let batch_futures = batches.into_iter().map(|batch| {
            let pool = pool.clone();
            let progress = progress_bar.clone();
            let shared_file = shared_file.clone();
            let segments_downloaded = segments_downloaded.clone();
            let segments_failed = segments_failed.clone();
            let actual_size = actual_size.clone();
            let failed_message_ids = failed_message_ids.clone();
            let segment_sizes = segment_sizes.clone();

            async move {
                // Get connection from pool with patient retry
                let mut conn = None;
                let mut attempt = 0u32;
                let start = Instant::now();
                let max_wait = Duration::from_secs(connection_wait_timeout);

                while conn.is_none() && start.elapsed() < max_wait {
                    if attempt > 0 {
                        let delay = Duration::from_millis(500) * (1 << attempt.min(4));
                        tokio::time::sleep(delay).await;

                        if attempt % 5 == 0 && !progress.is_hidden() {
                            progress.println(format!(
                                "  \x1b[90m⏳ Waiting for connection... ({:.0}s)\x1b[0m",
                                start.elapsed().as_secs_f64()
                            ));
                        }
                    }

                    match tokio::time::timeout(Duration::from_secs(60), pool.get_connection()).await
                    {
                        Ok(Ok(c)) => conn = Some(c),
                        Ok(Err(_)) | Err(_) => attempt += 1,
                    }
                }

                let mut conn = match conn {
                    Some(c) => c,
                    None => {
                        if progress.is_hidden() {
                            eprintln!(
                                "  Warning: Could not get connection after {:?}",
                                start.elapsed()
                            );
                        } else {
                            progress.println(format!(
                                "  \x1b[33m⚠ Connection unavailable, batch skipped\x1b[0m"
                            ));
                        }
                        // Mark all segments in batch as failed
                        for (req, _) in &batch {
                            segments_failed.fetch_add(1, Ordering::Relaxed);
                            if let Some(idx) = (req.segment_number as usize).checked_sub(1) {
                                if idx < segment_sizes.len() {
                                    progress.inc(segment_sizes[idx]);
                                }
                            }
                            failed_message_ids.lock().await.push(req.message_id.clone());
                        }
                        return;
                    }
                };

                // Extract just the requests for pipelining
                let requests: Vec<SegmentRequest> =
                    batch.iter().map(|(req, _)| req.clone()).collect();

                // Build offset lookup
                let offset_map: std::collections::HashMap<u32, u64> = batch
                    .iter()
                    .map(|(req, offset)| (req.segment_number, *offset))
                    .collect();

                // Download pipelined batch
                match conn.download_segments_pipelined(&requests).await {
                    Ok(results) => {
                        for (seg_num, data) in results {
                            let idx = (seg_num as usize).saturating_sub(1);
                            let seg_expected_size =
                                segment_sizes.get(idx).copied().unwrap_or_default();

                            if let Some(bytes) = data {
                                // Write directly to file at the correct offset
                                if let Some(&file_offset) = offset_map.get(&seg_num) {
                                    let mut file = shared_file.lock().await;
                                    if let Err(e) =
                                        file.seek(std::io::SeekFrom::Start(file_offset)).await
                                    {
                                        tracing::debug!(
                                            "Seek failed for segment {}: {}",
                                            seg_num,
                                            e
                                        );
                                        segments_failed.fetch_add(1, Ordering::Relaxed);
                                    } else if let Err(e) = file.write_all(&bytes).await {
                                        tracing::debug!(
                                            "Write failed for segment {}: {}",
                                            seg_num,
                                            e
                                        );
                                        segments_failed.fetch_add(1, Ordering::Relaxed);
                                    } else {
                                        segments_downloaded.fetch_add(1, Ordering::Relaxed);
                                        actual_size
                                            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                                    }
                                }
                            } else {
                                segments_failed.fetch_add(1, Ordering::Relaxed);
                                if let Some(req) =
                                    requests.iter().find(|r| r.segment_number == seg_num)
                                {
                                    failed_message_ids.lock().await.push(req.message_id.clone());
                                }
                            }
                            progress.inc(seg_expected_size);
                        }
                    }
                    Err(_) => {
                        // Failed - mark all as failed
                        for (req, _) in &batch {
                            segments_failed.fetch_add(1, Ordering::Relaxed);
                            if let Some(idx) = (req.segment_number as usize).checked_sub(1) {
                                if idx < segment_sizes.len() {
                                    progress.inc(segment_sizes[idx]);
                                }
                            }
                            failed_message_ids.lock().await.push(req.message_id.clone());
                        }
                    }
                }
            }
        });

        // Execute all batches concurrently
        stream::iter(batch_futures)
            .buffer_unordered(num_connections)
            .collect::<Vec<()>>()
            .await;

        // Ensure file is synced to disk
        {
            let file = shared_file.lock().await;
            file.sync_all().await?;
        }

        let download_time = start_time.elapsed();
        let final_size = actual_size.load(Ordering::Relaxed);
        let average_speed = if download_time.as_secs() > 0 {
            (final_size as f64 / 1024.0 / 1024.0) / download_time.as_secs_f64()
        } else {
            0.0
        };

        // Extract failed message IDs
        let failed_ids = Arc::try_unwrap(failed_message_ids)
            .map(|mutex| mutex.into_inner())
            .unwrap_or_else(|arc| {
                // If we can't unwrap (other references exist), block to get the data
                futures::executor::block_on(async { arc.lock().await.clone() })
            });

        Ok(DownloadResult {
            filename,
            path: output_path,
            size: final_size,
            segments_downloaded: segments_downloaded.load(Ordering::Relaxed),
            segments_failed: segments_failed.load(Ordering::Relaxed),
            download_time,
            average_speed,
            failed_message_ids: failed_ids,
        })
    }

    /// Clean up partial files after failed download
    pub async fn cleanup_partial_files(results: &[DownloadResult]) -> Result<usize> {
        let mut cleaned_count = 0;

        for result in results {
            // Only clean up files with failed segments
            if result.segments_failed > 0 && result.path.exists() {
                match tokio::fs::remove_file(&result.path).await {
                    Ok(_) => {
                        tracing::debug!("Cleaned up partial file: {}", result.path.display());
                        cleaned_count += 1;
                    }
                    Err(e) => {
                        tracing::debug!("Failed to clean up {}: {}", result.path.display(), e);
                    }
                }
            }
        }

        Ok(cleaned_count)
    }

    /// Verify a file has real content (not all zeros from pre-allocation)
    /// Samples start, middle, and end positions to detect zero-filled files
    async fn verify_file_not_empty(path: &std::path::Path) -> Result<bool> {
        use tokio::io::AsyncReadExt;

        let mut file = tokio::fs::File::open(path).await?;
        let metadata = file.metadata().await?;
        let file_size = metadata.len();

        // For very small files, just check if any byte is non-zero
        if file_size < 1024 {
            let mut buf = vec![0u8; file_size as usize];
            file.read_exact(&mut buf).await?;
            return Ok(buf.iter().any(|&b| b != 0));
        }

        // Sample start, middle, and end of file
        let positions = [0, file_size / 2, file_size.saturating_sub(1024)];
        let mut buf = [0u8; 1024];

        for pos in positions {
            file.seek(std::io::SeekFrom::Start(pos)).await?;
            let n = file.read(&mut buf).await?;
            if n > 0 && buf[..n].iter().any(|&b| b != 0) {
                return Ok(true);
            }
        }

        Ok(false)
    }
}
