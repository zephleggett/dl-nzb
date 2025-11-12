use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Fast NZB downloader for Usenet
#[derive(Parser, Debug)]
#[command(name = "dl-nzb")]
#[command(version, about, long_about = None)]
#[command(after_help = "EXAMPLES:
    Download an NZB file:
        dl-nzb file.nzb

    Download to specific directory:
        dl-nzb -o /downloads file.nzb

    List contents without downloading:
        dl-nzb -l file.nzb

    Show configuration:
        dl-nzb config

    Test connection:
        dl-nzb test

For advanced options, edit ~/.config/dl-nzb/config.toml")]
pub struct Cli {
    /// NZB files to download
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Output directory
    #[arg(short, long, value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// List contents without downloading
    #[arg(short, long)]
    pub list: bool,

    /// Quiet mode (errors only)
    #[arg(short, long)]
    pub quiet: bool,

    /// Verbose output (-vv for debug)
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// JSON output for scripting
    #[arg(long)]
    pub json: bool,

    /// Config file path
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Force operation (skip confirmations)
    #[arg(short, long)]
    pub force: bool,

    /// Subcommands
    #[command(subcommand)]
    pub command: Option<Commands>,

    // Hidden flags kept for backwards compatibility with scripts
    // These will be removed in future versions
    #[arg(short = 'c', long = "connections", hide = true)]
    pub connections: Option<u16>,

    #[arg(long = "output-dir", hide = true)]
    pub output_dir: Option<PathBuf>,

    #[arg(long = "no-directories", hide = true)]
    pub no_directories: bool,

    #[arg(long = "keep-partial", hide = true)]
    pub keep_partial: bool,

    #[arg(long = "no-par2", hide = true)]
    pub no_par2: bool,

    #[arg(long = "no-extract-rar", hide = true)]
    pub no_extract_rar: bool,

    #[arg(long = "delete-rar-after-extract", hide = true)]
    pub delete_rar_after_extract: bool,

    #[arg(long = "delete-par2", hide = true)]
    pub delete_par2: bool,

    #[arg(long = "print-names", hide = true)]
    pub print_names: bool,

    #[arg(long = "server", hide = true)]
    pub server: Option<String>,

    #[arg(long = "port", hide = true)]
    pub port: Option<u16>,

    #[arg(long = "ssl", hide = true)]
    pub ssl: Option<bool>,

    #[arg(short = 'u', long = "user", hide = true)]
    pub username: Option<String>,

    #[arg(short = 'p', long = "password", hide = true)]
    pub password: Option<String>,

    #[arg(long = "memory-limit", hide = true)]
    pub memory_limit: Option<usize>,

    #[arg(long = "buffer-size", hide = true)]
    pub buffer_size: Option<usize>,

    #[arg(long = "max-concurrent-files", hide = true)]
    pub max_concurrent_files: Option<usize>,

    #[arg(long = "log-level", hide = true)]
    pub log_level: Option<String>,

    #[arg(long = "log-file", hide = true)]
    pub log_file: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Test connection to Usenet server
    Test,

    /// Show configuration
    Config,

    /// Show version information
    Version,
}

impl Cli {
    /// Parse arguments and handle special cases
    pub fn parse_and_validate() -> Self {
        let mut cli = Self::parse();

        // Handle backwards compatibility
        if cli.output_dir.is_some() && cli.output.is_none() {
            cli.output = cli.output_dir.clone();
            eprintln!("Warning: --output-dir is deprecated, use -o/--output instead");
        }

        // Handle password from stdin (kept for backwards compat)
        if cli.password.as_deref() == Some("-") {
            use std::io::{self, BufRead};
            let stdin = io::stdin();
            if let Ok(password) = stdin.lock().lines().next().unwrap_or(Ok(String::new())) {
                cli.password = Some(password);
            }
        }

        // Print deprecation warnings for hidden flags if used
        if cli.connections.is_some() {
            eprintln!("Warning: --connections is deprecated, set 'connections' in config file");
        }
        if cli.no_par2 {
            eprintln!("Warning: --no-par2 is deprecated, set 'auto_par2_repair = false' in config file");
        }
        if cli.no_extract_rar {
            eprintln!("Warning: --no-extract-rar is deprecated, set 'auto_extract_rar = false' in config file");
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
            download_dir: self.output.clone(),
            log_level: self.log_level.clone(),
        }
    }

    /// Check if deprecated flags are used
    pub fn has_deprecated_flags(&self) -> bool {
        self.connections.is_some()
            || self.output_dir.is_some()
            || self.no_directories
            || self.keep_partial
            || self.no_par2
            || self.no_extract_rar
            || self.delete_rar_after_extract
            || self.delete_par2
            || self.print_names
            || self.server.is_some()
            || self.port.is_some()
            || self.ssl.is_some()
            || self.username.is_some()
            || self.password.is_some()
            || self.memory_limit.is_some()
            || self.buffer_size.is_some()
            || self.max_concurrent_files.is_some()
    }
}

/// CLI-specific error messages
pub mod messages {
    pub const NO_FILES: &str = "No NZB files specified. Use 'dl-nzb --help' for usage information.";
}