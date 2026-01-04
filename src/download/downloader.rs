use futures::stream::{self, StreamExt};
use indicatif::ProgressBar;
use std::path::PathBuf;
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

/// Optimized downloader using connection pooling and streaming
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
    async fn download_file_with_pool(
        file: NzbFile,
        config: &Config,
        pool: NntpPool,
        progress_bar: ProgressBar,
    ) -> Result<DownloadResult> {
        let filename = Nzb::get_filename_from_subject(&file.subject)
            .unwrap_or_else(|| format!("unknown_file_{}", file.date));

        let output_path = config.download.dir.join(&filename);

        // Check if file already exists with correct size (safe resume)
        // Size check is sufficient - corruption will be caught by PAR2 verification
        if !config.download.force_redownload {
            let expected_size: u64 = file.segments.segment.iter().map(|s| s.bytes).sum();
            if let Ok(metadata) = tokio::fs::metadata(&output_path).await {
                if metadata.len() == expected_size {
                    // Log skip using progress bar for clean output
                    if progress_bar.is_hidden() {
                        eprintln!("  Skipping complete: {}", filename);
                    } else {
                        progress_bar.println(format!("  \x1b[90m↳ Skipping: {}\x1b[0m", filename));
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

        let start_time = Instant::now();

        // Create shared file handle for concurrent writes
        let output_file = File::create(&output_path).await?;

        // Pre-allocate file to expected size for sparse writing
        let expected_size: u64 = file.segments.segment.iter().map(|s| s.bytes).sum();
        output_file.set_len(expected_size).await?;

        let shared_file = Arc::new(Mutex::new(output_file));

        // Prepare segment downloads using pipelining
        let group = &file.groups.group[0].name; // Use first group

        // Calculate segment offsets based on expected sizes (segments are 1-indexed)
        let segment_offsets: Vec<u64> = {
            let mut offsets = Vec::with_capacity(file.segments.segment.len());
            let mut current_offset = 0u64;
            for segment in &file.segments.segment {
                offsets.push(current_offset);
                current_offset += segment.bytes;
            }
            offsets
        };

        // Create segment requests with their offsets
        let segment_requests: Vec<(SegmentRequest, u64)> = file
            .segments
            .segment
            .iter()
            .zip(segment_offsets.iter())
            .map(|(segment, &offset)| {
                (
                    SegmentRequest {
                        message_id: segment.message_id.clone(),
                        group: group.clone(),
                        segment_number: segment.number,
                    },
                    offset,
                )
            })
            .collect();

        // Pipeline size: how many segments to request per connection
        let pipeline_size = config.tuning.pipeline_size;

        // Split into batches for pipelining
        let num_connections = config.usenet.connections as usize;
        let batches: Vec<Vec<(SegmentRequest, u64)>> = segment_requests
            .chunks(pipeline_size)
            .map(|chunk| chunk.to_vec())
            .collect();

        // Track download statistics
        let segments_downloaded = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let segments_failed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let actual_size = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let failed_message_ids = Arc::new(Mutex::new(Vec::<String>::new()));

        // Download batches in parallel using connection pool
        let connection_wait_timeout = config.tuning.connection_wait_timeout;
        let batch_futures = batches.into_iter().map(|batch| {
            let pool = pool.clone();
            let progress = progress_bar.clone();
            let segment_bytes: Vec<u64> = file.segments.segment.iter().map(|s| s.bytes).collect();
            let shared_file = shared_file.clone();
            let segments_downloaded = segments_downloaded.clone();
            let segments_failed = segments_failed.clone();
            let actual_size = actual_size.clone();
            let failed_message_ids = failed_message_ids.clone();

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
                        Ok(Ok(c)) => {
                            conn = Some(c);
                        }
                        Ok(Err(_)) | Err(_) => {
                            attempt += 1;
                        }
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
                        segments_failed
                            .fetch_add(batch.len(), std::sync::atomic::Ordering::Relaxed);
                        for (req, _) in &batch {
                            let mut failed = failed_message_ids.lock().await;
                            failed.push(req.message_id.clone());
                        }
                        return;
                    }
                };

                // Extract just the segment requests for pipelining
                let requests: Vec<SegmentRequest> =
                    batch.iter().map(|(req, _)| req.clone()).collect();

                // Download pipelined batch
                match conn.download_segments_pipelined(&requests).await {
                    Ok(results) => {
                        // Write each segment immediately using seek
                        for (seg_num, data) in results {
                            // Find the offset for this segment
                            if let Some((_, offset)) =
                                batch.iter().find(|(req, _)| req.segment_number == seg_num)
                            {
                                if let Some(bytes) = data {
                                    // Write to file at correct offset
                                    let mut file = shared_file.lock().await;
                                    if file.seek(std::io::SeekFrom::Start(*offset)).await.is_ok() {
                                        if file.write_all(&bytes).await.is_ok() {
                                            segments_downloaded
                                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                            actual_size.fetch_add(
                                                bytes.len() as u64,
                                                std::sync::atomic::Ordering::Relaxed,
                                            );
                                        } else {
                                            segments_failed
                                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                        }
                                    } else {
                                        segments_failed
                                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    }

                                    // Update progress
                                    if let Some(idx) = (seg_num as usize).checked_sub(1) {
                                        if idx < segment_bytes.len() {
                                            progress.inc(segment_bytes[idx]);
                                        }
                                    }
                                } else {
                                    segments_failed
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    let mut failed = failed_message_ids.lock().await;
                                    if let Some((req, _)) =
                                        batch.iter().find(|(r, _)| r.segment_number == seg_num)
                                    {
                                        failed.push(req.message_id.clone());
                                    }

                                    // Still update progress for failed segments
                                    if let Some(idx) = (seg_num as usize).checked_sub(1) {
                                        if idx < segment_bytes.len() {
                                            progress.inc(segment_bytes[idx]);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => {
                        // Failed - mark all as failed and update progress
                        segments_failed
                            .fetch_add(batch.len(), std::sync::atomic::Ordering::Relaxed);
                        for (req, _) in &batch {
                            let mut failed = failed_message_ids.lock().await;
                            failed.push(req.message_id.clone());

                            if let Some(idx) = (req.segment_number as usize).checked_sub(1) {
                                if idx < segment_bytes.len() {
                                    progress.inc(segment_bytes[idx]);
                                }
                            }
                        }
                    }
                }
            }
        });

        // Execute batches matching connection pool size exactly
        stream::iter(batch_futures)
            .buffer_unordered(num_connections)
            .collect::<Vec<()>>()
            .await;

        // Flush and close the file
        {
            let mut file = shared_file.lock().await;
            file.flush().await?;
        }

        // Extract final statistics
        let final_downloaded = segments_downloaded.load(std::sync::atomic::Ordering::Relaxed);
        let final_failed = segments_failed.load(std::sync::atomic::Ordering::Relaxed);
        let final_size = actual_size.load(std::sync::atomic::Ordering::Relaxed);
        let final_failed_ids = {
            let ids = failed_message_ids.lock().await;
            ids.clone()
        };

        let download_time = start_time.elapsed();
        let average_speed = if download_time.as_secs() > 0 {
            (final_size as f64 / 1024.0 / 1024.0) / download_time.as_secs_f64()
        } else {
            0.0
        };

        Ok(DownloadResult {
            filename,
            path: output_path,
            size: final_size,
            segments_downloaded: final_downloaded,
            segments_failed: final_failed,
            download_time,
            average_speed,
            failed_message_ids: final_failed_ids,
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
}
