use human_bytes::human_bytes;
use std::error::Error;
use tracing_subscriber::EnvFilter;

use dl_nzb::{
    cli::{Cli, Commands},
    config::Config,
    download::{Downloader, Nzb},
    error::{ConfigError, DlNzbError},
    json_output::{
        DownloadFileResult, DownloadSummary, ErrorOutput, FileInfo, NzbInfo, PostProcessingResult,
        TestResult,
    },
    nntp::AsyncNntpConnection,
    processing::PostProcessor,
    serde_json,
};

type Result<T> = std::result::Result<T, DlNzbError>;

#[tokio::main]
async fn main() {
    let cli = Cli::parse_and_validate();

    // Store JSON flag before moving cli
    let use_json = cli.json;

    // Run the actual main logic and handle errors appropriately
    if let Err(e) = run(cli).await {
        if use_json {
            let error_output = ErrorOutput::from_error(&e);
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&error_output)
                    .unwrap_or_else(|_| { format!(r#"{{"error": "Failed to serialize error"}}"#) })
            );
        } else {
            eprintln!("Error: {}", e);
            let mut source = e.source();
            while let Some(err) = source {
                eprintln!("  Caused by: {}", err);
                source = err.source();
            }
        }
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    // Initialize logging
    init_logging(&cli)?;

    // Handle special commands first
    if let Some(command) = &cli.command {
        return handle_command(command, &cli).await;
    }

    // Load configuration (auto-creates if it doesn't exist)
    let mut config = Config::load()?;

    // Apply CLI overrides
    config.apply_overrides(cli.get_config_overrides());

    // Handle deprecated flags for backwards compatibility
    if cli.has_deprecated_flags() {
        eprintln!("Note: Some flags used are deprecated. See --help for current usage.");
    }

    // Handle username/password from CLI
    if let Some(username) = &cli.username {
        config.usenet.username = username.clone();
    }
    if let Some(password) = &cli.password {
        config.usenet.password = password.clone();
    }

    // Validate configuration
    config.validate()?;

    // Handle list mode
    if cli.list {
        return handle_list_mode(&cli).await;
    }

    // Check if we have files to download
    if cli.files.is_empty() {
        eprintln!("No NZB files specified. Use 'dl-nzb --help' for usage information.");
        return Ok(());
    }

    // Download mode
    handle_download_mode(&cli, config).await
}

/// Initialize logging based on CLI arguments
fn init_logging(cli: &Cli) -> Result<()> {
    // Base filter from CLI, but suppress par2-rs logs (they break progress bars)
    let filter = EnvFilter::try_new(cli.get_log_level())
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("par2_rs=off".parse().unwrap());

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false);

    if cli.quiet {
        subscriber.without_time().init();
    } else if let Some(log_file) = &cli.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)?;
        subscriber.with_writer(file).init();
    } else {
        subscriber.init();
    }

    Ok(())
}

