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
    /// Cap on the engine's total download speed, in bytes per second; `None`
    /// is unlimited. In the file: an integer, or a string with a K, M or G
    /// suffix (1024-based, as curl's `--limit-rate`), e.g. `"500K"`, `"10M"`;
    /// absent or 0 is unlimited. The CLI's `--limit-rate` overrides it, and
    /// `Engine::set_speed_limit` changes it live.
    #[serde(
        default,
        with = "speed_limit_serde",
        skip_serializing_if = "Option::is_none"
    )]
    pub speed_limit: Option<u64>,
}

/// Parse a speed such as `"10M"`, `"500K"`, `"1.5M"` or `"250000"` into bytes
/// per second. Suffixes K, M, G and T are 1024-based (as curl's
/// `--limit-rate`) and may be followed by `B`, `iB` or `/s`
/// (`"10MB/s"`, `"10MiB"`). 0 means unlimited.
pub fn parse_speed(text: &str) -> std::result::Result<u64, String> {
    let invalid = || {
        format!("invalid speed {text:?}: use bytes per second, or a number with K, M or G (e.g. 500K, 10M)")
    };
    let mut s = text.trim().to_ascii_lowercase();
    if let Some(rest) = s.strip_suffix("/s") {
        s = rest.trim_end().to_string();
    }
    if let Some(rest) = s.strip_suffix("ib").or_else(|| s.strip_suffix('b')) {
        s = rest.to_string();
    }
    let (number, multiplier) = match s.chars().last() {
        Some('k') => (&s[..s.len() - 1], 1u64 << 10),
        Some('m') => (&s[..s.len() - 1], 1 << 20),
        Some('g') => (&s[..s.len() - 1], 1 << 30),
        Some('t') => (&s[..s.len() - 1], 1 << 40),
        _ => (s.as_str(), 1),
    };
    let number = number.trim();
    if number.is_empty() || number.starts_with(['-', '+']) {
        return Err(invalid());
    }
    if let Ok(n) = number.parse::<u64>() {
        return n.checked_mul(multiplier).ok_or_else(invalid);
    }
    let value: f64 = number.parse().map_err(|_| invalid())?;
    let bytes = value * multiplier as f64;
    if !bytes.is_finite() || bytes < 0.0 || bytes >= u64::MAX as f64 {
        return Err(invalid());
    }
    Ok(bytes.round() as u64)
}

/// Format bytes per second the way [`parse_speed`] reads it: `"10M"`,
/// `"500K"`, or a plain number when no suffix divides it exactly.
pub fn format_speed(bytes_per_sec: u64) -> String {
    for (suffix, unit) in [
        ("T", 1u64 << 40),
        ("G", 1 << 30),
        ("M", 1 << 20),
        ("K", 1 << 10),
    ] {
        if bytes_per_sec >= unit && bytes_per_sec % unit == 0 {
            return format!("{}{suffix}", bytes_per_sec / unit);
        }
    }
    bytes_per_sec.to_string()
}

/// `download.speed_limit`: an integer or a suffixed string in, a suffixed
/// string out; 0 reads as unlimited.
mod speed_limit_serde {
    use serde::de::{self, Visitor};
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S: Serializer>(value: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(n) => s.serialize_str(&super::format_speed(*n)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        struct SpeedVisitor;

        impl<'de> Visitor<'de> for SpeedVisitor {
            type Value = Option<u64>;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("bytes per second, or a string such as \"10M\" or \"500K\"")
            }

            fn visit_u64<E: de::Error>(self, n: u64) -> Result<Self::Value, E> {
                Ok((n > 0).then_some(n))
            }

            fn visit_i64<E: de::Error>(self, n: i64) -> Result<Self::Value, E> {
                u64::try_from(n)
                    .map_err(|_| E::custom("a speed limit can't be negative"))
                    .and_then(|n| self.visit_u64(n))
            }

            fn visit_f64<E: de::Error>(self, n: f64) -> Result<Self::Value, E> {
                if n.is_finite() && n >= 0.0 && n < u64::MAX as f64 {
                    self.visit_u64(n.round() as u64)
                } else {
                    Err(E::custom("invalid speed limit"))
                }
            }

            fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
                super::parse_speed(s)
                    .map_err(E::custom)
                    .and_then(|n| self.visit_u64(n))
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(None)
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(None)
            }

            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
                d.deserialize_any(self)
            }
        }

        d.deserialize_any(SpeedVisitor)
    }
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
            speed_limit: None,
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

    /// Load configuration from local or standard location, creating the
    /// standard file with defaults if neither exists.
    pub fn load() -> Result<Self> {
        Self::load_or_create().map(|(config, _)| config)
    }

    /// [`load`](Self::load), also returning the path of the default file if
    /// this call had to create it (so the CLI can tell the user to edit it).
    pub fn load_or_create() -> Result<(Self, Option<PathBuf>)> {
        let local_config = PathBuf::from("dl-nzb.toml");
        let standard_config = Self::config_path()?;
        let mut created = None;

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
                created = Some(standard_config.clone());
            }
            tracing::debug!("Loaded configuration from: {}", standard_config.display());
            standard_config
        };

        // Load and parse TOML file
        let content = std::fs::read_to_string(&config_path)?;
        let mut config = Self::from_toml_str(&content)?;

        // Apply environment variable overrides (which may bring their own `~`)
        config = load_env_overrides(config);
        config.download.dir = expand_tilde(&config.download.dir);

        config.validate()?;
        Ok((config, created))
    }

    /// Parse a configuration file's contents (no environment overrides, no
    /// validation), expanding `~` in paths.
    pub fn from_toml_str(content: &str) -> Result<Self> {
        let mut config: Config = toml::from_str(content)
            .map_err(|e| ConfigError::ParseError(format!("Failed to parse config: {}", e)))?;
        config.download.dir = expand_tilde(&config.download.dir);
        if let Some(log_file) = config.logging.file.as_ref() {
            config.logging.file = Some(expand_tilde(log_file));
        }
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
# speed_limit       - Cap the download speed, e.g. "10M" or "500K" (bytes/s;
#                     K/M/G are 1024-based). Absent or 0 = unlimited.
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

    /// The configuration as TOML, for showing: a password reads `********`.
    pub fn display_toml(&self) -> Result<String> {
        let mut shown = self.clone();
        if !shown.usenet.password.is_empty() {
            shown.usenet.password = "********".to_string();
        }
        toml::to_string_pretty(&shown).map_err(|e| {
            ConfigError::ParseError(format!("Failed to serialize config: {}", e)).into()
        })
    }

    /// Apply command-line overrides
    pub fn apply_overrides(&mut self, overrides: ConfigOverrides) {
        if let Some(dir) = overrides.download_dir {
            self.download.dir = dir;
        }
        if let Some(rate) = overrides.speed_limit {
            self.download.speed_limit = (rate > 0).then_some(rate);
        }
    }
}

