use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// A high-performance NZB downloader for Usenet
#[derive(Parser, Debug)]
#[command(name = "dl-nzb")]
#[command(version, about, long_about = None)]
#[command(after_help = "EXAMPLES:
    # Download an NZB file
    dl-nzb file.nzb

    # Download with custom output directory
    dl-nzb file.nzb -o /path/to/downloads

    # Download with increased connections
    dl-nzb file.nzb -c 50

    # Download multiple NZB files
    dl-nzb file1.nzb file2.nzb file3.nzb

    # List contents without downloading
    dl-nzb -l file.nzb

    # Continue partial download
    dl-nzb -c file.nzb

    # Show current configuration
    dl-nzb config")]
pub struct Cli {
    /// NZB files to download
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    // Network Options
    /// Maximum number of simultaneous connections
    #[arg(short = 'c', long, value_name = "NUM")]
    pub connections: Option<u16>,

    /// Bandwidth limit in KB/s (0 = unlimited)
    #[arg(long = "limit-rate", value_name = "RATE")]
    pub limit_rate: Option<u64>,

    /// Connection timeout in seconds
    #[arg(long = "timeout", value_name = "SECS", default_value = "30")]
    pub timeout: u64,

    /// Number of retries for failed segments
    #[arg(long = "retries", value_name = "NUM", default_value = "3")]
    pub retries: u8,

    // Download Options
    /// Output directory
    #[arg(short = 'o', long = "output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// Continue partial downloads
    #[arg(short = 'C', long = "continue")]
    pub continue_download: bool,

    /// Overwrite existing files
    #[arg(long = "overwrite")]
    pub overwrite: bool,

    /// Don't create subdirectories
    #[arg(long = "no-directories")]
    pub no_directories: bool,

    /// Keep partial files on error
    #[arg(long = "keep-partial")]
    pub keep_partial: bool,

    // Post-processing Options
    /// Skip PAR2 verification and repair
    #[arg(long = "no-par2")]
    pub no_par2: bool,

    /// Skip RAR archive extraction
    #[arg(long = "no-extract-rar")]
    pub no_extract_rar: bool,

    /// Delete RAR archives after extraction
    #[arg(long = "delete-rar-after-extract")]
    pub delete_rar_after_extract: bool,

    /// Delete PAR2 files after successful repair
    #[arg(long = "delete-par2")]
    pub delete_par2: bool,

    // Information Options
    /// List NZB contents without downloading
    #[arg(short = 'l', long = "list")]
    pub list: bool,

    /// Show download progress (auto, bar, percent, quiet)
    #[arg(long = "progress", value_enum, default_value = "auto")]
    pub progress: ProgressType,

    /// Quiet mode (no output except errors)
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Verbose output
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Print downloaded file paths to stdout
    #[arg(long = "print-names")]
    pub print_names: bool,

    // Server Override Options
    /// Usenet server address (overrides config)
    #[arg(long = "server", value_name = "HOST")]
    pub server: Option<String>,

    /// Usenet server port (overrides config)
    #[arg(long = "port", value_name = "PORT")]
    pub port: Option<u16>,

    /// Use SSL/TLS connection (overrides config)
    #[arg(long = "ssl")]
    pub ssl: Option<bool>,

    /// Usenet username (overrides config)
    #[arg(short = 'u', long = "user", value_name = "USER")]
    pub username: Option<String>,

    /// Usenet password (overrides config, use - for stdin)
    #[arg(short = 'p', long = "password", value_name = "PASS")]
    pub password: Option<String>,

    // Advanced Options
    /// Memory limit for segment buffering (MB)
    #[arg(long = "memory-limit", value_name = "MB")]
    pub memory_limit: Option<usize>,

    /// I/O buffer size (KB)
    #[arg(long = "buffer-size", value_name = "KB", default_value = "4096")]
    pub buffer_size: usize,

    /// Maximum concurrent file downloads
    #[arg(long = "max-concurrent-files", value_name = "NUM", default_value = "5")]
    pub max_concurrent_files: usize,

    /// Log level (error, warn, info, debug, trace)
    #[arg(long = "log-level", value_name = "LEVEL")]
    pub log_level: Option<String>,

    /// Log file path
    #[arg(long = "log-file", value_name = "FILE")]
    pub log_file: Option<PathBuf>,

    /// Dry run (simulate download without fetching)
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Run self-tests
    #[arg(long = "self-test", hide = true)]
    pub self_test: bool,

    /// Subcommands for additional functionality
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Test connection to Usenet server
    Test {
        /// Test with specific server
        #[arg(long = "server")]
        server: Option<String>,
    },

    /// Show configuration file location and contents
    Config,

    /// Manage download history and cache
    History {
        /// Show download history
        #[arg(long = "show")]
        show: bool,

        /// Clear download history
        #[arg(long = "clear")]
        clear: bool,

        /// Remove specific entry
        #[arg(long = "remove", value_name = "ID")]
        remove: Option<String>,
    },

    /// Show version and build information
    Version {
        /// Show detailed version info
        #[arg(long = "detailed")]
        detailed: bool,
    },
}

#[derive(Debug, Clone, ValueEnum)]
pub enum ProgressType {
    /// Automatically choose progress display
    Auto,
    /// Show progress bar
    Bar,
    /// Show percentage only
    Percent,
    /// No progress output
    Quiet,
    /// Detailed progress with stats
    Verbose,
}

impl Cli {
    /// Parse arguments and handle special cases
    pub fn parse_and_validate() -> Self {
        let mut cli = Self::parse();

        // Handle password from stdin
        if cli.password.as_deref() == Some("-") {
            use std::io::{self, BufRead};
            let stdin = io::stdin();
            if let Ok(password) = stdin.lock().lines().next().unwrap_or(Ok(String::new())) {
                cli.password = Some(password);
            }
        }

        // Adjust verbosity based on quiet flag
        if cli.quiet {
            cli.verbose = 0;
        }

        cli
    }

    /// Get the effective log level
    pub fn get_log_level(&self) -> &str {
        if let Some(ref level) = self.log_level {
            level
        } else {
            match self.verbose {
                0 if self.quiet => "error",
                0 => "info",
                1 => "debug",
                _ => "trace",
            }
        }
    }

    /// Get configuration overrides from CLI arguments
    pub fn get_config_overrides(&self) -> crate::config::ConfigOverrides {
        crate::config::ConfigOverrides {
            server: self.server.clone(),
            port: self.port,
            connections: self.connections,
            ssl: self.ssl,
            download_dir: self.output_dir.clone(),
            log_level: self.log_level.clone(),
        }
    }
}

/// CLI-specific error messages
pub mod messages {
    pub const NO_FILES: &str = "No NZB files specified. Use 'dl-nzb --help' for usage information.";
}