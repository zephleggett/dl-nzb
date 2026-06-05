use human_bytes::human_bytes;
use std::error::Error;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
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
    let use_json = cli.json;

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
    init_logging(&cli)?;

    // Two-stage Ctrl+C:
    //   1st: request graceful shutdown — workers stop accepting new jobs, the
    //        writer finalizes in-flight writes, and post-processing is skipped.
    //   2nd: force an immediate exit (130).
    // A 10-second backstop hard-exits if the graceful drain hangs.
    spawn_signal_handler();

    // A download holds one open fd per output file (large NZBs have hundreds)
    // plus one per connection, and PAR2 repair opens a handle per recovery file.
    // The inherited soft limit is often only 256 on macOS, which produced
    // "Too many open files" on big releases — raise it (best-effort).
    raise_fd_limit();

    if let Some(command) = &cli.command {
        return handle_command(command, &cli).await;
    }

    let mut config = Config::load()?;
    config.apply_overrides(cli.get_config_overrides());
    config.validate()?;

    if cli.list {
        return handle_list_mode(&cli).await;
    }

    if cli.files.is_empty() {
        eprintln!("No NZB files specified. Use 'dl-nzb --help' for usage information.");
        return Ok(());
    }

    handle_download_mode(&cli, config).await
}

/// Counter for the `test` subcommand, which doesn't care about byte tallies.
fn throwaway_counter() -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(0))
}

/// Best-effort raise of the process open-file (RLIMIT_NOFILE) soft limit toward
/// the hard limit, so downloads with many files don't hit "Too many open files".
#[cfg(unix)]
fn raise_fd_limit() {
    match rlimit::increase_nofile_limit(u64::MAX) {
        Ok(limit) => tracing::debug!("Open-file limit raised to {}", limit),
        Err(e) => tracing::debug!("Could not raise open-file limit: {}", e),
    }
}

#[cfg(not(unix))]
fn raise_fd_limit() {}

/// Spawn the Ctrl+C handler. First interrupt requests a graceful shutdown (and
/// arms a 10s hard-exit backstop); a second interrupt forces an immediate exit.
fn spawn_signal_handler() {
    #[cfg(unix)]
    tokio::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => return,
        };
        sigint.recv().await;
        eprintln!("\nInterrupted; finishing pending writes, skipping post-processing…");
        dl_nzb::shutdown::request();
        // Either a second Ctrl+C or the backstop forces exit.
        tokio::select! {
            _ = sigint.recv() => {
                eprintln!("Force quit.");
                std::process::exit(130);
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                std::process::exit(130);
            }
        }
    });

    #[cfg(not(unix))]
    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_err() {
            return;
        }
        eprintln!("\nInterrupted; finishing pending writes, skipping post-processing…");
        dl_nzb::shutdown::request();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("Force quit.");
                std::process::exit(130);
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                std::process::exit(130);
            }
        }
    });
}