/// Handle subcommands
async fn handle_command(command: &Commands, cli: &Cli) -> Result<()> {
    match command {
        Commands::Test => {
            let config = Config::load()?;
            let test_config = config.usenet.clone();

            if cli.json {
                // JSON output mode
                let mut result = TestResult {
                    server: test_config.server.clone(),
                    port: test_config.port,
                    ssl: test_config.ssl,
                    connected: false,
                    authenticated: false,
                    healthy: false,
                    error: None,
                };

                match AsyncNntpConnection::connect(&test_config, None).await {
                    Ok(mut conn) => {
                        result.connected = true;
                        result.authenticated = true;
                        result.healthy = conn.is_healthy().await;
                        let _ = conn.close().await;
                    }
                    Err(e) => {
                        result.error = Some(e.to_string());
                    }
                }

                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                // Human-readable output
                println!("Testing connection to Usenet server...");

                match AsyncNntpConnection::connect(&test_config, None).await {
                    Ok(mut conn) => {
                        println!("✓ Successfully connected to {}", test_config.server);
                        println!("   Authentication: OK");

                        if conn.is_healthy().await {
                            println!("   Server status: Healthy");
                        }

                        let _ = conn.close().await;
                    }
                    Err(e) => {
                        eprintln!("❌ Connection failed: {}", e);
                        return Err(e);
                    }
                }
            }

            Ok(())
        }

        Commands::Config => {
            let config_path = Config::config_path()?;

            println!("Configuration file location:");
            println!("  {}", config_path.display());
            println!();

            if config_path.exists() {
                println!("Current configuration:");
                println!("{}", "─".repeat(60));
                let config = Config::load()?;
                let toml = toml::to_string_pretty(&config).map_err(|e| {
                    ConfigError::ParseError(format!("Failed to serialize config: {}", e))
                })?;
                println!("{}", toml);
                println!("{}", "─".repeat(60));
            } else {
                println!("Configuration file does not exist yet.");
                println!("Run any command to auto-create it with default values.");
            }

            Ok(())
        }

        Commands::Version => {
            println!("dl-nzb {}", env!("CARGO_PKG_VERSION"));
            println!("A fast, lightweight NZB downloader");
            println!();
            println!("Features:");
            println!("  • Parallel segment downloads");
            println!("  • Built-in PAR2 repair");
            println!("  • Automatic RAR extraction");
            println!("  • Resume support");
            println!("  • JSON output for scripting");
            Ok(())
        }
    }
}

/// Handle list mode
async fn handle_list_mode(cli: &Cli) -> Result<()> {
    if cli.json {
        // JSON output mode
        let mut results = Vec::new();

        for nzb_path in &cli.files {
            let nzb = Nzb::from_file(nzb_path)?;

            let files: Vec<FileInfo> = nzb
                .files()
                .iter()
                .map(|file| {
                    let filename = Nzb::get_filename_from_subject(&file.subject)
                        .unwrap_or_else(|| file.subject.clone());
                    let size: u64 = file.segments.segment.iter().map(|s| s.bytes).sum();
                    let is_par2 = filename.to_lowercase().ends_with(".par2");

                    FileInfo {
                        filename,
                        size,
                        segments: file.segments.segment.len(),
                        is_par2,
                    }
                })
                .collect();

            results.push(NzbInfo {
                file: nzb_path.clone(),
                total_files: nzb.files().len(),
                total_size: nzb.total_size(),
                total_segments: nzb.total_segments(),
                files,
            });
        }

        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        // Human-readable output
        for nzb_path in &cli.files {
            println!("\n📄 {}", nzb_path.display());
            println!("{}", "─".repeat(50));

            let nzb = Nzb::from_file(nzb_path)?;

            // Display NZB info
            println!("Total files: {}", nzb.files().len());
            println!("Total size: {}", human_bytes(nzb.total_size() as f64));
            println!("Total segments: {}", nzb.total_segments());

            println!("\nFiles:");
            for file in nzb.files() {
                let filename = Nzb::get_filename_from_subject(&file.subject)
                    .unwrap_or_else(|| file.subject.clone());
                let size: u64 = file.segments.segment.iter().map(|s| s.bytes).sum();
                let file_type = if filename.to_lowercase().ends_with(".par2") {
                    "PAR2"
                } else {
                    "DATA"
                };
                println!(
                    "  [{:4}] {} ({})",
                    file_type,
                    filename,
                    human_bytes(size as f64)
                );
            }
        }
    }

    Ok(())
}

