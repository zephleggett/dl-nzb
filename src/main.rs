use human_bytes::human_bytes;
use tracing_subscriber::EnvFilter;

use dl_nzb::{
    cli::{Cli, Commands},
    config::Config,
    download::{Downloader, Nzb},
    processing::PostProcessor,
    nntp::AsyncNntpConnection,
    error::{DlNzbError, ConfigError},
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
    let filter = EnvFilter::try_new(cli.get_log_level())
        .unwrap_or_else(|_| EnvFilter::new("info"));

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

            // Test connection using async NNTP
            match AsyncNntpConnection::connect(&test_config).await {
                Ok(mut conn) => {
                    println!("✅ Successfully connected to {}", test_config.server);
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
                let toml = toml::to_string_pretty(&config)
                    .map_err(|e| ConfigError::ParseError(format!("Failed to serialize config: {}", e)))?;
                println!("{}", toml);
                println!("{}", "─".repeat(60));
            } else {
                println!("Configuration file does not exist yet.");
                println!("Run any command to auto-create it with default values.");
            }

            Ok(())
        }

        Commands::History { show, clear, remove } => {
            if *show {
                println!("Download history:");
                // TODO: Implement history
            } else if *clear {
                println!("Clearing download history...");
                // TODO: Implement history clear
            } else if let Some(id) = remove {
                println!("Removing history entry: {}", id);
                // TODO: Implement history remove
            }
            Ok(())
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
            let file_type = if filename.to_lowercase().ends_with(".par2") { "PAR2" } else { "DATA" };
            println!("  [{:4}] {} ({})", file_type, filename, human_bytes(size as f64));
        }
    }
    
    Ok(())
}

/// Handle download mode
async fn handle_download_mode(cli: &Cli, mut config: Config) -> Result<()> {
    // Apply CLI settings to config
    if let Some(_limit) = cli.limit_rate {
        // TODO: Implement bandwidth limiting
        tracing::warn!("Bandwidth limiting not yet implemented");
    }

    if cli.no_directories {
        config.download.create_subfolders = false;
    }

    if cli.overwrite {
        config.download.overwrite_existing = true;
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

    // Create downloader
    let downloader = Downloader::new(config.clone()).await?;

    // Pre-warm connection pool silently
    downloader.warm_up().await?;

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
        match downloader.download_nzb(&nzb, download_config.clone()).await {
            Ok((results, progress_bar)) => {
                // Finish the download progress bar
                progress_bar.finish_and_clear();

                if cli.print_names {
                    for result in &results {
                        println!("{}", result.path.display());
                    }
                }

                // Post-processing - create new progress bars
                if config.post_processing.auto_par2_repair || config.post_processing.auto_extract_rar {
                    let processor = PostProcessor::new(download_config.post_processing.clone());
                    if let Err(e) = processor.process_downloads(&results).await {
                        eprintln!("Post-processing error: {}", e);
                    }
                }

                all_results.extend(results);
            }
            Err(e) => {
                eprintln!("Download failed for {}: {}", nzb_path.display(), e);
                if !cli.keep_partial {
                    // TODO: Cleanup partial files
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parsing() {
        let args = vec!["dl-nzb", "test.nzb", "-o", "/tmp", "-c", "50"];
        let cli = Cli::try_parse_from(args).unwrap();
        assert_eq!(cli.files.len(), 1);
        assert_eq!(cli.connections, Some(50));
    }
}