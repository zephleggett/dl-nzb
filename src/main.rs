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

fn main() {
    // Explicit runtime so the blocking pool is bounded: per-article writes,
    // finalize, PAR2 repair and RAR extraction all use spawn_blocking, and the
    // 512-thread default could balloon thread/stack/fd use. 64 comfortably
    // covers concurrent writes + finalizes + one PAR2 + one RAR.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(64)
        .build()
        .expect("failed to build Tokio runtime");
    rt.block_on(async_main());
}

async fn async_main() {
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
    // Resolve colour + glyph mode ONCE, before logging or any output. JSON mode
    // suppresses all decorative output anyway, so force colour off there. Auto
    // detection keys off the stream this command writes to: stdout for the
    // `list` / `config` data dumps, stderr for the download/test narrative.
    use std::io::IsTerminal;
    let color_choice = if cli.json {
        dl_nzb::ui::style::ColorChoice::Never
    } else {
        cli.color.into()
    };
    let stdout_command = cli.list || matches!(cli.command, Some(Commands::Config));
    let auto_tty = if stdout_command {
        std::io::stdout().is_terminal()
    } else {
        std::io::stderr().is_terminal()
    };
    dl_nzb::ui::style::init(color_choice, auto_tty);
    dl_nzb::ui::glyph::init();

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

/// A `MakeWriter` that wraps each log write in `MultiProgress::suspend` so log
/// lines never tear a live progress bar, and always writes to stderr (stdout is
/// reserved for `--json` / `list` data).
struct SuspendingWriter;

impl std::io::Write for SuspendingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        dl_nzb::progress::multi().suspend(|| std::io::stderr().write_all(buf))?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SuspendingWriter {
    type Writer = SuspendingWriter;
    fn make_writer(&'a self) -> Self::Writer {
        SuspendingWriter
    }
}

fn init_logging(cli: &Cli) -> Result<()> {
    let filter = EnvFilter::try_new(cli.get_log_level())
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("par2_rs=off".parse().unwrap());

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(SuspendingWriter);

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
                use dl_nzb::ui;
                let spinner = dl_nzb::progress::DelayedSpinner::new(
                    "Testing connection…",
                    std::time::Duration::from_millis(150),
                );
                match AsyncNntpConnection::connect(&test_config, None, throwaway_counter()).await {
                    Ok(mut conn) => {
                        let healthy = conn.is_healthy().await;
                        let _ = conn.close().await;
                        spinner.finish_and_clear();
                        // Human diagnostic → stderr (keeps the narrative off stdout,
                        // consistent with the download run).
                        eprintln!(
                            "{}",
                            ui::ok_line(format!("Connected to {}", test_config.server))
                        );
                        eprintln!("{}", ui::child_line(!healthy, "Authentication OK"));
                        if healthy {
                            eprintln!("{}", ui::child_line(true, "Server healthy"));
                        }
                    }
                    Err(e) => {
                        spinner.finish_and_clear();
                        eprintln!("{}", ui::error_line(format!("Connection failed: {e}")));
                        return Err(e);
                    }
                }
            }
            Ok(())
        }

        Commands::Config => {
            use dl_nzb::ui::{self, style};
            let config_path = Config::config_path()?;
            println!("{}", style::heading("Configuration"));
            println!(
                "{}",
                ui::child_line(true, style::path(&config_path.display().to_string()))
            );
            println!();

            if config_path.exists() {
                let mut config = Config::load()?;
                if !config.usenet.password.is_empty() {
                    config.usenet.password = "********".to_string();
                }
                println!("{}", style::dim(&ui::rule(60)));
                println!("{}", config.display_toml()?);
                println!("{}", style::dim(&ui::rule(60)));
            } else {
                println!(
                    "{}",
                    ui::child_line(
                        true,
                        ui::warn_line("No config yet — run any command to create it.")
                    )
                );
            }
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
        use dl_nzb::ui::{self, style};
        for nzb_path in &cli.files {
            let nzb = Nzb::from_file(nzb_path)?;
            println!();
            println!(
                "{}{}",
                style::heading(&nzb_path.display().to_string()),
                style::dim(&format!(
                    "  ·  {} · {} files · {} segments",
                    human_bytes(nzb.total_size() as f64),
                    nzb.files().len(),
                    nzb.total_segments(),
                ))
            );
            let files = nzb.files();
            let shown = files.len().min(ui::MAX_LISTED_FILES);
            let extra = files.len().saturating_sub(shown);
            for (i, file) in files.iter().take(shown).enumerate() {
                let last = extra == 0 && i + 1 == shown;
                let filename = Nzb::get_filename_from_subject(&file.subject)
                    .unwrap_or_else(|| file.subject.clone());
                let display_name = ui::truncate_middle(&ui::sanitize_display(&filename), 48);
                let size: u64 = file.segments.segment.iter().map(|s| s.bytes).sum();
                let is_par2 = dl_nzb::patterns::par2::is_par2_file(std::path::Path::new(&filename));
                let tag = if is_par2 {
                    style::dim("PAR2")
                } else {
                    style::accent("DATA")
                };
                println!(
                    "{}",
                    ui::child_line(
                        last,
                        format!(
                            "{}  {}  {}",
                            tag,
                            display_name,
                            style::info(&human_bytes(size as f64))
                        )
                    )
                );
            }
            if extra > 0 {
                println!(
                    "{}",
                    ui::child_line(true, style::dim(&format!("… and {extra} more")))
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
        let spinner = dl_nzb::progress::DelayedSpinner::new(
            "Connecting to server…",
            std::time::Duration::from_millis(150),
        );
        let downloader = Downloader::new(config.clone()).await?;
        spinner.finish_and_clear();
        downloader
    };

    // Track the whole batch for the optional completion bell.
    let batch_start = std::time::Instant::now();
    let mut all_succeeded = true;
    let total_nzbs = cli.files.len();

    for (idx, nzb_path) in cli.files.iter().enumerate() {
        // Don't start (or continue to) another NZB after an interrupt.
        if dl_nzb::shutdown::is_requested() {
            break;
        }
        let nzb = match Nzb::from_file(nzb_path) {
            Ok(nzb) => nzb,
            Err(e) => {
                eprintln!("Failed to load {}: {}", nzb_path.display(), e);
                all_succeeded = false;
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

        // Per-NZB banner: release name + size/file count, with a blank line
        // separating consecutive NZBs.
        {
            use dl_nzb::ui::{self, style};
            let title = nzb_path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(ui::sanitize_display)
                .unwrap_or_else(|| nzb_path.display().to_string());
            let title = ui::truncate_middle(&title, 64);
            let counter = if total_nzbs > 1 {
                format!("  [{}/{}]", idx + 1, total_nzbs)
            } else {
                String::new()
            };
            let file_count = nzb.files().len();
            ui::blank();
            ui::header(format!(
                "{}{}",
                style::heading(&title),
                style::dim(&format!(
                    "  ·  {} · {} file{}{counter}",
                    human_bytes(nzb.total_size() as f64),
                    file_count,
                    if file_count == 1 { "" } else { "s" },
                ))
            ));
        }

        let mut download_config = config.clone();
        download_config.download.dir = output_dir.clone();
        download_config.download.force_redownload = cli.force;

        let download_start = std::time::Instant::now();

        // Pre-flight availability scan: STAT every segment to learn exactly
        // what's missing before committing to the download. Builds the skip set
        // (so missing articles are never fetched) and estimates whether PAR2 can
        // repair the gap. Runs in every mode; only the interactive abort prompt
        // is gated to interactive runs (JSON/quiet report and continue).
        let scan_start = std::time::Instant::now();
        let mut skip_message_ids: Option<std::collections::HashSet<String>> = None;
        {
            use dl_nzb::ui::{self, glyph, style};
            let interactive = !cli.json && !cli.quiet;
            // The pre-flight scan's only unique value is the interactive abort
            // verdict and, when there's no PAR2, learning repair is impossible up
            // front. On a non-interactive run that has PAR2, skip it: missing
            // articles are detected inline (430) and on-demand recovery handles
            // the gap, so the scan would just be latency before the first byte.
            let has_par2 = nzb.files().iter().any(|f| {
                let name =
                    Nzb::get_filename_from_subject(&f.subject).unwrap_or_else(|| f.subject.clone());
                dl_nzb::patterns::par2::is_par2_file(std::path::Path::new(&name))
            });
            if interactive || !has_par2 {
                let spinner = interactive.then(|| {
                    dl_nzb::progress::DelayedSpinner::new(
                        "Checking article availability…",
                        std::time::Duration::from_millis(150),
                    )
                });
                match downloader.check_all_availability(&nzb).await {
                    Ok(report) => {
                        if let Some(s) = spinner {
                            s.finish_and_clear();
                        }
                        if !report.missing_ids.is_empty() {
                            skip_message_ids = Some(report.missing_ids.clone());
                        }
                        // If the scan was interrupted (Ctrl+C), don't print a verdict
                        // from partial data or show the interactive prompt — we're
                        // about to abort below.
                        if !report.missing_files.is_empty() && !dl_nzb::shutdown::is_requested() {
                            let percent = report.data_completion_percent();
                            let missing_display: Vec<String> = report
                                .missing_files
                                .iter()
                                .map(|n| ui::sanitize_display(n))
                                .collect();
                            if report.only_nonessential_missing() {
                                ui::child(
                                    false,
                                    style::dim(&format!(
                                    "{} {:.1}% of data available ({} non-essential missing: {})",
                                    glyph::INFO,
                                    percent,
                                    report.missing_files.len(),
                                    missing_display.join(", ")
                                )),
                                );
                            } else if report.likely_repairable() {
                                ui::child(
                                false,
                                ui::warn_line(format!(
                                    "{:.1}% of data available; {} recovery present — PAR2 repair likely.",
                                    percent,
                                    human_bytes(report.available_par2_bytes as f64)
                                )),
                            );
                            } else {
                                let reason = if report.has_par2 {
                                    "PAR2 recovery is insufficient"
                                } else {
                                    "no PAR2 files for repair"
                                };
                                ui::child(
                                    false,
                                    ui::error_line(format!(
                                    "{percent:.1}% of data available; {reason} — repair unlikely."
                                )),
                                );

                                // Only prompt when stdin AND stderr are real TTYs; a
                                // piped/CI run must never block. Non-interactive skips
                                // the NZB unless --force is set.
                                use std::io::IsTerminal;
                                let can_prompt = interactive
                                    && std::io::stdin().is_terminal()
                                    && std::io::stderr().is_terminal();
                                if can_prompt {
                                    eprint!("  Continue anyway? [y/N] ");
                                    use std::io::{self, BufRead, Write};
                                    io::stderr().flush().ok();
                                    let proceed = matches!(
                                        io::stdin().lock().lines().next(),
                                        Some(Ok(line)) if {
                                            let a = line.trim().to_ascii_lowercase();
                                            a == "y" || a == "yes"
                                        }
                                    );
                                    if !proceed {
                                        eprintln!("  Aborted.");
                                        all_succeeded = false;
                                        continue;
                                    }
                                } else if !cli.force {
                                    eprintln!(
                                    "  Non-interactive: skipping likely-unrepairable download (re-run with --force to download anyway)."
                                );
                                    all_succeeded = false;
                                    continue;
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
        }
        let scan_seconds = scan_start.elapsed().as_secs_f64();

        // A Ctrl+C during the availability scan should abort before downloading.
        if dl_nzb::shutdown::is_requested() {
            break;
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
                let post_start = std::time::Instant::now();

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

                let post_processing_seconds = post_start.elapsed().as_secs_f64();

                // Re-read shutdown: a Ctrl-C *during* post-processing (cancelled
                // PAR2 repair returns Failed) should read as "interrupted", not a
                // genuine verification failure.
                let interrupted = interrupted || dl_nzb::shutdown::is_requested();
                // PAR2 repair is exactly how download-phase segment failures are
                // recovered, so a successful verify/repair means the payload is
                // whole regardless of wire hiccups. Absent that, the download is
                // still OK if the only failures are non-essential files
                // (.nfo/.sfv) the user doesn't actually need.
                let par2_repaired_ok = matches!(post_outcome.as_ref(), Some(o) if o.par2_status == Par2Status::Success);
                let download_ok = par2_repaired_ok
                    || results
                        .iter()
                        .all(|r| r.segments_failed == 0 || is_auxiliary(&r.filename));
                let post_ok = if post_failed {
                    false
                } else if let Some(outcome) = post_outcome.as_ref() {
                    outcome.par2_status != Par2Status::Failed && outcome.rar_archives_failed == 0
                } else {
                    true
                };
                let success = download_ok && post_ok && !interrupted;
                if !success {
                    all_succeeded = false;
                }

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
                        availability_scan_seconds: scan_seconds,
                        post_processing_seconds,
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
                    use dl_nzb::ui::{self, glyph, style};
                    ui::blank();
                    ui::header(style::warn(&format!("{} Interrupted", glyph::INTERRUPTED)));
                    ui::child(true, style::path(&output_dir.display().to_string()));
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
                all_succeeded = false;
            }
        }
    }

    // Optional completion bell: opt-in, success-only, only for a run long enough
    // to warrant attention, and never into a pipe/CI (stderr must be a TTY).
    {
        use std::io::IsTerminal;
        let long_enough =
            batch_start.elapsed().as_secs() >= config.notifications.notify_min_seconds;
        if !cli.quiet
            && !cli.json
            && config.notifications.notify_on_complete
            && all_succeeded
            && long_enough
            && std::io::stderr().is_terminal()
        {
            eprint!("\x07");
        }
    }

    Ok(())
}

/// Files we don't surface individually in the summary (recovery / metadata).
fn is_auxiliary(name: &str) -> bool {
    let n = name.to_lowercase();
    n.ends_with(".par2") || n.ends_with(".nfo") || n.ends_with(".sfv") || n.ends_with(".partial")
}

fn print_final_summary(
    results: &[dl_nzb::download::DownloadResult],
    output_dir: &std::path::Path,
    download_time: std::time::Duration,
    post_outcome: Option<&PostProcessingOutcome>,
    post_failed: bool,
) {
    use dl_nzb::ui::{self, glyph, style};

    let total_size: u64 = results.iter().map(|r| r.size).sum();
    // A successful PAR2 verify/repair means the payload is whole, so download-
    // phase segment failures were recovered and are no longer errors. Files that
    // are non-essential (.nfo/.sfv) and never arrived also aren't errors. So
    // count only essential payload files still broken after post-processing.
    let par2_ok = matches!(post_outcome, Some(o) if o.par2_status == Par2Status::Success);
    let failed_count = if par2_ok {
        0
    } else {
        results
            .iter()
            .filter(|r| r.segments_failed > 0 && !is_auxiliary(&r.filename))
            .count()
    };

    // Payload files (what the user actually wanted), failed-first then largest,
    // so the per-file cap never hides a failure.
    let mut payload: Vec<&dl_nzb::download::DownloadResult> = results
        .iter()
        .filter(|r| !is_auxiliary(&r.filename))
        .collect();
    payload.sort_by_key(|r| (r.segments_failed == 0, std::cmp::Reverse(r.size)));

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

    ui::blank();

    // Header line: status verb, plus the file name for a single-file release.
    let clean = failed_count == 0 && post_issue.is_none();
    if clean {
        let verb = ui::ok_line("Complete");
        if payload.len() == 1 {
            let name = ui::truncate_middle(&ui::sanitize_display(&payload[0].filename), 56);
            ui::header(format!(
                "{verb}{}{}",
                style::dim("  ·  "),
                style::heading(&name)
            ));
        } else {
            ui::header(verb);
        }
    } else if failed_count == 0 {
        ui::header(ui::warn_line("Completed with issues"));
    } else {
        ui::header(ui::warn_line(format!(
            "Completed — {failed_count} file{} with errors",
            ui::plural(failed_count)
        )));
    }

    // Child tree, built then emitted so exactly the last row gets └─.
    let mut tree = ui::Tree::new();

    // Multi-file: a compact list (capped, failed-first).
    if payload.len() > 1 {
        let shown = payload.len().min(ui::MAX_LISTED_FILES);
        for r in payload.iter().take(shown) {
            let name = ui::truncate_middle(&ui::sanitize_display(&r.filename), 48);
            let mark = if r.segments_failed == 0 || par2_ok {
                style::success(glyph::OK.as_str())
            } else {
                style::error(glyph::ERR.as_str())
            };
            tree.push(format!(
                "{mark}  {name}  {}",
                style::info(&human_bytes(r.size as f64))
            ));
        }
        let extra = payload.len().saturating_sub(shown);
        if extra > 0 {
            tree.push(
                style::dim(&format!("… and {extra} more file{}", ui::plural(extra))).to_string(),
            );
        }
    }

    if let Some(issue) = post_issue {
        tree.push(ui::warn_line(issue));
    }

    tree.push(format!(
        "{} {}",
        style::dim(glyph::INFO.as_str()),
        style::path(&output_dir.display().to_string())
    ));
    tree.push(format!(
        "{} {} in {}",
        style::dim(glyph::INFO.as_str()),
        style::info(&human_bytes(total_size as f64)),
        style::accent(&ui::format_duration(download_time)),
    ));

    tree.emit();
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