/// Handle download mode
async fn handle_download_mode(cli: &Cli, mut config: Config) -> Result<()> {
    // Validate download-specific configuration (server credentials)
    config.validate_for_download()?;

    // Apply CLI settings to config
    if cli.no_directories {
        config.download.create_subfolders = false;
    }

    if cli.no_par2 {
        config.post_processing.auto_par2_repair = false;
    }

    if cli.no_extract_rar {
        config.post_processing.auto_extract_rar = false;
    }

    if cli.delete_rar_after_extract {
        config.post_processing.delete_rar_after_extract = true;
    }

    if cli.delete_par2 {
        config.post_processing.delete_par2_after_repair = true;
    }

    // Update memory settings (from deprecated flags if present)
    if let Some(memory_mb) = cli.memory_limit {
        config.memory.max_segments_in_memory = (memory_mb * 1024 * 1024) / 100_000;
        // Rough estimate
    }
    if let Some(buffer_kb) = cli.buffer_size {
        config.memory.io_buffer_size = buffer_kb * 1024;
    }
    if let Some(concurrent) = cli.max_concurrent_files {
        config.memory.max_concurrent_files = concurrent;
    }

    // Create downloader with spinner (unless JSON output)
    let downloader = if cli.json {
        Downloader::new(config.clone()).await?
    } else {
        use indicatif::{ProgressBar, ProgressStyle};
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
        );
        spinner.enable_steady_tick(std::time::Duration::from_millis(80));
        spinner.set_message("Connecting to server...");

        let downloader = Downloader::new(config.clone()).await?;

        spinner.finish_and_clear();
        downloader
    };

    // Process each NZB file
    let mut all_results = Vec::new();

    for nzb_path in &cli.files {
        let nzb = match Nzb::from_file(nzb_path) {
            Ok(nzb) => nzb,
            Err(e) => {
                eprintln!("Failed to load {}: {}", nzb_path.display(), e);
                continue;
            }
        };

        // Pre-flight availability check
        if !cli.json {
            match downloader.check_availability(&nzb).await {
                Ok((available, _missing, total)) if total > 0 => {
                    let pct = (available as f64 / total as f64) * 100.0;
                    if available == 0 {
                        eprintln!("\x1b[31m✗ No articles found. Content may have expired.\x1b[0m");
                        continue;
                    } else if pct < 100.0 {
                        eprintln!(
                            "\x1b[33m⚠ Warning: Only {:.0}% of articles available. Download may be incomplete.\x1b[0m",
                            pct
                        );
                    }
                }
                Err(e) => {
                    tracing::debug!("Availability check failed: {}", e);
                }
                _ => {}
            }
        }

        // Create output directory based on NZB filename
        let output_dir = if config.download.create_subfolders {
            // Use NZB filename (without extension) as folder name
            let folder_name = nzb_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("download")
                .to_string();
            config.download.dir.join(folder_name)
        } else {
            config.download.dir.clone()
        };

        std::fs::create_dir_all(&output_dir)?;

        // Update config for this download
        let mut download_config = config.clone();
        download_config.download.dir = output_dir.clone();
        download_config.download.force_redownload = cli.force;

        // Track timing for JSON output
        let download_start = std::time::Instant::now();

        // Download the NZB with updated config
        match downloader.download_nzb(&nzb, download_config.clone()).await {
            Ok((results, _progress_bar)) => {
                let download_time = download_start.elapsed();

                if cli.print_names {
                    for result in &results {
                        println!("{}", result.path.display());
                    }
                }

                // Post-processing
                let mut post_result = PostProcessingResult {
                    par2_verified: false,
                    par2_repaired: false,
                    rar_extracted: false,
                    files_renamed: 0,
                };

                if config.post_processing.auto_par2_repair
                    || config.post_processing.auto_extract_rar
                {
                    let processor = PostProcessor::new(
                        download_config.post_processing.clone(),
                        download_config.tuning.large_file_threshold,
                    );
                    if let Err(e) = processor.process_downloads(&results).await {
                        if !cli.json {
                            eprintln!("Post-processing error: {}", e);
                        }
                    } else {
                        post_result.par2_verified = config.post_processing.auto_par2_repair;
                        post_result.rar_extracted = config.post_processing.auto_extract_rar;
                    }
                }

                // Output results
                if cli.json {
                    let total_size: u64 = results.iter().map(|r| r.size).sum();
                    let summary = DownloadSummary {
                        nzb: nzb_path.clone(),
                        output_dir: output_dir.clone(),
                        success: results.iter().all(|r| r.segments_failed == 0),
                        total_size,
                        download_time_seconds: download_time.as_secs_f64(),
                        average_speed_mbps: if download_time.as_secs() > 0 {
                            (total_size as f64 / 1024.0 / 1024.0) / download_time.as_secs_f64()
                        } else {
                            0.0
                        },
                        files: results
                            .iter()
                            .map(|r| DownloadFileResult {
                                filename: r.filename.clone(),
                                path: r.path.clone(),
                                size: r.size,
                                segments_downloaded: r.segments_downloaded,
                                segments_failed: r.segments_failed,
                                success: r.segments_failed == 0,
                            })
                            .collect(),
                        post_processing: post_result,
                    };
                    println!("{}", serde_json::to_string_pretty(&summary)?);
                } else {
                    print_final_summary(&nzb, &results, &output_dir);
                }

                all_results.extend(results);
            }
            Err(e) => {
                if cli.json {
                    let error_output = ErrorOutput::from_error(&e);
                    println!("{}", serde_json::to_string_pretty(&error_output)?);
                } else {
                    eprintln!("Download failed for {}: {}", nzb_path.display(), e);
                    if !cli.keep_partial {
                        eprintln!("Note: Partial files may remain. Use --keep-partial to explicitly keep them.");
                    }
                }
            }
        }
    }

    // Terminal bell to notify completion (skip in quiet/json mode)
    if !cli.quiet && !cli.json {
        print!("\x07");
    }

    Ok(())
}

