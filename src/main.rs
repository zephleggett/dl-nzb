mod config;
mod nzb;
mod nntp;
mod downloader;
mod post_process;

use clap::{Parser, Subcommand};
use anyhow::Result;
use std::path::PathBuf;
use tracing_subscriber;
use regex;

use config::Config;
use nzb::Nzb;
use downloader::Downloader;
use post_process::PostProcessor;

#[derive(Parser)]
#[command(name = "dl-nzb")]
#[command(about = "A macOS CLI tool to parse and download NZB files from Usenet")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Download files from an NZB file
    Download {
        /// Path to the NZB file
        #[arg(value_name = "NZB_FILE")]
        nzb_file: PathBuf,

        /// Output directory (overrides config)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Number of concurrent connections (overrides config)
        #[arg(short, long)]
        connections: Option<u8>,

        /// Disable PAR2 repair
        #[arg(long)]
        no_par2: bool,

        /// Disable RAR extraction
        #[arg(long)]
        no_rar: bool,

        /// Disable ZIP extraction
        #[arg(long)]
        no_zip: bool,

        /// Delete archives after extraction
        #[arg(long)]
        delete_archives: bool,

        /// Delete PAR2 files after repair
        #[arg(long)]
        delete_par2: bool,

        /// Use SSL connection
        #[arg(long)]
        ssl: Option<bool>,

        /// Server port (overrides config)
        #[arg(long)]
        port: Option<u16>,
    },

    /// Parse and display information about an NZB file
    Info {
        /// Path to the NZB file
        #[arg(value_name = "NZB_FILE")]
        nzb_file: PathBuf,
    },

    /// Show current configuration
    Config,

    /// Test connection to Usenet server
    Test,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Download { nzb_file, output, connections, no_par2, no_rar, no_zip, delete_archives, delete_par2, ssl, port } => {
            download_command(nzb_file, output, connections, no_par2, no_rar, no_zip, delete_archives, delete_par2, ssl, port).await?;
        }
        Commands::Info { nzb_file } => {
            info_command(nzb_file)?;
        }
        Commands::Config => {
            config_command()?;
        }
        Commands::Test => {
            test_command().await?;
        }
    }

    Ok(())
}

async fn download_command(
    nzb_file: PathBuf,
    output: Option<PathBuf>,
    connections: Option<u8>,
    no_par2: bool,
    no_rar: bool,
    no_zip: bool,
    delete_archives: bool,
    delete_par2: bool,
    ssl: Option<bool>,
    port: Option<u16>,
) -> Result<()> {
    println!("📁 Loading NZB: {}", nzb_file.file_name().unwrap_or_default().to_string_lossy());
    let nzb = Nzb::from_file(&nzb_file)?;

    let mut config = Config::load_or_create()?;

    // Override config with command line options
    if let Some(conn) = connections {
        config.usenet.connections = conn;
    }
    if let Some(use_ssl) = ssl {
        config.usenet.ssl = use_ssl;
    }
    if let Some(server_port) = port {
        config.usenet.port = server_port;
    }
    if no_par2 {
        config.post_processing.auto_par2_repair = false;
    }
    if no_rar {
        config.post_processing.auto_extract_rar = false;
    }
    if no_zip {
        config.post_processing.auto_extract_zip = false;
    }
    if delete_archives {
        config.post_processing.delete_archives_after_extract = true;
    }
    if delete_par2 {
        config.post_processing.delete_par2_after_repair = true;
    }

    // Create a folder name from the NZB file
    let folder_name = generate_folder_name(&nzb_file, &nzb);
    let download_dir = if let Some(output_dir) = output {
        output_dir.join(&folder_name)
    } else {
        config.download_dir.join(&folder_name)
    };

    // Update config to use the specific download directory
    config.download_dir = download_dir;

    // Show brief info
    let main_files = nzb.get_main_files();
    let total_size_mb = nzb.total_size() as f64 / 1024.0 / 1024.0;

    println!("📁 Folder: {}", folder_name);
    if total_size_mb > 1024.0 {
        println!("📦 {} files • {:.1} GB • {} connections",
                main_files.len(), total_size_mb / 1024.0, config.usenet.connections);
    } else {
        println!("📦 {} files • {:.0} MB • {} connections",
                main_files.len(), total_size_mb, config.usenet.connections);
    }

    let downloader = Downloader::new(config.clone());
    let results = downloader.download_nzb(&nzb).await?;

    // Run post-processing if enabled
    let post_processor = PostProcessor::new(config.post_processing.clone());
    post_processor.process_downloads(&results).await?;

    // Show summary
    let total_size: u64 = results.iter().map(|r| r.size).sum();
    let total_time: f64 = results.iter().map(|r| r.download_time.as_secs_f64()).sum();
    let overall_speed = if total_time > 0.0 {
        (total_size as f64 / 1024.0 / 1024.0) / total_time
    } else {
        0.0
    };

    let failed_files = results.iter().filter(|r| r.segments_failed > 0).count();
    let success_files = results.len() - failed_files;

    println!("\n🎉 Download Complete!");
    if failed_files > 0 {
        println!("✅ {} files successful • ⚠️  {} files with errors", success_files, failed_files);
    } else {
        println!("✅ All {} files downloaded successfully", success_files);
    }

    let size_mb = total_size as f64 / 1024.0 / 1024.0;
    if size_mb > 1024.0 {
        println!("📊 {:.1} GB in {:.0}s • {:.1} MB/s average",
                size_mb / 1024.0, total_time, overall_speed);
    } else {
        println!("📊 {:.0} MB in {:.0}s • {:.1} MB/s average",
                size_mb, total_time, overall_speed);
    }

    Ok(())
}

