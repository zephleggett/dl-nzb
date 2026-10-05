use human_bytes::human_bytes;
use std::error::Error;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tracing_subscriber::EnvFilter;

use dl_nzb::{
    cli::{observer::TerminalObserver, Cli, Commands},
    config::Config,
    download::split_password,
    engine::{
        Engine, ErrorKind, FileKind, FileReport, JobHandle, JobRequest, JobSummary, NzbInfo,
        OnUnrepairable, Outcome, Preflight,
    },
    error::DlNzbError,
    json_output::{
        DownloadFileResult, DownloadSummary, ErrorOutput, FileInfo, NzbInfo as NzbInfoJson,
        PostProcessingResult, TestResult,
    },
    patterns::{is_auxiliary_name, PARTIAL_EXT},
    serde_json,
};

type Result<T> = std::result::Result<T, DlNzbError>;

fn main() {
    let rt = dl_nzb::engine::runtime().expect("failed to build Tokio runtime");
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

    init_logging(&cli);

    // Two-stage Ctrl+C:
    //   1st: stop the running job gracefully: workers abandon their articles,
    //        the writer flushes what already arrived, incomplete files stay
    //        `.partial`, and post-processing is skipped.
    //   2nd: force an immediate exit (130).
    // A 10-second backstop hard-exits if the graceful stop hangs.
    let interrupt = Interrupt::default();
    spawn_signal_handler(interrupt.clone());

    if let Some(command) = &cli.command {
        return handle_command(command, &cli).await;
    }

    let mut config = load_config()?;
    config.apply_overrides(cli.get_config_overrides());
    config.validate()?;

    if cli.list {
        return handle_list_mode(&cli);
    }

    if cli.files.is_empty() {
        eprintln!("No NZB files specified. Use 'dl-nzb --help' for usage information.");
        return Ok(());
    }

    handle_download_mode(&cli, config, &interrupt).await
}

/// Load the configuration, creating (and announcing) the default file on first run.
fn load_config() -> Result<Config> {
    let (config, created) = Config::load_or_create()?;
    if let Some(path) = created {
        eprintln!("Created default configuration at: {}", path.display());
        eprintln!("Please edit this file with your Usenet server credentials.");
        eprintln!();
    }
    Ok(config)
}

/// The CLI's handle on the job in flight, for the Ctrl+C handler.
#[derive(Clone, Default)]
struct Interrupt {
    requested: Arc<AtomicBool>,
    current: Arc<Mutex<Option<JobHandle>>>,
}

impl Interrupt {
    fn requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn request(&self) {
        self.requested.store(true, Ordering::Release);
        if let Ok(current) = self.current.lock() {
            if let Some(job) = current.as_ref() {
                job.stop();
            }
        }
    }

    fn set_current(&self, job: Option<JobHandle>) {
        if let Ok(mut current) = self.current.lock() {
            *current = job;
        }
    }
}

/// Spawn the Ctrl+C handler. First interrupt stops the running job (and arms
/// a 10s hard-exit backstop); a second interrupt forces an immediate exit.
fn spawn_signal_handler(interrupt: Interrupt) {
    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => return,
        };
        sigint.recv().await;
        eprintln!("\nInterrupted; finishing pending writes, skipping post-processing…");
        interrupt.request();
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
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_err() {
            return;
        }
        eprintln!("\nInterrupted; finishing pending writes, skipping post-processing…");
        interrupt.request();
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

fn init_logging(cli: &Cli) {
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
}