fn init_logging(cli: &Cli) -> Result<()> {
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

async fn handle_command(command: &Commands, cli: &Cli) -> Result<()> {
    match command {
        Commands::Test => {
            let config = Config::load()?;
            let test_config = config.usenet.clone();

            if cli.json {
                let mut result = TestResult {
                    server: test_config.server.clone(),
                    port: test_config.port,
                    ssl: test_config.ssl,
                    connected: false,
                    authenticated: false,
                    healthy: false,
                    error: None,
                };
                match AsyncNntpConnection::connect(&test_config, None, throwaway_counter()).await {
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
                println!("Testing connection to Usenet server...");
                match AsyncNntpConnection::connect(&test_config, None, throwaway_counter()).await {
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
            println!("  • Parallel segment downloads with per-segment retry");
            println!("  • yEnc decoder with =ypart offsets and CRC32 verification");
            println!("  • Built-in PAR2 repair (par2-rs, pure Rust + SIMD)");
            println!("  • Automatic RAR extraction");
            println!("  • JSON output for scripting");
            Ok(())
        }
    }
}

async fn handle_list_mode(cli: &Cli) -> Result<()> {
    if cli.json {
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
                    let is_par2 =
                        dl_nzb::patterns::par2::is_par2_file(std::path::Path::new(&filename));
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
        for nzb_path in &cli.files {
            println!("\n📄 {}", nzb_path.display());
            println!("{}", "─".repeat(50));
            let nzb = Nzb::from_file(nzb_path)?;
            println!("Total files: {}", nzb.files().len());
            println!("Total size: {}", human_bytes(nzb.total_size() as f64));
            println!("Total segments: {}", nzb.total_segments());
            println!("\nFiles:");
            for file in nzb.files() {
                let filename = Nzb::get_filename_from_subject(&file.subject)
                    .unwrap_or_else(|| file.subject.clone());
                let display_name = sanitize_display(&filename);
                let size: u64 = file.segments.segment.iter().map(|s| s.bytes).sum();
                let file_type =
                    if dl_nzb::patterns::par2::is_par2_file(std::path::Path::new(&filename)) {
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

async fn handle_download_mode(cli: &Cli, config: Config) -> Result<()> {
    config.validate_for_download()?;

    // In JSON mode, suppress the decorative human-readable progress lines from
    // the downloader/post-processor so the JSON document is the only stdout
    // content. stderr remains available for warnings.
    if cli.json {
        dl_nzb::output_mode::set_quiet(true);
    }

    let downloader = if cli.json {
        Downloader::new(config.clone()).await?
    } else {
        let spinner = dl_nzb::progress::create_spinner("Connecting to server...");
        let downloader = Downloader::new(config.clone()).await?;
        spinner.finish_and_clear();
        downloader
    };

    for nzb_path in &cli.files {
        // Don't start (or continue to) another NZB after an interrupt.
        if dl_nzb::shutdown::is_requested() {
            break;
        }
        let nzb = match Nzb::from_file(nzb_path) {
            Ok(nzb) => nzb,
            Err(e) => {
                eprintln!("Failed to load {}: {}", nzb_path.display(), e);
                continue;
            }
        };

        let output_dir = if config.download.create_subfolders {
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

        let mut download_config = config.clone();
        download_config.download.dir = output_dir.clone();
        download_config.download.force_redownload = cli.force;

        let download_start = std::time::Instant::now();

        // Pre-flight availability scan: STAT every segment to learn exactly
        // what's missing before committing to the download. Builds the skip set
        // (so missing articles are never fetched) and estimates whether PAR2 can
        // repair the gap. Runs in every mode; only the interactive abort prompt
        // is gated to interactive runs (JSON/quiet report and continue).
        let mut skip_message_ids: Option<std::collections::HashSet<String>> = None;
        {
            let interactive = !cli.json && !cli.quiet;
            let spinner = interactive
                .then(|| dl_nzb::progress::create_spinner("Checking article availability..."));
            match downloader.check_all_availability(&nzb).await {
                Ok(report) => {
                    if let Some(s) = spinner {
                        s.finish_and_clear();
                    }
                    if !report.missing_ids.is_empty() {
                        skip_message_ids = Some(report.missing_ids.clone());
                    }
                    if !report.missing_files.is_empty() {
                        let percent = report.data_completion_percent();
                        let missing_display: Vec<String> = report
                            .missing_files
                            .iter()
                            .map(|n| sanitize_display(n))
                            .collect();
                        // Emit the verdict to stdout for interactive runs, stderr
                        // otherwise (keeps `--json` stdout a clean document).
                        let say = |line: String| {
                            if interactive {
                                println!("{}", line);
                            } else {
                                eprintln!("{}", line);
                            }
                        };
                        if report.only_nonessential_missing() {
                            say(format!(
                                "\x1b[90mℹ {:.1}% of data available ({} non-essential missing: {})\x1b[0m",
                                percent,
                                report.missing_files.len(),
                                missing_display.join(", ")
                            ));
                        } else if report.likely_repairable() {
                            say(format!(
                                "\x1b[33m⚠ {:.1}% of data available; {} recovery present — PAR2 repair likely.\x1b[0m",
                                percent,
                                human_bytes(report.available_par2_bytes as f64)
                            ));
                        } else {
                            let reason = if report.has_par2 {
                                "PAR2 recovery is insufficient"
                            } else {
                                "no PAR2 files for repair"
                            };
                            say(format!(
                                "\x1b[31m✗ {:.1}% of data available; {} — repair unlikely.\x1b[0m",
                                percent, reason
                            ));
                            if interactive {
                                eprint!("  Continue anyway? [y/N] ");
                                use std::io::{self, BufRead, Write};
                                io::stderr().flush().ok();
                                let stdin = io::stdin();
                                let proceed = matches!(
                                    stdin.lock().lines().next(),
                                    Some(Ok(line)) if {
                                        let a = line.trim().to_lowercase();
                                        a == "y" || a == "yes"
                                    }
                                );
                                if !proceed {
                                    println!("  Aborted.");
                                    continue;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    if let Some(s) = spinner {
                        s.finish_and_clear();
                    }
                    eprintln!("Warning: could not check availability: {}", e);
                }
            }
        }

        match downloader
            .download_nzb_on_demand(
                &nzb,
                download_config.clone(),
                skip_message_ids.as_ref(),
                config.post_processing.download_all_par2,
            )
            .await
        {
            Ok(outcome) => {
                let results = outcome.files;
                let transfer_time = outcome.transfer_duration;
                let actual_wire_bytes = outcome.actual_wire_bytes;
                let download_time = download_start.elapsed();

                let mut post_result = PostProcessingResult {
                    par2_verified: false,
                    par2_repaired: false,
                    rar_extracted: false,
                    files_renamed: 0,
                };
                let mut post_outcome: Option<PostProcessingOutcome> = None;
                let mut post_failed = false;

                // Skip post-processing entirely if a shutdown was requested —
                // PAR2 repair / RAR extraction on an interrupted, incomplete
                // download is wasted work the user asked us to stop.
                let interrupted = dl_nzb::shutdown::is_requested();

                if interrupted {
                    let partials = count_partials(&output_dir);
                    if !cli.json {
                        eprintln!(
                            "Skipping post-processing (interrupted). Left {} incomplete file(s) in {}",
                            partials,
                            output_dir.display()
                        );
                    }
                } else if config.post_processing.auto_par2_repair
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

                // Re-read shutdown: a Ctrl-C *during* post-processing (cancelled
                // PAR2 repair returns Failed) should read as "interrupted", not a
                // genuine verification failure.
                let interrupted = interrupted || dl_nzb::shutdown::is_requested();
                let download_ok = results.iter().all(|r| r.segments_failed == 0);
                let post_ok = if post_failed {
                    false
                } else if let Some(outcome) = post_outcome.as_ref() {
                    outcome.par2_status != Par2Status::Failed && outcome.rar_archives_failed == 0
                } else {
                    true
                };
                let success = download_ok && post_ok && !interrupted;

                if cli.json {
                    let total_size: u64 = results.iter().map(|r| r.size).sum();
                    let par2_bytes: u64 = results
                        .iter()
                        .filter(|r| dl_nzb::patterns::par2::is_par2_file(&r.path))
                        .map(|r| r.size)
                        .sum();
                    let data_bytes = total_size.saturating_sub(par2_bytes);
                    let transfer_secs = transfer_time.as_secs_f64();
                    // Guard against division by ~0 for very short downloads;
                    // anything under 50 ms doesn't yield a meaningful rate.
                    let speed_mib_per_sec = if transfer_secs >= 0.05 {
                        (actual_wire_bytes as f64) / 1_048_576.0 / transfer_secs
                    } else {
                        0.0
                    };
                    let summary = DownloadSummary {
                        nzb: nzb_path.clone(),
                        output_dir: output_dir.clone(),
                        success,
                        total_size,
                        data_bytes,
                        par2_bytes,
                        wire_bytes: actual_wire_bytes,
                        download_time_seconds: download_time.as_secs_f64(),
                        transfer_time_seconds: transfer_secs,
                        average_speed_mib_per_sec: speed_mib_per_sec,
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
                } else if interrupted {
                    println!(
                        "\x1b[1;33m■ Interrupted\x1b[0m \x1b[90m└─\x1b[0m \x1b[34m{}\x1b[0m",
                        output_dir.display()
                    );
                } else {
                    print_final_summary(
                        &results,
                        &output_dir,
                        download_time,
                        post_outcome.as_ref(),
                        post_failed,
                    );
                }
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

    if !cli.quiet && !cli.json {
        print!("\x07");
    }

    Ok(())
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 {
            format!("{}m", m)
        } else {
            format!("{}m {}s", m, s)
        }
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{}h", h)
        } else {
            format!("{}h {}m", h, m)
        }
    }
}

fn print_final_summary(
    results: &[dl_nzb::download::DownloadResult],
    output_dir: &std::path::Path,
    download_time: std::time::Duration,
    post_outcome: Option<&PostProcessingOutcome>,
    post_failed: bool,
) {
    let total_size: u64 = results.iter().map(|r| r.size).sum();
    let failed_count = results.iter().filter(|r| r.segments_failed > 0).count();

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
                    && !name.ends_with(".partial")
            })
            .max_by_key(|e| e.metadata().ok().map(|m| m.len()).unwrap_or(0))
    });

    println!();

    let post_issue: Option<&str> = if post_failed {
        Some("Post-processing failed")
    } else if let Some(outcome) = post_outcome {
        if outcome.par2_status == Par2Status::Failed {
            Some("PAR2 verification failed")
        } else if outcome.rar_archives_failed > 0 {
            Some("RAR extraction had failures")
        } else {
            None
        }
    } else {
        None
    };

    if failed_count == 0 && post_issue.is_none() {
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
                "  \x1b[90m└─\x1b[0m \x1b[36m{}\x1b[0m in \x1b[35m{}\x1b[0m",
                human_bytes(file_size as f64),
                format_duration(download_time)
            );
        } else {
            println!("\x1b[1;32m✓ Complete\x1b[0m");
            println!(
                "  \x1b[90m└─\x1b[0m \x1b[34m{}\x1b[0m",
                output_dir.display()
            );
            println!(
                "  \x1b[90m└─\x1b[0m \x1b[36m{}\x1b[0m in \x1b[35m{}\x1b[0m",
                human_bytes(total_size as f64),
                format_duration(download_time)
            );
        }
    } else if failed_count == 0 {
        let issue = post_issue.unwrap_or("Post-processing issues");
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

/// Count `*.partial` files left behind in a directory (after an interrupt we
/// keep them rather than renaming truncated data to final names).
fn count_partials(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".partial"))
                .count()
        })
        .unwrap_or(0)
}
