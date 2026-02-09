use human_bytes::human_bytes;
use std::error::Error;
use tracing_subscriber::EnvFilter;

use dl_nzb::{
    cli::{Cli, Commands},
    config::Config,
    download::{Downloader, Nzb},
    error::DlNzbError,
    json_output::{
        DownloadFileResult, DownloadSummary, ErrorOutput, FileInfo, NzbInfo, PostProcessingResult,
        TestResult,
    },
    nntp::AsyncNntpConnection,
    processing::{Par2Status, PostProcessingOutcome, PostProcessor},
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
                    .unwrap_or_else(|_| r#"{"error": "Failed to serialize error"}"#.to_string())
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

    // Handle Ctrl+C gracefully
    tokio::spawn(async {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("\nInterrupted.");
        std::process::exit(130); // Standard Ctrl+C exit code
    });

    // Handle special commands first
    if let Some(command) = &cli.command {
        return handle_command(command, &cli).await;
    }

    // Load configuration (auto-creates if it doesn't exist)
    let mut config = Config::load()?;

    // Apply CLI overrides
    config.apply_overrides(cli.get_config_overrides());

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
                let mut config = Config::load()?;
                // Redact password before display
                if !config.usenet.password.is_empty() {
                    config.usenet.password = "********".to_string();
                }
                let toml = config.display_toml()?;
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
                let display_name = sanitize_display(&filename);
                let size: u64 = file.segments.segment.iter().map(|s| s.bytes).sum();
                let file_type = if filename.to_lowercase().ends_with(".par2") {
                    "PAR2"
                } else {
                    "DATA"
                };
                println!(
                    "  [{:4}] {} ({})",
                    file_type,
                    display_name,
                    human_bytes(size as f64)
                );
            }
        }
    }

    Ok(())
}

/// Create a styled spinner with the given message
fn create_spinner(msg: &str) -> indicatif::ProgressBar {
    use indicatif::{ProgressBar, ProgressStyle};
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));
    spinner.set_message(msg.to_string());
    spinner
}