async fn handle_command(command: &Commands, cli: &Cli) -> Result<()> {
    match command {
        Commands::Test => {
            let config = load_config()?;
            let server = config.usenet.clone();

            if cli.json {
                let mut result = TestResult {
                    server: server.server.clone(),
                    port: server.port,
                    ssl: server.ssl,
                    connected: false,
                    authenticated: false,
                    healthy: false,
                    error: None,
                };
                match Engine::test_connection(&server).await {
                    Ok(_) => {
                        result.connected = true;
                        result.authenticated = true;
                        result.healthy = true;
                    }
                    Err(e) => {
                        // Got far enough to be refused (or to see a bad reply).
                        result.connected =
                            matches!(e.kind(), ErrorKind::Auth | ErrorKind::Protocol);
                        result.authenticated = e.kind() == ErrorKind::Protocol;
                        result.error = Some(e.user_message());
                    }
                }
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                use dl_nzb::ui;
                let spinner = dl_nzb::progress::DelayedSpinner::new(
                    "Testing connection…",
                    std::time::Duration::from_millis(150),
                );
                let result = Engine::test_connection(&server).await;
                spinner.finish_and_clear();
                // Human diagnostic → stderr (keeps the narrative off stdout,
                // consistent with the download run).
                match result {
                    Ok(check) => {
                        eprintln!("{}", ui::ok_line(format!("Connected to {}", server.server)));
                        eprintln!("{}", ui::child_line(false, "Authentication OK"));
                        eprintln!(
                            "{}",
                            ui::child_line(
                                true,
                                format!("Server healthy ({} ms)", check.latency_ms)
                            )
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "{}",
                            ui::error_line(format!("Connection failed: {}", e.user_message()))
                        );
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
                let config = Config::load()?;
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

fn handle_list_mode(cli: &Cli) -> Result<()> {
    if cli.json {
        let mut results = Vec::new();
        for nzb_path in &cli.files {
            let info = Engine::inspect(nzb_path)?;
            results.push(NzbInfoJson {
                file: nzb_path.clone(),
                total_files: info.files.len(),
                total_size: info.total_bytes,
                total_segments: info.files.iter().map(|f| f.segments as usize).sum(),
                files: info
                    .files
                    .iter()
                    .map(|f| FileInfo {
                        filename: f.name.clone(),
                        size: f.bytes,
                        segments: f.segments as usize,
                        is_par2: f.kind == FileKind::Par2,
                    })
                    .collect(),
            });
        }
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        use dl_nzb::ui::{self, style};
        for nzb_path in &cli.files {
            let info = Engine::inspect(nzb_path)?;
            let segments: u64 = info.files.iter().map(|f| f.segments as u64).sum();
            println!();
            println!(
                "{}{}",
                style::heading(&nzb_path.display().to_string()),
                style::dim(&format!(
                    "  ·  {} · {} files · {} segments",
                    human_bytes(info.total_bytes as f64),
                    info.files.len(),
                    segments,
                ))
            );
            let shown = info.files.len().min(ui::MAX_LISTED_FILES);
            let extra = info.files.len().saturating_sub(shown);
            for (i, file) in info.files.iter().take(shown).enumerate() {
                let last = extra == 0 && i + 1 == shown;
                let display_name = ui::truncate_middle(&ui::sanitize_display(&file.name), 48);
                let tag = if file.kind == FileKind::Par2 {
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
                            style::info(&human_bytes(file.bytes as f64))
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

async fn handle_download_mode(cli: &Cli, config: Config, interrupt: &Interrupt) -> Result<()> {
    config.validate_for_download()?;

    // In JSON mode, suppress the decorative human-readable progress so the JSON
    // documents are the only stdout content. stderr remains for warnings.
    if cli.json {
        dl_nzb::output_mode::set_quiet(true);
    }

    let engine = Engine::new(config.clone())?;
    let base_dir = std::path::absolute(&config.download.dir)?;

    // Track the whole batch for the optional completion bell.
    let batch_start = std::time::Instant::now();
    let mut all_succeeded = true;
    let total_nzbs = cli.files.len();
    // Interactive runs check availability up front so an unrepairable release
    // can be confirmed before downloading; non-interactive runs use the
    // engine's automatic rule (scan only when there is no PAR2).
    let interactive = !cli.json && !cli.quiet;
    let on_unrepairable = if cli.force {
        OnUnrepairable::Continue
    } else {
        OnUnrepairable::Stop
    };

    for (idx, nzb_path) in cli.files.iter().enumerate() {
        // Don't start (or continue to) another NZB after an interrupt.
        if interrupt.requested() {
            break;
        }
        let info = match Engine::inspect(nzb_path) {
            Ok(info) => info,
            Err(e) => {
                eprintln!("Failed to load {}: {}", nzb_path.display(), e);
                all_succeeded = false;
                continue;
            }
        };

        // The job is named after its NZB file: `Name{{password}}.nzb` is
        // `Name` (the password is one of the NZB's, tried after --password).
        // Its folder, with subfolders on, and its renamed main file.
        let job_name = nzb_path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| split_password(s).0)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "download".to_string());
        let output_dir = if config.download.create_subfolders {
            base_dir.join(&job_name)
        } else {
            base_dir.clone()
        };

        print_banner(&info, idx, total_nzbs);

        let mut request = JobRequest {
            nzb_path: nzb_path.clone(),
            output_dir: output_dir.clone(),
            passwords: cli.passwords.clone(),
            preflight: if interactive {
                Preflight::Always
            } else {
                Preflight::Auto
            },
            on_unrepairable,
            free_space_hint: None,
            title: Some(job_name),
        };
        let mut summary = run_job(&engine, &config, request.clone(), &info, interrupt).await;

        if summary.outcome == Outcome::Unrepairable && !interrupt.requested() {
            // Only prompt when stdin AND stderr are real TTYs; a piped/CI run
            // must never block. Non-interactive skips the NZB unless --force.
            use std::io::IsTerminal;
            let can_prompt =
                interactive && std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
            if can_prompt && prompt_continue().await {
                // The scan already ran; don't repeat it.
                request.preflight = Preflight::Never;
                request.on_unrepairable = OnUnrepairable::Continue;
                summary = run_job(&engine, &config, request, &info, interrupt).await;
            } else {
                if can_prompt {
                    eprintln!("  Aborted.");
                } else {
                    eprintln!(
                        "  Non-interactive: skipping likely-unrepairable download (re-run with --force to download anyway)."
                    );
                }
                all_succeeded = false;
                continue;
            }
        }

        // Couldn't reach or log in to the server at all: that fails every NZB
        // the same way, so end the run with the error (exit 1).
        if let (Outcome::Failed, Some(kind)) = (summary.outcome, summary.error_kind) {
            let connection_problem = matches!(
                kind,
                ErrorKind::Auth
                    | ErrorKind::Dns
                    | ErrorKind::Connect
                    | ErrorKind::Tls
                    | ErrorKind::Timeout
            );
            if connection_problem && summary.nzb_files.is_empty() {
                return Err(DlNzbError::Job {
                    kind,
                    message: summary.message.unwrap_or_default(),
                });
            }
        }

        if summary.outcome != Outcome::Completed {
            all_succeeded = false;
        }
        report(cli, nzb_path, &summary)?;
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

/// Run one job with a terminal observer and wait for it, registering it with
/// the Ctrl+C handler meanwhile.
async fn run_job(
    engine: &Engine,
    config: &Config,
    request: JobRequest,
    info: &NzbInfo,
    interrupt: &Interrupt,
) -> JobSummary {
    let observer = Arc::new(TerminalObserver::new(
        info,
        config.post_processing.download_all_par2,
    ));
    let job = engine.start(request, observer);
    interrupt.set_current(Some(job.clone()));
    // A Ctrl+C that landed between the loop's check and `start`.
    if interrupt.requested() {
        job.stop();
    }
    let summary = job.wait().await;
    interrupt.set_current(None);
    summary
}

/// "Continue anyway? [y/N]" on stderr; reads stdin off the async threads.
async fn prompt_continue() -> bool {
    tokio::task::spawn_blocking(|| {
        use std::io::{self, BufRead, Write};
        eprint!("  Continue anyway? [y/N] ");
        io::stderr().flush().ok();
        matches!(
            io::stdin().lock().lines().next(),
            Some(Ok(line)) if {
                let a = line.trim().to_ascii_lowercase();
                a == "y" || a == "yes"
            }
        )
    })
    .await
    .unwrap_or(false)
}

/// Per-NZB banner: release name + size/file count, with a blank line
/// separating consecutive NZBs.
fn print_banner(info: &NzbInfo, idx: usize, total: usize) {
    use dl_nzb::ui::{self, style};
    let title = ui::truncate_middle(&ui::sanitize_display(&info.title), 64);
    let counter = if total > 1 {
        format!("  [{}/{}]", idx + 1, total)
    } else {
        String::new()
    };
    let file_count = info.files.len();
    ui::blank();
    ui::header(format!(
        "{}{}",
        style::heading(&title),
        style::dim(&format!(
            "  ·  {} · {} file{}{counter}",
            human_bytes(info.total_bytes as f64),
            file_count,
            ui::plural(file_count),
        ))
    ));
}

/// Print a finished job: the JSON document, or the human summary.
fn report(cli: &Cli, nzb_path: &Path, summary: &JobSummary) -> Result<()> {
    let job_error = summary.outcome == Outcome::Failed && summary.error_kind.is_some();
    if cli.json {
        if job_error {
            let error_output = ErrorOutput {
                error: summary.message.clone().unwrap_or_default(),
                details: None,
            };
            println!("{}", serde_json::to_string_pretty(&error_output)?);
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&json_summary(nzb_path, summary))?
            );
        }
        return Ok(());
    }

    use dl_nzb::ui::{self, glyph, style};
    if summary.outcome == Outcome::Stopped {
        eprintln!(
            "Skipping post-processing (interrupted). Left {} incomplete file(s) in {}",
            count_partials(&summary.output_dir),
            summary.output_dir.display()
        );
        ui::blank();
        ui::header(style::warn(&format!("{} Interrupted", glyph::INTERRUPTED)));
        ui::child(true, style::path(&summary.output_dir.display().to_string()));
        if summary.resumable {
            eprintln!("Run the same command again to resume.");
        }
    } else if job_error {
        eprintln!(
            "Download failed for {}: {}",
            nzb_path.display(),
            summary.message.as_deref().unwrap_or("unknown error")
        );
        if summary.resumable {
            eprintln!("Run the same command again to resume.");
        }
    } else {
        print_stage_lines(summary);
        print_final_summary(summary);
    }
    Ok(())
}

fn json_summary(nzb_path: &Path, summary: &JobSummary) -> DownloadSummary {
    let total_size: u64 = summary.nzb_files.iter().map(|f| f.bytes).sum();
    let par2_bytes = total_size.saturating_sub(summary.data_bytes);
    let transfer_secs = summary.download_secs;
    // Guard against division by ~0 for very short downloads; anything under
    // 50 ms doesn't yield a meaningful rate.
    let speed_mib_per_sec = if transfer_secs >= 0.05 {
        (summary.wire_bytes as f64) / 1_048_576.0 / transfer_secs
    } else {
        0.0
    };
    DownloadSummary {
        nzb: nzb_path.to_path_buf(),
        output_dir: summary.output_dir.clone(),
        success: summary.outcome == Outcome::Completed,
        outcome: outcome_name(summary.outcome),
        message: summary.message.clone(),
        total_size,
        data_bytes: summary.data_bytes,
        par2_bytes,
        wire_bytes: summary.wire_bytes,
        download_time_seconds: (summary.elapsed_secs - summary.post_secs).max(0.0),
        transfer_time_seconds: transfer_secs,
        average_speed_mib_per_sec: speed_mib_per_sec,
        availability_scan_seconds: summary.check_secs,
        post_processing_seconds: summary.post_secs,
        files: summary
            .nzb_files
            .iter()
            .map(|f| DownloadFileResult {
                filename: f.name.clone(),
                path: f.path.clone(),
                size: f.bytes,
                segments_downloaded: f.articles_total.saturating_sub(f.articles_failed) as usize,
                segments_failed: f.articles_failed as usize,
                success: f.articles_failed == 0,
            })
            .collect(),
        post_processing: PostProcessingResult {
            par2_verified: summary.par2.verified_ok,
            par2_repaired: summary.par2.repaired,
            rar_extracted: summary.archives_extracted > 0,
            files_renamed: summary.files_renamed as usize,
        },
    }
}

/// The JSON name of an outcome.
fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Completed => "completed",
        Outcome::CompletedWithIssues => "completed_with_issues",
        Outcome::Failed => "failed",
        Outcome::Stopped => "stopped",
        Outcome::NeedsPassword => "needs_password",
        Outcome::Unrepairable => "unrepairable",
    }
}

/// One status line per post-processing stage, in the order they ran.
fn print_stage_lines(summary: &JobSummary) {
    use dl_nzb::ui::{self, glyph, plural, style};
    let par2 = &summary.par2;
    if par2.ran {
        if par2.verified_ok && par2.repaired {
            ui::child(
                false,
                ui::ok_line(format!(
                    "PAR2 verified (repaired {} block{})",
                    par2.repaired_blocks,
                    plural(par2.repaired_blocks as usize)
                )),
            );
        } else if par2.verified_ok {
            ui::child(false, ui::ok_line("PAR2 verified"));
        } else {
            if par2.damaged_blocks > 0 {
                ui::child(
                    false,
                    ui::warn_line(format!(
                        "{} damaged block{}",
                        par2.damaged_blocks,
                        plural(par2.damaged_blocks as usize)
                    )),
                );
            }
            ui::child(false, ui::error_line("PAR2 failed: not repairable"));
        }
    } else if par2.verified_ok {
        ui::child(
            false,
            ui::ok_line("Verified on download — skipped PAR2 re-scan"),
        );
    }

    let (ok, failed) = (
        summary.archives_extracted as usize,
        summary.archives_failed as usize,
    );
    if ok > 0 && failed == 0 {
        ui::child(
            false,
            ui::ok_line(format!("Extracted {ok} archive{}", plural(ok))),
        );
    } else if ok > 0 {
        ui::child(
            false,
            ui::warn_line(format!(
                "Extracted {ok} archive{} ({failed} failed)",
                plural(ok)
            )),
        );
    } else if failed > 0 {
        ui::child(
            false,
            ui::error_line(format!(
                "Extraction failed for {failed} archive{}",
                plural(failed)
            )),
        );
    }
    if summary.files_renamed > 0 {
        ui::child(
            false,
            style::info(&format!(
                "{} Deobfuscated ({} renamed)",
                glyph::OK,
                summary.files_renamed
            )),
        );
    }
}

fn print_final_summary(summary: &JobSummary) {
    use dl_nzb::ui::{self, glyph, style};

    let results = &summary.nzb_files;
    let total_size: u64 = results.iter().map(|r| r.bytes).sum();
    // A successful PAR2 verify/repair means the payload is whole, so download-
    // phase segment failures were recovered and are no longer errors.
    let par2_ok = summary.par2.verified_ok;

    // Payload files (what the user actually wanted), failed-first then largest,
    // so the per-file cap never hides a failure.
    let mut payload: Vec<&FileReport> = results
        .iter()
        .filter(|r| !is_auxiliary_name(&r.name))
        .collect();
    payload.sort_by_key(|r| (r.articles_failed == 0, std::cmp::Reverse(r.bytes)));

    let (header, note) = verdict_lines(summary.outcome, summary.message.as_deref(), &payload);
    ui::blank();
    ui::header(header);

    // Child tree, built then emitted so exactly the last row gets └─.
    let mut tree = ui::Tree::new();

    // Multi-file: a compact list (capped, failed-first).
    if payload.len() > 1 {
        let shown = payload.len().min(ui::MAX_LISTED_FILES);
        for r in payload.iter().take(shown) {
            let name = ui::truncate_middle(&ui::sanitize_display(&r.name), 48);
            let mark = if r.articles_failed == 0 || par2_ok {
                style::success(glyph::OK.as_str())
            } else {
                style::error(glyph::ERR.as_str())
            };
            tree.push(format!(
                "{mark}  {name}  {}",
                style::info(&human_bytes(r.bytes as f64))
            ));
        }
        let extra = payload.len().saturating_sub(shown);
        if extra > 0 {
            tree.push(
                style::dim(&format!("… and {extra} more file{}", ui::plural(extra))).to_string(),
            );
        }
    }

    if let Some(note) = note {
        tree.push(note);
    }
    if summary.outcome == Outcome::NeedsPassword {
        tree.push(style::dim("Re-run with --password <PW> to extract it").to_string());
    }

    let download_time =
        std::time::Duration::from_secs_f64((summary.elapsed_secs - summary.post_secs).max(0.0));
    tree.push(format!(
        "{} {}",
        style::dim(glyph::INFO.as_str()),
        style::path(&summary.output_dir.display().to_string())
    ));
    tree.push(format!(
        "{} {} in {}",
        style::dim(glyph::INFO.as_str()),
        style::info(&human_bytes(total_size as f64)),
        style::accent(&ui::format_duration(download_time)),
    ));

    tree.emit();
}

/// The summary's header and the line under it, both from the engine's
/// verdict: its outcome and its one-sentence `message`. The header names the
/// file of a single-file release that completed (`payload` is what the user
/// wanted from the release).
fn verdict_lines(
    outcome: Outcome,
    message: Option<&str>,
    payload: &[&FileReport],
) -> (String, Option<String>) {
    use dl_nzb::ui::{self, glyph, style};
    let message = message.map(|m| m.trim_end_matches('.'));
    match outcome {
        Outcome::Completed => {
            let verb = ui::ok_line("Complete");
            let header = match payload {
                [only] => format!(
                    "{verb}{}{}",
                    style::dim("  ·  "),
                    style::heading(&ui::truncate_middle(&ui::sanitize_display(&only.name), 56))
                ),
                _ => verb,
            };
            (header, None)
        }
        Outcome::CompletedWithIssues => (
            ui::warn_line("Completed with issues"),
            message.map(ui::warn_line),
        ),
        // The header says a password is needed; add why only when the ones
        // given were refused ("The password didn't work.").
        Outcome::NeedsPassword => (
            ui::warn_line("Archive needs a password"),
            message
                .filter(|m| !m.contains("needs a password"))
                .map(ui::warn_line),
        ),
        Outcome::Failed => (
            ui::error_line("Download failed"),
            message.map(ui::error_line),
        ),
        Outcome::Unrepairable => (
            ui::error_line("Not repairable"),
            message.map(ui::error_line),
        ),
        Outcome::Stopped => (
            style::warn(&format!("{} Interrupted", glyph::INTERRUPTED)).to_string(),
            message.map(ui::warn_line),
        ),
    }
}

/// Count `*.partial` files left behind in a directory (after an interrupt we
/// keep them rather than renaming truncated data to final names).
fn count_partials(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(PARTIAL_EXT))
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, articles_failed: u64) -> FileReport {
        FileReport {
            name: name.to_string(),
            path: name.into(),
            bytes: 100,
            articles_total: 10,
            articles_failed,
        }
    }

    /// The header and the line under it come from the engine's verdict. A
    /// payload missing articles PAR2 could not repair used to read
    /// "Completed — 1 file with errors", without the engine's sentence, and
    /// a download that never finished read "Complete".
    #[test]
    fn the_summary_says_what_the_engine_decided() {
        let (broken, whole) = (file("a.mkv", 3), file("b.mkv", 0));
        let lines = |outcome, message| verdict_lines(outcome, message, &[&broken, &whole]);

        let (header, note) = lines(
            Outcome::Failed,
            Some("12% of articles are missing and PAR2 could not repair them."),
        );
        assert!(header.ends_with("Download failed"), "{header}");
        assert!(
            note.as_deref().is_some_and(
                |n| n.ends_with("12% of articles are missing and PAR2 could not repair them")
            ),
            "{note:?}"
        );
        let (header, note) = verdict_lines(
            Outcome::Failed,
            Some("The download did not finish."),
            &[&whole],
        );
        assert!(header.ends_with("Download failed"), "{header}");
        assert!(note.is_some_and(|n| n.ends_with("The download did not finish")));

        let (header, note) = verdict_lines(Outcome::Completed, None, &[&whole]);
        assert!(
            header.contains("Complete") && header.ends_with("b.mkv"),
            "{header}"
        );
        assert_eq!(note, None);

        let (header, note) = lines(
            Outcome::CompletedWithIssues,
            Some("1 archive could not be extracted."),
        );
        assert!(header.ends_with("Completed with issues"), "{header}");
        assert!(note.is_some_and(|n| n.ends_with("1 archive could not be extracted")));

        let (header, note) = lines(
            Outcome::NeedsPassword,
            Some("This archive needs a password."),
        );
        assert!(header.ends_with("Archive needs a password"), "{header}");
        assert_eq!(note, None);
        let (_, note) = lines(Outcome::NeedsPassword, Some("The password didn't work."));
        assert!(note.is_some_and(|n| n.ends_with("The password didn't work")));

        let (header, note) = lines(
            Outcome::Unrepairable,
            Some("9% of articles are missing and there is no recovery data."),
        );
        assert!(header.ends_with("Not repairable"), "{header}");
        assert!(note.is_some_and(|n| n.ends_with("there is no recovery data")));
    }
}