/// Command-line configuration overrides
#[derive(Debug, Default)]
pub struct ConfigOverrides {
    pub download_dir: Option<PathBuf>,
    /// Bytes per second; `Some(0)` removes the configured limit.
    pub speed_limit: Option<u64>,
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
    fn speeds_parse_like_curl_limit_rate() {
        assert_eq!(parse_speed("250000"), Ok(250_000));
        assert_eq!(parse_speed("500K"), Ok(500 * 1024));
        assert_eq!(parse_speed("10M"), Ok(10 * 1024 * 1024));
        assert_eq!(parse_speed("10m"), Ok(10 * 1024 * 1024));
        assert_eq!(parse_speed("1.5M"), Ok(1024 * 1024 * 3 / 2));
        assert_eq!(parse_speed("2G"), Ok(2 << 30));
        assert_eq!(parse_speed(" 10MB/s "), Ok(10 << 20));
        assert_eq!(parse_speed("10MiB"), Ok(10 << 20));
        assert_eq!(parse_speed("0"), Ok(0));
        for bad in ["", "M", "fast", "-1M", "10X", "1e400", "99999999999T"] {
            assert!(parse_speed(bad).is_err(), "{bad:?} should not parse");
        }
        for n in [1, 1000, 1024, 500 * 1024, 10 << 20, 3 << 30, 1_500_000] {
            assert_eq!(parse_speed(&format_speed(n)), Ok(n));
        }
        assert_eq!(format_speed(10 << 20), "10M");
    }

    #[test]
    fn speed_limit_reads_strings_and_integers_and_round_trips() {
        let read = |download: &str| {
            let toml = format!("[download]\ndir = \"d\"\ncreate_subfolders = true\n{download}");
            Config::from_toml_str(&toml).map(|c| c.download.speed_limit)
        };
        assert_eq!(read("").unwrap(), None);
        assert_eq!(read("speed_limit = 0").unwrap(), None);
        assert_eq!(read("speed_limit = \"0\"").unwrap(), None);
        assert_eq!(read("speed_limit = 1048576").unwrap(), Some(1 << 20));
        assert_eq!(read("speed_limit = \"10M\"").unwrap(), Some(10 << 20));
        assert_eq!(read("speed_limit = \"500K\"").unwrap(), Some(500 << 10));
        assert!(read("speed_limit = \"lots\"").is_err());
        assert!(read("speed_limit = -5").is_err());

        let mut config = Config::default();
        assert!(!config.display_toml().unwrap().contains("speed_limit"));
        config.download.speed_limit = Some(10 << 20);
        let text = config.display_toml().unwrap();
        assert!(text.contains("speed_limit = \"10M\""), "{text}");
        let back = Config::from_toml_str(&text).unwrap();
        assert_eq!(back.download.speed_limit, Some(10 << 20));

        config.apply_overrides(ConfigOverrides {
            speed_limit: Some(0),
            ..Default::default()
        });
        assert_eq!(config.download.speed_limit, None);
    }

    #[test]
    fn display_toml_hides_the_password() {
        let mut config = Config::default();
        config.usenet.username = "zeph".to_string();
        config.usenet.password = "hunter2".to_string();
        let text = config.display_toml().unwrap();
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("password = \"********\""), "{text}");
        assert!(text.contains("username = \"zeph\""), "{text}");
        // The configuration itself is untouched.
        assert_eq!(config.usenet.password, "hunter2");
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
