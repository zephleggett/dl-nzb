use bytes::Bytes;
use futures::stream::{self, StreamExt};
use indicatif::ProgressBar;
use std::path::PathBuf;
use std::sync::Arc; // Only for Semaphore
use std::time::{Duration, Instant};
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::Semaphore;

use super::nzb::{Nzb, NzbFile};
use crate::config::Config;
use crate::error::{DlNzbError, DownloadError};
use crate::nntp::{NntpPool, NntpPoolBuilder, NntpPoolExt};
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
    segment_semaphore: Arc<Semaphore>,
}

impl Downloader {
    /// Create a new downloader with connection pool
    pub async fn new(config: Config) -> Result<Self> {
        let pool = NntpPoolBuilder::new(config.usenet.clone())
            .max_size(config.usenet.connections as usize)
            .build()?;

        // Create semaphore to enforce max_segments_in_memory limit
        // This prevents memory exhaustion on large downloads
        let segment_semaphore = Arc::new(Semaphore::new(config.memory.max_segments_in_memory));

        Ok(Self {
            pool,
            segment_semaphore,
        })
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

        let download_futures = files.iter().map(|file| {
            let pool = self.pool.clone();
            let semaphore = self.segment_semaphore.clone();
            let config = config.clone();
            let file = (*file).clone();
            let progress = progress_bar.clone();
            let completed = completed_count.clone();

            async move {
                let result = Self::download_file_with_pool(
                    file,
                    config,
                    pool,
                    semaphore,
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

        // Process downloads with controlled concurrency
        // Use config value for file-level parallelism to better utilize connection pool
        let file_concurrency = config.memory.max_concurrent_files.min(files.len());
        let results: Vec<Result<DownloadResult>> = stream::iter(download_futures)
            .buffer_unordered(file_concurrency)
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
        segment_semaphore: Arc<Semaphore>,
        progress_bar: ProgressBar,
    ) -> Result<DownloadResult> {
        let filename = Nzb::get_filename_from_subject(&file.subject)
            .unwrap_or_else(|| format!("unknown_file_{}", file.date));

        let output_path = config.download.dir.join(&filename);
        let start_time = Instant::now();

        // Create output file with async I/O
        let output_file = File::create(&output_path).await?;
        let mut writer = BufWriter::with_capacity(config.memory.io_buffer_size, output_file);

        // Prepare segment downloads
        let group = &file.groups.group[0].name; // Use first group
        let segment_futures = file.segments.segment.iter().map(|segment| {
            let pool = pool.clone();
            let semaphore = segment_semaphore.clone();
            let message_id = segment.message_id.clone();
            let group = group.clone();
            let segment_number = segment.number;
            let expected_bytes = segment.bytes;
            let progress = progress_bar.clone();

            async move {
                // Acquire semaphore permit BEFORE downloading segment
                // This enforces max_segments_in_memory limit
                let _permit = semaphore.acquire().await.unwrap();

                // Retry up to 3 times
                for attempt in 0..3 {
                    // Get connection from pool with timeout
                    let mut conn =
                        match tokio::time::timeout(Duration::from_secs(30), pool.get_connection())
                            .await
                        {
                            Ok(Ok(conn)) => conn,
                            Ok(Err(_)) | Err(_) => {
                                if attempt == 2 {
                                    // Last attempt failed
                                    progress.inc(expected_bytes);
                                    return Ok(SegmentResult {
                                        segment_number,
                                        data: None,
                                        message_id: message_id.clone(),
                                    });
                                }
                                // Small delay before retry to avoid overwhelming server
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                continue;
                            }
                        };

                    // Download segment with timeout
                    let download_result = tokio::time::timeout(
                        Duration::from_secs(60),
                        conn.download_segment(&message_id, &group),
                    )
                    .await;

                    match download_result {
                        Ok(Ok(data)) => {
                            // Success! Update progress and return
                            progress.inc(expected_bytes);
                            return Ok(SegmentResult {
                                segment_number,
                                data: Some(data),
                                message_id: message_id.clone(),
                            });
                        }
                        Ok(Err(_)) | Err(_) => {
                            // Failed or timed out
                            if attempt == 2 {
                                // Last attempt - give up
                                progress.inc(expected_bytes);
                                return Ok(SegmentResult {
                                    segment_number,
                                    data: None,
                                    message_id: message_id.clone(),
                                });
                            }
                            // Small delay before retry to avoid overwhelming server
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }

                // Shouldn't reach here but just in case
                progress.inc(expected_bytes);
                Ok(SegmentResult {
                    segment_number,
                    data: None,
                    message_id: message_id.clone(),
                })
                // Permit is dropped here, releasing memory slot
            }
        });

        // Download segments with controlled concurrency
        let segment_results: Vec<Result<SegmentResult>> = stream::iter(segment_futures)
            .buffer_unordered(config.usenet.connections as usize)
            .collect()
            .await;

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
                        if segment_result.segment_number > 0
                            && segment_result.segment_number <= total_segments as u32
                        {
                            segment_data[(segment_result.segment_number - 1) as usize] = Some(data);
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