fn info_command(nzb_file: PathBuf) -> Result<()> {
    println!("📁 Analyzing: {}", nzb_file.file_name().unwrap_or_default().to_string_lossy());
    let nzb = Nzb::from_file(&nzb_file)?;

    let main_files = nzb.get_main_files();
    let par2_files = nzb.get_par2_files();
    let total_size_mb = nzb.total_size() as f64 / 1024.0 / 1024.0;

    println!("\n📊 NZB Summary");
    if total_size_mb > 1024.0 {
        println!("📦 {} main files • {:.1} GB total", main_files.len(), total_size_mb / 1024.0);
    } else {
        println!("📦 {} main files • {:.0} MB total", main_files.len(), total_size_mb);
    }
    println!("🔧 {} PAR2 recovery files", par2_files.len());
    println!("📡 {} total segments", nzb.total_segments());

    if main_files.len() <= 10 {
        println!("\n📄 Main Files:");
        for (i, file) in main_files.iter().enumerate() {
            let filename = Nzb::get_filename_from_subject(&file.subject)
                .unwrap_or_else(|| format!("unknown_file_{}", file.date));

            let file_size: u64 = file.segments.segment.iter().map(|s| s.bytes).sum();
            let size_mb = file_size as f64 / 1024.0 / 1024.0;

            if size_mb > 1024.0 {
                println!("  {}. {} ({:.1} GB)", i + 1, filename, size_mb / 1024.0);
            } else {
                println!("  {}. {} ({:.0} MB)", i + 1, filename, size_mb);
            }
        }
    } else {
        println!("\n📄 Main Files: {} files (use download command to see details)", main_files.len());
    }

    Ok(())
}

fn config_command() -> Result<()> {
    let config = Config::load_or_create()?;

    println!("⚙️  Current Configuration");
    println!("🌐 Server: {}:{} (SSL: {})", config.usenet.server, config.usenet.port, config.usenet.ssl);
    println!("👤 User: {} ({})", config.usenet.username, "*".repeat(config.usenet.password.len()));
    println!("🔗 Connections: {}", config.usenet.connections);
    println!("📁 Downloads: {}", config.download_dir.display());
    println!("🔧 Auto-extract: RAR={}, ZIP={}", config.post_processing.auto_extract_rar, config.post_processing.auto_extract_zip);

    Ok(())
}

async fn test_command() -> Result<()> {
    let config = Config::load_or_create()?;

    println!("🔌 Testing connection to {}:{}...", config.usenet.server, config.usenet.port);

    let result = tokio::task::spawn_blocking(move || {
        let mut client = nntp::NntpClient::connect(config.usenet)?;
        client.quit()?;
        Ok::<(), anyhow::Error>(())
    }).await?;

    match result {
        Ok(()) => {
            println!("✅ Connection successful!");
        }
        Err(e) => {
            println!("❌ Connection failed: {}", e);
        }
    }

    Ok(())
}

fn generate_folder_name(nzb_file: &PathBuf, nzb: &Nzb) -> String {
    // Try to get a meaningful name from the NZB content
    let main_files = nzb.get_main_files();

    if let Some(first_file) = main_files.first() {
        if let Some(filename) = Nzb::get_filename_from_subject(&first_file.subject) {
            // Extract a clean name from the filename
            let clean_name = extract_clean_name(&filename);
            if !clean_name.is_empty() {
                return clean_name;
            }
        }
    }

    // Fallback to NZB filename without extension
    nzb_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download")
        .to_string()
}

fn extract_clean_name(filename: &str) -> String {
    // Remove common file extensions
    let name = filename
        .trim_end_matches(".mkv")
        .trim_end_matches(".mp4")
        .trim_end_matches(".avi")
        .trim_end_matches(".mov")
        .trim_end_matches(".wmv")
        .trim_end_matches(".flv")
        .trim_end_matches(".webm")
        .trim_end_matches(".m4v")
        .trim_end_matches(".nfo")
        .trim_end_matches(".txt")
        .trim_end_matches(".pdf")
        .trim_end_matches(".epub")
        .trim_end_matches(".zip")
        .trim_end_matches(".rar")
        .trim_end_matches(".7z")
        .trim_end_matches(".tar")
        .trim_end_matches(".gz");

    // Remove common patterns like .part01, .part001, etc.
    let re = regex::Regex::new(r"\.part\d+$").unwrap();
    let name = re.replace(name, "");

    // Replace problematic characters for folder names
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}