/// Print a final summary after all processing is complete
fn print_final_summary(
    _nzb: &Nzb,
    results: &[dl_nzb::download::DownloadResult],
    output_dir: &std::path::Path,
) {
    use std::time::Duration;

    // Calculate total stats
    let total_size: u64 = results.iter().map(|r| r.size).sum();
    let total_time: Duration = results.iter().map(|r| r.download_time).sum();
    let failed_count = results.iter().filter(|r| r.segments_failed > 0).count();

    // Find the main video/media file (largest non-PAR2, non-RAR file)
    let main_file = std::fs::read_dir(output_dir).ok().and_then(|entries| {
        entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_lowercase();
                !name.ends_with(".par2")
                    && !name.ends_with(".rar")
                    && !name.ends_with(".nfo")
                    && !name.ends_with(".sfv")
            })
            .max_by_key(|e| e.metadata().ok().map(|m| m.len()).unwrap_or(0))
    });

    println!();

    if failed_count == 0 {
        if let Some(file) = main_file {
            let filename = file.file_name().to_string_lossy().to_string();
            let file_size = file.metadata().ok().map(|m| m.len()).unwrap_or(0);

            println!("\x1b[1;32m✓ Complete:\x1b[0m \x1b[37m{}\x1b[0m", filename);
            println!(
                "  \x1b[90m└─\x1b[0m \x1b[34m{}\x1b[0m",
                output_dir.display()
            );
            println!(
                "  \x1b[90m└─\x1b[0m \x1b[36m{}\x1b[0m in \x1b[35m{:.0}s\x1b[0m",
                human_bytes(file_size as f64),
                total_time.as_secs_f64()
            );
        } else {
            // No main file found, just show stats
            println!("\x1b[1;32m✓ Complete\x1b[0m");
            println!(
                "  \x1b[90m└─\x1b[0m \x1b[34m{}\x1b[0m",
                output_dir.display()
            );
            println!(
                "  \x1b[90m└─\x1b[0m \x1b[36m{}\x1b[0m in \x1b[35m{:.0}s\x1b[0m",
                human_bytes(total_size as f64),
                total_time.as_secs_f64()
            );
        }
    } else {
        println!(
            "\x1b[1;33m! Completed with {} file{} having errors\x1b[0m",
            failed_count,
            if failed_count == 1 { "" } else { "s" }
        );
        println!(
            "  \x1b[90m└─\x1b[0m \x1b[34m{}\x1b[0m",
            output_dir.display()
        );
    }
}
