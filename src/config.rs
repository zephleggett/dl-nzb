use serde::{Deserialize, Serialize};
use std::env;
use std::path::{Path, PathBuf};

use crate::error::{ConfigError, DlNzbError};

type Result<T> = std::result::Result<T, DlNzbError>;

/// Expand tilde (~) in paths to the actual home directory
fn expand_tilde(path: &Path) -> PathBuf {
    if let Some(path_str) = path.to_str() {
        if let Some(stripped) = path_str.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(stripped);
            }
        } else if path_str == "~" {
            if let Some(home) = dirs::home_dir() {
                return home;
            }
        }
    }
    path.to_path_buf()
}

/// Main configuration structure with builder pattern support
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub usenet: UsenetConfig,

    #[serde(default)]
    pub download: DownloadConfig,

    #[serde(default)]
    pub post_processing: PostProcessingConfig,

    #[serde(default)]
    pub logging: LoggingConfig,

    #[serde(default)]
    pub tuning: TuningConfig,

    #[serde(default)]
    pub notifications: NotificationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationConfig {
    /// Ring the terminal bell when a download finishes *successfully* and the
    /// run lasted at least `notify_min_seconds`. Default `false`: silence is the
    /// polite default (matching cargo/uv/bun). The bell is also suppressed when
    /// stderr is not a terminal (pipes/CI).
    #[serde(default)]
    pub notify_on_complete: bool,

    /// Minimum run length (seconds) before the bell rings, so quick downloads
    /// don't startle. Only applies when `notify_on_complete` is true.
    #[serde(default = "default_notify_min_seconds")]
    pub notify_min_seconds: u64,
}

