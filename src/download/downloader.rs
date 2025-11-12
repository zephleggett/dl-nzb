use bytes::Bytes;
use futures::stream::{self, StreamExt};
use indicatif::ProgressBar;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};

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
    pub average_speed: f64, // MB/s
    pub failed_message_ids: Vec<String>, // Track failed segments for potential retry
}

/// Result of downloading a single segment
struct SegmentResult {
    segment_number: u32,
    data: Option<Bytes>,
    message_id: String, // Track for error reporting
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

        // Sort files by size (largest first) to maximize initial throughput
        let mut sorted_files: Vec<&NzbFile> = files.iter().copied().collect();
        sorted_files.sort_by_key(|f| std::cmp::Reverse(f.segments.segment.len()));

        let download_futures = sorted_files.iter().map(|file| {
            let pool = self.pool.clone();
            let config = config.clone();
            let file = (*file).clone();
            let progress = progress_bar.clone();
            let completed = completed_count.clone();

            async move {
                let result = Self::download_file_with_pool(
                    file,
                    config,
                    pool,
                    progress.clone(),
                )
                .await;

                // Update file counter (only update every 5 files to reduce overhead)
                let count = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if count % 5 == 0 || count == total_files {
                    progress.set_message(format!("({}/{})", count, total_files));
                }

                result
            }
        });

        // Process downloads with maximum concurrency for aggressive performance
        // Download all files in parallel - no artificial file-level throttling
        // The segment semaphore and connection pool provide the real limits
        let results: Vec<Result<DownloadResult>> = stream::iter(download_futures)
            .buffer_unordered(files.len())
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
        config: Config,
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
                    tracing::info!("File already complete, skipping: {}", filename);
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

        // Create output file with async I/O
        let output_file = File::create(&output_path).await?;
        let mut writer = BufWriter::with_capacity(config.memory.io_buffer_size, output_file);

        // Prepare segment downloads using pipelining
        let group = &file.groups.group[0].name; // Use first group

        // Create segment requests
        let mut segment_requests: Vec<SegmentRequest> = file.segments.segment
            .iter()
            .map(|segment| SegmentRequest {
                message_id: segment.message_id.clone(),
                group: group.clone(),
                segment_number: segment.number,
            })
            .collect();

        // Pipeline size: how many segments to request per connection
        // Aggressive pipelining: 50 segments per connection for maximum throughput
        let pipeline_size = 50;

        // Split into batches for pipelining
        let num_connections = config.usenet.connections as usize;
        let batches: Vec<Vec<SegmentRequest>> = segment_requests
            .chunks(pipeline_size)
            .map(|chunk| chunk.to_vec())
            .collect();

        // Download batches in parallel using connection pool
        let batch_futures = batches.into_iter().map(|batch| {
            let pool = pool.clone();
            let progress = progress_bar.clone();
            let segment_bytes: Vec<u64> = file.segments.segment.iter().map(|s| s.bytes).collect();

            async move {
                // Get connection from pool with retry logic
                let mut conn = None;
                for attempt in 0..2 {
                    if attempt > 0 {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    match tokio::time::timeout(Duration::from_secs(60), pool.get_connection()).await {
                        Ok(Ok(c)) => {
                            conn = Some(c);
                            break;
                        }
                        _ if attempt == 1 => {
                            tracing::error!("Failed to get connection from pool after retry");
                            return batch.iter().map(|req| (req.segment_number, None)).collect();
                        }
                        _ => continue,
                    }
                }
                let mut conn = conn.expect("connection should be set");


                // Download pipelined batch
                match conn.download_segments_pipelined(&batch).await {
                    Ok(results) => {
                        // Update progress for all segments
                        for (seg_num, _) in &results {
                            if let Some(idx) = (*seg_num as usize).checked_sub(1) {
                                if idx < segment_bytes.len() {
                                    progress.inc(segment_bytes[idx]);
                                }
                            }
                        }
                        results
                    }
                    Err(_) => {
                        // Failed - update progress anyway
                        for req in &batch {
                            if let Some(idx) = (req.segment_number as usize).checked_sub(1) {
                                if idx < segment_bytes.len() {
                                    progress.inc(segment_bytes[idx]);
                                }
                            }
                        }
                        Vec::new()
                    }
                }
            }
        });

        // Execute batches matching connection pool size exactly
        // This prevents timeout errors from queuing too many requests
        let batch_results: Vec<Vec<(u32, Option<Bytes>)>> = stream::iter(batch_futures)
            .buffer_unordered(num_connections)
            .collect()
            .await;

        // Flatten results into segment_results format
        let segment_results: Vec<Result<SegmentResult>> = batch_results
            .into_iter()
            .flatten()
            .map(|(segment_number, data)| {
                let message_id = file.segments.segment
                    .iter()
                    .find(|s| s.number == segment_number)
                    .map(|s| s.message_id.clone())
                    .unwrap_or_default();

                Ok(SegmentResult {
                    segment_number,
                    data,
                    message_id,
                })
            })
            .collect();

        // Process results and write to file
        // Pre-allocate Vec for segment data (faster than HashMap)
        let total_segments = file.segments.segment.len();
        let mut segment_data: Vec<Option<Bytes>> = vec![None; total_segments];
        let mut segments_downloaded = 0;
        let mut segments_failed = 0;
        let mut actual_size = 0u64;
        let mut failed_message_ids = Vec::new();

        for result in segment_results {
            match result {
                Ok(segment_result) => {
                    if let Some(data) = segment_result.data {
                        segments_downloaded += 1;
                        actual_size += data.len() as u64;
                        // Segments are 1-indexed, Vec is 0-indexed
                        let index = segment_result.segment_number.saturating_sub(1) as usize;
                        if index < total_segments {
                            segment_data[index] = Some(data);
                        } else {
                            tracing::warn!("Invalid segment number: {} (expected 1-{})",
                                segment_result.segment_number, total_segments);
                        }
                    } else {
                        segments_failed += 1;
                        failed_message_ids.push(segment_result.message_id);
                    }
                }
                Err(_) => segments_failed += 1,
            }
        }

        // Write segments in order (Vec iteration is faster than HashMap lookups)
        for data in segment_data.into_iter().flatten() {
            writer.write_all(&data).await?;
        }

        // Ensure all data is written
        writer.flush().await?;
        writer.shutdown().await?;

        let download_time = start_time.elapsed();
        let average_speed = if download_time.as_secs() > 0 {
            (actual_size as f64 / 1024.0 / 1024.0) / download_time.as_secs_f64()
        } else {
            0.0
        };

        Ok(DownloadResult {
            filename,
            path: output_path,
            size: actual_size,
            segments_downloaded,
            segments_failed,
            download_time,
            average_speed,
            failed_message_ids,
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
                        tracing::info!("Cleaned up partial file: {}", result.path.display());
                        cleaned_count += 1;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to clean up {}: {}", result.path.display(), e);
                    }
                }
            }
        }

        Ok(cleaned_count)
    }
}
