use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// When to colourise output.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ColorWhen {
    /// Colour only when the output stream is a terminal (honours `NO_COLOR`).
    Auto,
    /// Always colourise.
    Always,
    /// Never colourise.
    Never,
}

impl From<ColorWhen> for crate::ui::style::ColorChoice {
    fn from(w: ColorWhen) -> Self {
        match w {
            ColorWhen::Auto => Self::Auto,
            ColorWhen::Always => Self::Always,
            ColorWhen::Never => Self::Never,
        }
    }
}

/// Full text rendered for `--version` (a single version surface; `-V` shows the
/// bare version line).
const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\n",
    "A fast, lightweight NZB downloader\n\n",
    "Features:\n",
    "  • Parallel segment downloads with per-segment retry\n",
    "  • yEnc decoder with =ypart offsets and CRC32 verification\n",
    "  • Built-in PAR2 repair (par2-rs, pure Rust + SIMD)\n",
    "  • Automatic RAR extraction\n",
    "  • JSON output for scripting",
);

/// Fast NZB downloader for Usenet
#[derive(Parser, Debug)]
#[command(name = "dl-nzb")]
#[command(version, long_version = LONG_VERSION, about, long_about = None)]
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

    /// Colourise output: auto (default), always, or never
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = ColorWhen::Auto)]
    pub color: ColorWhen,

    /// Force re-download (overwrite existing files)
    #[arg(short, long)]
    pub force: bool,

    /// Subcommands
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Test connection to Usenet server
    Test,

    /// Show configuration
    Config,
}

impl Cli {
    /// Parse arguments and handle special cases
    pub fn parse_and_validate() -> Self {
        let mut cli = Self::parse();

        // Adjust verbosity based on quiet flag
        if cli.quiet {
            cli.verbose = 0;
        }

        cli
    }

    /// Get the effective log level
    pub fn get_log_level(&self) -> &str {
        match self.verbose {
            0 if self.quiet => "error",
            0 => "info",
            1 => "debug",
            _ => "trace",
        }
    }

    /// Get configuration overrides from CLI arguments
    pub fn get_config_overrides(&self) -> crate::config::ConfigOverrides {
        crate::config::ConfigOverrides {
            download_dir: self.output.clone(),
        }
    }
}