fn default_notify_min_seconds() -> u64 {
    30
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            notify_on_complete: false,
            notify_min_seconds: default_notify_min_seconds(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct UsenetConfig {
    pub server: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub ssl: bool,
    pub verify_ssl_certs: bool,
    pub connections: u16,
    pub timeout: u64, // seconds
    pub retry_attempts: u8,
    pub retry_delay: u64, // milliseconds
}

// Custom Debug implementation to hide sensitive data
impl std::fmt::Debug for UsenetConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsenetConfig")
            .field("server", &self.server)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<REDACTED>")
            .field("ssl", &self.ssl)
            .field("verify_ssl_certs", &self.verify_ssl_certs)
            .field("connections", &self.connections)
            .field("timeout", &self.timeout)
            .field("retry_attempts", &self.retry_attempts)
            .field("retry_delay", &self.retry_delay)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadConfig {
    pub dir: PathBuf,
    pub create_subfolders: bool,
    #[serde(default)]
    pub force_redownload: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostProcessingConfig {
    pub auto_par2_repair: bool,
    pub auto_extract_rar: bool,
    pub delete_rar_after_extract: bool,
    pub delete_par2_after_repair: bool,
    pub deobfuscate_file_names: bool,
    /// Download every PAR2 recovery volume up front. Default `false`: recovery
    /// volumes are deferred and only fetched when a data segment is actually
    /// missing or corrupt, saving the (often 10-30%) recovery bytes on the
    /// common case where the download completes intact. (SABnzbd `enable_all_par`.)
    #[serde(default)]
    pub download_all_par2: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub file: Option<PathBuf>,
    pub format: String,
}

/// Performance tuning parameters
/// These are advanced settings that typically don't need adjustment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningConfig {
    /// Number of `BODY` requests each connection keeps in flight (a continuous
    /// sliding window) to hide round-trip latency. Small values (2-8) are ideal:
    /// the pipeline stays continuously full while retry granularity and memory
    /// stay tiny. Replaces the old per-batch `pipeline_size` knob (which is now
    /// ignored if present in an old config).
    #[serde(default = "default_pipeline_depth")]
    pub pipeline_depth: usize,
    /// Maximum retries for a yEnc decode (CRC/size) failure before the article
    /// is given up. Transient wire failures are retried separately, uncounted.
    #[serde(default = "default_decode_retry_cap")]
    pub decode_retry_cap: u8,
    /// Maximum concurrent connection creation attempts
    pub max_concurrent_connections: usize,
    /// File size threshold (bytes) above which to show progress during RAR extraction
    pub large_file_threshold: u64,
    /// `fsync` each finished file at finalize. Default `false`: PAR2 verifies
    /// integrity and a crash just means re-download, so we skip hundreds of
    /// fsync barriers (a large wall-clock win on big sets / slow or network
    /// disks). Set true only if you need crash-durability of partial downloads.
    #[serde(default)]
    pub fsync_on_finalize: bool,
}

fn default_pipeline_depth() -> usize {
    4
}

fn default_decode_retry_cap() -> u8 {
    3
}

// Default implementations
impl Default for UsenetConfig {
    fn default() -> Self {
        Self {
            server: String::new(),
            port: 563, // Default SSL port
            username: String::new(),
            password: String::new(),
            ssl: true, // Default to SSL
            verify_ssl_certs: true,
            connections: 30,   // Most providers allow 30-50; saturates a fast link
            timeout: 30,       // Reduced from 45s
            retry_attempts: 2, // Faster failover
            retry_delay: 500,  // Quick retries
        }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("downloads"),
            create_subfolders: true,
            force_redownload: false,
        }
    }
}

impl Default for PostProcessingConfig {
    fn default() -> Self {
        Self {
            auto_par2_repair: true,
            auto_extract_rar: true,
            delete_rar_after_extract: false,
            delete_par2_after_repair: false,
            deobfuscate_file_names: true,
            download_all_par2: false,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            file: None,
            format: "pretty".to_string(),
        }
    }
}

impl Default for TuningConfig {
    fn default() -> Self {
        Self {
            pipeline_depth: default_pipeline_depth(), // in-flight BODY requests per connection
            decode_retry_cap: default_decode_retry_cap(), // bounded yEnc-decode retries
            max_concurrent_connections: 30,           // Concurrent connection creation limit
            large_file_threshold: 10 * 1024 * 1024,   // 10MB for progress monitoring
            fsync_on_finalize: false,                 // skip fsync; PAR2 verifies integrity
        }
    }
}

/// Load configuration from environment variables
fn load_env_overrides(mut config: Config) -> Config {
    // Override with DL_NZB_ prefixed environment variables
    if let Ok(val) = env::var("DL_NZB_USENET_SERVER") {
        config.usenet.server = val;
    }
    if let Ok(val) = env::var("DL_NZB_USENET_PORT") {
        if let Ok(port) = val.parse() {
            config.usenet.port = port;
        }
    }
    if let Ok(val) = env::var("DL_NZB_USENET_USERNAME") {
        config.usenet.username = val;
    }
    if let Ok(val) = env::var("DL_NZB_USENET_PASSWORD") {
        config.usenet.password = val;
    }
    if let Ok(val) = env::var("DL_NZB_USENET_SSL") {
        if let Ok(ssl) = val.parse() {
            config.usenet.ssl = ssl;
        }
    }
    if let Ok(val) = env::var("DL_NZB_USENET_CONNECTIONS") {
        if let Ok(connections) = val.parse() {
            config.usenet.connections = connections;
        }
    }
    if let Ok(val) = env::var("DL_NZB_DOWNLOAD_DIR") {
        config.download.dir = PathBuf::from(val);
    }
    if let Ok(val) = env::var("DL_NZB_NOTIFY_ON_COMPLETE") {
        if let Ok(b) = val.parse() {
            config.notifications.notify_on_complete = b;
        }
    }

    config
}

impl Config {
    /// Get the standard config file path
    pub fn config_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir().ok_or_else(|| ConfigError::Invalid {
            field: "config_dir".to_string(),
            reason: "Could not determine config directory".to_string(),
        })?;
        Ok(config_dir.join("dl-nzb").join("config.toml"))
    }

    /// Load configuration from local or standard location
    pub fn load() -> Result<Self> {
        let local_config = PathBuf::from("dl-nzb.toml");
        let standard_config = Self::config_path()?;

        // Check for local config first (for development/testing)
        let config_path = if local_config.exists() {
            tracing::debug!("Loaded configuration from: {}", local_config.display());
            local_config
        } else {
            // Create standard config file with defaults if it doesn't exist
            if !standard_config.exists() {
                tracing::debug!(
                    "Config file not found, creating default at: {}",
                    standard_config.display()
                );

                // Ensure directory exists
                if let Some(parent) = standard_config.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                // Create default config file
                Self::create_sample(&standard_config)?;

                println!(
                    "📝 Created default configuration at: {}",
                    standard_config.display()
                );
                println!("⚙️  Please edit this file with your Usenet server credentials.");
                println!();
            }
            tracing::debug!("Loaded configuration from: {}", standard_config.display());
            standard_config
        };

        // Load and parse TOML file
        let content = std::fs::read_to_string(&config_path)?;
        let mut config: Config = toml::from_str(&content)
            .map_err(|e| ConfigError::ParseError(format!("Failed to parse config: {}", e)))?;

        // Apply environment variable overrides
        config = load_env_overrides(config);

        // Expand tilde in paths
        config.download.dir = expand_tilde(&config.download.dir);
        if let Some(log_file) = config.logging.file.as_ref() {
            config.logging.file = Some(expand_tilde(log_file));
        }

        config.validate()?;
        Ok(config)
    }

    /// Create a sample configuration file
    fn create_sample<P: AsRef<Path>>(path: P) -> Result<()> {
        let sample = Self::default();
        let content = toml::to_string_pretty(&sample)
            .map_err(|e| ConfigError::ParseError(format!("Failed to serialize config: {}", e)))?;

        // Add helpful comments
        let commented_content = format!(
            r#"# dl-nzb Configuration File
#
# This file configures the dl-nzb Usenet downloader.
# All settings can be overridden via environment variables with the DL_NZB_ prefix.
# For example: DL_NZB_USENET_SERVER=news.example.com
#
# REQUIRED: Set your Usenet server details below

{}

# Configuration Guide:
#
# [usenet]
# server       - Your Usenet provider's server address (REQUIRED)
# port         - Usually 563 for SSL, 119 for non-SSL
# username     - Your Usenet account username (REQUIRED)
# password     - Your Usenet account password (REQUIRED)
# ssl          - Use encrypted SSL/TLS connection (recommended)
# connections  - Number of connections (30-50 typical, check your provider's limit)
# timeout      - Connection timeout in seconds
# retry_attempts - Number of times to retry failed downloads
#
# [download]
# dir               - Where to save downloads
# create_subfolders - Create a subfolder for each NZB file
#
# [post_processing]
# auto_par2_repair        - Automatically verify/repair with PAR2 files
# auto_extract_rar        - Automatically extract RAR archives
# delete_rar_after_extract - Delete RAR files after successful extraction
# delete_par2_after_repair - Delete PAR2 files after successful repair
# deobfuscate_file_names  - Rename obfuscated files to meaningful names
# download_all_par2       - Download all PAR2 recovery up front (default: false = fetch on demand)
#
# [notifications]
# notify_on_complete - Ring the terminal bell when a long download succeeds (default: false)
# notify_min_seconds - Minimum run length before the bell rings (default: 30)
"#,
            content
        );

        std::fs::write(path, commented_content)?;
        Ok(())
    }

    /// Validate basic configuration (always run)
    /// Does not require server credentials - use validate_for_download() before downloading
    pub fn validate(&self) -> Result<()> {
        // Validate connection count only if server is configured
        if !self.usenet.server.is_empty()
            && (self.usenet.connections == 0 || self.usenet.connections > 100)
        {
            return Err(ConfigError::InvalidConnections {
                count: self.usenet.connections,
            }
            .into());
        }

        if self.tuning.pipeline_depth == 0 {
            return Err(ConfigError::Invalid {
                field: "pipeline_depth".to_string(),
                reason: "Must be at least 1".to_string(),
            }
            .into());
        }

        if self.tuning.max_concurrent_connections == 0 {
            return Err(ConfigError::Invalid {
                field: "max_concurrent_connections".to_string(),
                reason: "Must be at least 1".to_string(),
            }
            .into());
        }

        // Validate paths
        if self.download.dir.as_os_str().is_empty() {
            return Err(ConfigError::InvalidPath {
                path: self.download.dir.clone(),
                reason: "Download directory not specified".to_string(),
            }
            .into());
        }

        Ok(())
    }

    /// Validate configuration for download operations
    /// Call this before starting any downloads to ensure server credentials are set
    pub fn validate_for_download(&self) -> Result<()> {
        if self.usenet.server.is_empty() {
            return Err(ConfigError::NoServer.into());
        }

        if self.usenet.username.is_empty() || self.usenet.password.is_empty() {
            return Err(ConfigError::NoCredentials.into());
        }

        if self.usenet.connections == 0 || self.usenet.connections > 100 {
            return Err(ConfigError::InvalidConnections {
                count: self.usenet.connections,
            }
            .into());
        }

        Ok(())
    }

    /// Ensure required directories exist
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.download.dir)?;

        if let Some(log_file) = &self.logging.file {
            if let Some(parent) = log_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }

        Ok(())
    }

    /// Display the configuration as TOML
    pub fn display_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| {
            ConfigError::ParseError(format!("Failed to serialize config: {}", e)).into()
        })
    }

    /// Apply command-line overrides
    pub fn apply_overrides(&mut self, overrides: ConfigOverrides) {
        if let Some(dir) = overrides.download_dir {
            self.download.dir = dir;
        }
    }
}

/// Command-line configuration overrides
#[derive(Debug, Default)]
pub struct ConfigOverrides {
    pub download_dir: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.usenet.connections, 30);
        assert!(!config.tuning.fsync_on_finalize);
    }

    #[test]
    fn test_config_validation() {
        let config = Config::default();
        // Basic validation should pass without server credentials
        assert!(config.validate().is_ok());

        // But download validation should fail without credentials
        assert!(config.validate_for_download().is_err());
    }

    #[test]
    fn test_config_validation_for_download() {
        let mut config = Config::default();

        // Set required fields for download
        config.usenet.server = "news.example.org".to_string();
        config.usenet.username = "user".to_string();
        config.usenet.password = "pass".to_string();

        assert!(config.validate().is_ok());
        assert!(config.validate_for_download().is_ok());
    }
}
