use human_bytes::human_bytes;
use tracing_subscriber::EnvFilter;

use dl_nzb::{
    cli::{Cli, Commands},
    config::Config,
    download::{Downloader, Nzb},
    error::{ConfigError, DlNzbError},
    nntp::AsyncNntpConnection,
    processing::PostProcessor,
};

type Result<T> = std::result::Result<T, DlNzbError>;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse_and_validate();

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
    let filter = EnvFilter::try_new(cli.get_log_level()).unwrap_or_else(|_| EnvFilter::new("info"));

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
async fn handle_command(command: &Commands, _cli: &Cli) -> Result<()> {
    match command {
        Commands::Test { server } => {
            println!("Testing connection to Usenet server...");

            let config = Config::load()?;
            let test_config = if let Some(server) = server {
                let mut cfg = config.usenet.clone();
                cfg.server = server.clone();
                cfg
            } else {
                config.usenet.clone()
            };

            // Test connection using async NNTP (no shared connector for test)
            match AsyncNntpConnection::connect(&test_config, None).await {
                Ok(mut conn) => {
                    println!("✓ Successfully connected to {}", test_config.server);
                    println!("   Authentication: OK");

                    if conn.is_healthy().await {
                        println!("   Server status: Healthy");
                    }

                    let _ = conn.close().await;
                    Ok(())
                }
                Err(e) => {
                    eprintln!("❌ Connection failed: {}", e);
                    Err(e)
                }
            }
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

        Commands::History {
            show: _,
            clear: _,
            remove: _,
        } => {
            eprintln!("❌ History feature is not yet implemented.");
            eprintln!();
            eprintln!("This is a planned feature for tracking download history.");
            eprintln!("Check https://github.com/zephleggett/dl-nzb/issues for updates.");
            eprintln!();
            eprintln!("For now, downloaded files are tracked in the filesystem.");
            std::process::exit(1);
        }

        Commands::Version { detailed } => {
            println!("dl-nzb {}", env!("CARGO_PKG_VERSION"));

            if *detailed {
                println!("Build information:");
                println!("  Package version: {}", env!("CARGO_PKG_VERSION"));
                println!("  Features: async, connection-pooling, streaming");
            }
            Ok(())
        }
    }
}

/// Handle list mode
async fn handle_list_mode(cli: &Cli) -> Result<()> {
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

    Ok(())
}

/// Handle download mode
async fn handle_download_mode(cli: &Cli, mut config: Config) -> Result<()> {
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

    // Update memory settings
    if let Some(memory_mb) = cli.memory_limit {
        config.memory.max_segments_in_memory = (memory_mb * 1024 * 1024) / 100_000; // Rough estimate
    }
    config.memory.io_buffer_size = cli.buffer_size * 1024;
    config.memory.max_concurrent_files = cli.max_concurrent_files;

    // Create downloader with spinner
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

        // Download the NZB with updated config
        match downloader
            .download_nzb(&nzb, download_config.clone())
            .await
        {
            Ok((results, _progress_bar)) => {
                // Keep the download progress bar visible
                // Don't call finish_and_clear() - let it stay on screen

                if cli.print_names {
                    for result in &results {
                        println!("{}", result.path.display());
                    }
                }

                // Post-processing - create new progress bars
                if config.post_processing.auto_par2_repair
                    || config.post_processing.auto_extract_rar
                {
                    let processor = PostProcessor::new(download_config.post_processing.clone());
                    if let Err(e) = processor.process_downloads(&results).await {
                        eprintln!("Post-processing error: {}", e);
                    }
                }

                // Print final summary
                print_final_summary(&nzb, &results, &output_dir);

                all_results.extend(results);
            }
            Err(e) => {
                eprintln!("Download failed for {}: {}", nzb_path.display(), e);
                if !cli.keep_partial {
                    eprintln!("Note: Partial files may remain. Use --keep-partial to explicitly keep them.");
                }
            }
        }
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

