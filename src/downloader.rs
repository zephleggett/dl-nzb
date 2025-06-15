use anyhow::{anyhow, Result};
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Write, BufWriter};
use std::time::{Instant, Duration};

use crate::config::Config;
use crate::nzb::{Nzb, NzbFile};
use crate::nntp::NntpClient;

#[derive(Clone)]
pub struct Downloader {
    config: Config,
    multi_progress: MultiProgress,
}

#[derive(Debug)]
pub struct DownloadResult {
    pub filename: String,
    pub path: PathBuf,
    pub size: u64,
    pub segments_downloaded: usize,
    pub segments_failed: usize,
    pub download_time: Duration,
    pub average_speed: f64, // MB/s
}

#[derive(Debug)]
struct SegmentResult {
    segment_number: u32,
    data: Vec<u8>,
    error: Option<String>,
}

impl Downloader {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            multi_progress: MultiProgress::new(),
        }
    }

    pub async fn download_nzb(&self, nzb: &Nzb) -> Result<Vec<DownloadResult>> {
        self.config.ensure_dirs()?;

        let main_files = nzb.get_main_files();
        let par2_files = nzb.get_par2_files();

        if main_files.is_empty() {
            return Err(anyhow!("No downloadable files found in NZB"));
        }

        println!("Found {} main files to download", main_files.len());
        if !par2_files.is_empty() {
            println!("Found {} PAR2 recovery files (will download if needed)", par2_files.len());
        }
        println!("Total size: {:.2} MB", nzb.total_size() as f64 / 1024.0 / 1024.0);
        println!("Total segments: {}", nzb.total_segments());

        // Create a shared semaphore for all downloads to respect connection limit
        let global_semaphore = Arc::new(Semaphore::new(self.config.usenet.connections as usize));

        // Create a single progress bar for all downloads (initially just main files)
        let main_bytes: u64 = main_files.iter()
            .flat_map(|f| &f.segments.segment)
            .map(|s| s.bytes)
            .sum();

        let progress_bar = self.multi_progress.add(ProgressBar::new(main_bytes));
        progress_bar.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({percent}%) | {bytes_per_sec} | {eta} | {msg}"
            )?
            .progress_chars("█▉▊▋▌▍▎▏  ")
        );

        let file_count = main_files.len();
        progress_bar.set_message(format!("Downloading {} main files", file_count));

        let mut results = Vec::new();

        // Download all main files concurrently
        let mut file_handles = Vec::new();

        for file in main_files {
            let config = self.config.clone();
            let file_clone = file.clone();
            let semaphore = global_semaphore.clone();
            let shared_progress = progress_bar.clone();

            let handle = tokio::spawn(async move {
                Self::download_file_with_shared_progress(&file_clone, config, semaphore, shared_progress).await
            });

            file_handles.push(handle);
        }

        // Wait for all downloads to complete
        let mut error_count = 0;
        for handle in file_handles {
            match handle.await {
                Ok(Ok(result)) => results.push(result),
                Ok(Err(_e)) => {
                    error_count += 1;
                    // Don't print errors to avoid interrupting progress bar
                }
                Err(_e) => {
                    error_count += 1;
                    // Don't print errors to avoid interrupting progress bar
                }
            }
        }

        // Finish the progress bar
        let total_downloaded: u64 = results.iter().map(|r| r.size).sum();
        let total_time: f64 = results.iter()
            .map(|r| r.download_time.as_secs_f64())
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0);
        let avg_speed = if total_time > 0.0 {
            (total_downloaded as f64 / 1024.0 / 1024.0) / total_time
        } else {
            0.0
        };

        progress_bar.finish_with_message(format!("Completed {} files ({:.2} MB/s avg)", file_count, avg_speed));

        // Show completion summary right after progress bar
        println!("\n🎉 Download Complete!");
        println!("📁 {} files downloaded successfully", file_count);
        if error_count > 0 {
            println!("⚠️  {} files failed to download", error_count);
        }
        println!("📊 Total: {:.2} MB in {:.1}s ({:.2} MB/s avg)",
                total_downloaded as f64 / 1024.0 / 1024.0, total_time, avg_speed);

        let failed_files = results.iter().filter(|r| r.segments_failed > 0).count();
        if failed_files == 0 {
            println!("✅ All files completed without errors");
        } else {
            println!("⚠️  {} files had failed segments", failed_files);
        }

        // Check if we need PAR2 files (if any main files had failed segments)
        let total_failed_segments: usize = results.iter().map(|r| r.segments_failed).sum();

        if total_failed_segments > 0 && !par2_files.is_empty() {
            println!("\n⚠️  {} segments failed, downloading all PAR2 recovery files...", total_failed_segments);

            // Extend progress bar to include PAR2 files
            let par2_bytes: u64 = par2_files.iter()
                .flat_map(|f| &f.segments.segment)
                .map(|s| s.bytes)
                .sum();
            progress_bar.inc_length(par2_bytes);
            progress_bar.set_message(format!("Downloading {} main + {} PAR2 files", file_count, par2_files.len()));

            // Download all PAR2 files for complete recovery capability
            let mut par2_handles = Vec::new();

            for par2_file in par2_files {
                let config = self.config.clone();
                let file_clone = par2_file.clone();
                let semaphore = global_semaphore.clone();
                let shared_progress = progress_bar.clone();

                let handle = tokio::spawn(async move {
                    Self::download_file_with_shared_progress(&file_clone, config, semaphore, shared_progress).await
                });

                par2_handles.push(handle);
            }

            // Wait for all PAR2 downloads to complete
            for handle in par2_handles {
                match handle.await {
                    Ok(Ok(result)) => {
                        results.push(result);
                        // Don't print individual PAR2 completion to avoid interrupting progress
                    }
                    Ok(Err(_e)) => {
                        // Don't print errors to avoid interrupting progress bar
                    }
                    Err(_e) => {
                        // Don't print errors to avoid interrupting progress bar
                    }
                }
            }

            println!("💡 All PAR2 files downloaded. Use external PAR2 tools to verify and repair if needed.");
        } else if total_failed_segments == 0 {
            println!("✅ All segments downloaded successfully, no PAR2 files needed!");
        }

        Ok(results)
    }

    async fn download_file_with_shared_progress(
        file: &NzbFile,
        config: Config,
        global_semaphore: Arc<Semaphore>,
        shared_progress: ProgressBar,
    ) -> Result<DownloadResult> {
        let filename = Nzb::get_filename_from_subject(&file.subject)
            .unwrap_or_else(|| format!("unknown_file_{}", file.date));

        let output_path = config.download_dir.join(&filename);
        let start_time = Instant::now();

        // Create output file with buffered writer for better I/O performance
        let output_file = File::create(&output_path)?;
        let mut writer = BufWriter::with_capacity(config.memory.io_buffer_size, output_file);

        // Pre-allocate space for segments if not streaming to disk
        let mut segment_data: HashMap<u32, Vec<u8>> = if config.memory.stream_to_disk {
            HashMap::new()
        } else {
            HashMap::with_capacity(file.segments.segment.len())
        };

        // Download segments concurrently using the shared semaphore
        let mut handles: Vec<JoinHandle<SegmentResult>> = Vec::new();
        let group = &file.groups.group[0].name; // Use first group

        for segment in &file.segments.segment {
            let semaphore = global_semaphore.clone();
            let config_clone = config.clone();
            let message_id = segment.message_id.clone();
            let group = group.clone();
            let segment_number = segment.number;
            let progress_bar = shared_progress.clone();

            let handle = tokio::spawn(async move {
                let _permit = semaphore.acquire().await.unwrap();

                let result = tokio::task::spawn_blocking(move || {
                    let mut client = NntpClient::connect(config_clone.usenet)?;
                    let data = client.download_segment(&message_id, &group)?;
                    client.quit().ok(); // Ignore quit errors
                    Ok::<Vec<u8>, anyhow::Error>(data)
                }).await;

                match result {
                    Ok(Ok(data)) => {
                        progress_bar.inc(data.len() as u64);
                        SegmentResult {
                            segment_number,
                            data,
                            error: None,
                        }
                    }
                    Ok(Err(e)) => {
                        SegmentResult {
                            segment_number,
                            data: Vec::new(),
                            error: Some(e.to_string()),
                        }
                    }
                    Err(e) => {
                        SegmentResult {
                            segment_number,
                            data: Vec::new(),
                            error: Some(e.to_string()),
                        }
                    }
                }
            });

            handles.push(handle);
        }

        // Collect results
        let mut segments_downloaded = 0;
        let mut segments_failed = 0;
        let mut actual_size = 0u64;

        for handle in handles {
            match handle.await {
                Ok(segment_result) => {
                    if segment_result.error.is_none() && !segment_result.data.is_empty() {
                        segments_downloaded += 1;
                        actual_size += segment_result.data.len() as u64;

                        if config.memory.stream_to_disk {
                            // Stream directly to disk to save memory
                            // For now, we still need to collect segments to write in order
                            // A future optimization could write segments as they arrive
                            segment_data.insert(segment_result.segment_number, segment_result.data);
                        } else {
                            segment_data.insert(segment_result.segment_number, segment_result.data);
                        }
                    } else {
                        segments_failed += 1;
                        // Don't print individual segment errors to avoid interrupting progress bar
                    }
                }
                Err(_e) => {
                    segments_failed += 1;
                    // Don't print task errors to avoid interrupting progress bar
                }
            }
        }

        // Write segments in order to output file
        for i in 1..=file.segments.segment.len() as u32 {
            if let Some(data) = segment_data.get(&i) {
                writer.write_all(data)?;
            }
        }

        // Ensure all data is written to disk
        writer.flush()?;
        drop(writer); // Close the file

        let download_time = start_time.elapsed();
        let average_speed = if download_time.as_secs() > 0 {
            (actual_size as f64 / 1024.0 / 1024.0) / download_time.as_secs_f64()
        } else {
            0.0
        };

        // Don't print completion message here to avoid interrupting progress bar

        Ok(DownloadResult {
            filename,
            path: output_path,
            size: actual_size,
            segments_downloaded,
            segments_failed,
            download_time,
            average_speed,
        })
    }
}