/// Handle download mode
async fn handle_download_mode(cli: &Cli, config: Config) -> Result<()> {
    // Validate download-specific configuration (server credentials)
    config.validate_for_download()?;

    // Create downloader with spinner (unless JSON output)
    let downloader = if cli.json {
        Downloader::new(config.clone()).await?
    } else {
        let spinner = create_spinner("Connecting to server...");

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

        // Quick availability check (unless JSON mode or quiet)
        let mut skip_message_ids: Option<std::collections::HashSet<String>> = None;
        if !cli.json && !cli.quiet {
            let spinner = create_spinner("Checking article availability...");

            match downloader.check_availability(&nzb).await {
                Ok((available, missing, total, missing_ids)) => {
                    spinner.finish_and_clear();

                    if !missing_ids.is_empty() {
                        skip_message_ids = Some(missing_ids.clone());
                    }

                    if missing > 0 {
                        let percent = (available as f64 / total as f64) * 100.0;

                        // Check which files are missing by looking up the missing message IDs
                        let missing_files: Vec<String> = nzb
                            .files()
                            .iter()
                            .filter(|f| {
                                f.segments
                                    .segment
                                    .first()
                                    .map(|s| missing_ids.contains(&s.message_id))
                                    .unwrap_or(false)
                            })
                            .filter_map(|f| Nzb::get_filename_from_subject(&f.subject))
                            .collect();
                        let missing_display: Vec<String> =
                            missing_files.iter().map(|n| sanitize_display(n)).collect();

                        // Check if only non-essential files are missing (.nfo, .sfv, .srr)
                        let only_nonessential = missing_files.iter().all(|name| {
                            let lower = name.to_lowercase();
                            lower.ends_with(".nfo")
                                || lower.ends_with(".sfv")
                                || lower.ends_with(".srr")
                        });

                        // Check if PAR2 files are available
                        let has_par2 = nzb.files().iter().any(|f| {
                            Nzb::get_filename_from_subject(&f.subject)
                                .map(|n| n.to_lowercase().ends_with(".par2"))
                                .unwrap_or(false)
                        });

                        // PAR2 typically provides 10% redundancy
                        let missing_percent = 100.0 - percent;
                        let can_likely_repair = has_par2 && missing_percent <= 10.0;

                        if only_nonessential {
                            // Just info, non-essential files missing
                            println!(
                                "\x1b[90mℹ {:.0}% available ({} missing: {})\x1b[0m",
                                percent,
                                missing,
                                missing_display.join(", ")
                            );
                            // Continue without prompting
                        } else if can_likely_repair {
                            println!(
                                "\x1b[33m⚠ {:.0}% available ({} of {} files). PAR2 repair likely.\x1b[0m",
                                percent, available, total
                            );
                            // Continue without prompting
                        } else {
                            // Significant missing files - prompt user
                            if has_par2 {
                                println!(
                                    "\x1b[31m✗ Only {:.0}% available ({} of {} files). PAR2 repair unlikely.\x1b[0m",
                                    percent, available, total
                                );
                            } else {
                                println!(
                                    "\x1b[31m✗ Only {:.0}% available ({} of {} files). No PAR2 for repair.\x1b[0m",
                                    percent, available, total
                                );
                            }

                            // Prompt user
                            eprint!("  Continue anyway? [y/N] ");
                            use std::io::{self, BufRead, Write};
                            io::stderr().flush().ok();

                            let stdin = io::stdin();
                            let response = stdin.lock().lines().next();
                            match response {
                                Some(Ok(line)) => {
                                    let answer = line.trim().to_lowercase();
                                    if answer != "y" && answer != "yes" {
                                        println!("  Aborted.");
                                        continue; // Skip to next NZB
                                    }
                                }
                                _ => {
                                    println!("  Aborted.");
                                    continue;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    spinner.finish_and_clear();
                    eprintln!("Warning: Could not check availability: {}", e);
                    // Continue with download anyway
                }
            }
        }

        // Download the NZB - handle 430s inline
        match downloader
            .download_nzb(&nzb, download_config.clone(), skip_message_ids.as_ref())
            .await
        {
            Ok((results, _progress_bar)) => {
                let download_time = download_start.elapsed();

                // Post-processing
                let mut post_result = PostProcessingResult {
                    par2_verified: false,
                    par2_repaired: false,
                    rar_extracted: false,
                    files_renamed: 0,
                };
                let mut post_outcome: Option<PostProcessingOutcome> = None;
                let mut post_failed = false;

                if config.post_processing.auto_par2_repair
                    || config.post_processing.auto_extract_rar
                {
                    let processor = PostProcessor::new(
                        download_config.post_processing.clone(),
                        download_config.tuning.large_file_threshold,
                    );
                    match processor.process_downloads(&results).await {
                        Ok(outcome) => {
                            post_result.par2_verified = outcome.par2_status == Par2Status::Success;
                            post_result.par2_repaired = post_result.par2_verified;
                            post_result.rar_extracted = outcome.rar_extracted;
                            post_result.files_renamed = outcome.files_renamed;
                            post_outcome = Some(outcome);
                        }
                        Err(e) => {
                            post_failed = true;
                            if !cli.json {
                                eprintln!("Post-processing error: {}", e);
                            }
                        }
                    }
                }

                let download_ok = results.iter().all(|r| r.segments_failed == 0);
                let post_ok = if post_failed {
                    false
                } else if let Some(outcome) = post_outcome.as_ref() {
                    outcome.par2_status != Par2Status::Failed
                } else {
                    true
                };

                let success = download_ok && post_ok;

                // Output results
                if cli.json {
                    let total_size: u64 = results.iter().map(|r| r.size).sum();
                    let summary = DownloadSummary {
                        nzb: nzb_path.clone(),
                        output_dir: output_dir.clone(),
                        success,
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
                    print_final_summary(
                        &nzb,
                        &results,
                        &output_dir,
                        post_outcome.as_ref(),
                        post_failed,
                    );
                }

                all_results.extend(results);
            }
            Err(e) => {
                if cli.json {
                    let error_output = ErrorOutput::from_error(&e);
                    println!("{}", serde_json::to_string_pretty(&error_output)?);
                } else {
                    eprintln!("Download failed for {}: {}", nzb_path.display(), e);
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
    post_outcome: Option<&PostProcessingOutcome>,
    post_failed: bool,
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

    let mut post_issue = false;
    let mut post_issue_msg = None;
    if post_failed {
        post_issue = true;
        post_issue_msg = Some("Post-processing failed");
    } else if let Some(outcome) = post_outcome {
        if outcome.par2_status == Par2Status::Failed {
            post_issue = true;
            post_issue_msg = Some("PAR2 verification failed");
        }
    }

    if failed_count == 0 && !post_issue {
        if let Some(file) = main_file {
            let filename = file.file_name().to_string_lossy().to_string();
            let display_name = sanitize_display(&filename);
            let file_size = file.metadata().ok().map(|m| m.len()).unwrap_or(0);

            println!(
                "\x1b[1;32m✓ Complete:\x1b[0m \x1b[37m{}\x1b[0m",
                display_name
            );
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
    } else if failed_count == 0 {
        let issue = post_issue_msg.unwrap_or("Post-processing issues");
        println!(
            "\x1b[1;33m⚠ Completed with issues:\x1b[0m \x1b[37m{}\x1b[0m",
            issue
        );
        println!(
            "  \x1b[90m└─\x1b[0m \x1b[34m{}\x1b[0m",
            output_dir.display()
        );
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

fn sanitize_display(input: &str) -> String {
    input.chars().filter(|c| !c.is_ascii_control()).collect()
}
