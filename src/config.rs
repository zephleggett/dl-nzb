use config::{Config as ConfigLib, Environment, File};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::env;

use crate::error::{ConfigError, DlNzbError};

type Result<T> = std::result::Result<T, DlNzbError>;

/// Expand tilde (~) in paths to the actual home directory
fn expand_tilde(path: &Path) -> PathBuf {
    if let Some(path_str) = path.to_str() {
        if path_str.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(&path_str[2..]);
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub usenet: UsenetConfig,
    
    #[serde(default)]
    pub download: DownloadConfig,
    
    #[serde(default)]
    pub memory: MemoryConfig,
    
    #[serde(default)]
    pub post_processing: PostProcessingConfig,
    
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadConfig {
    pub dir: PathBuf,
    pub temp_dir: PathBuf,
    pub create_subfolders: bool,
    pub overwrite_existing: bool,
    pub user_agent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub max_segments_in_memory: usize,
    pub stream_to_disk: bool,
    pub io_buffer_size: usize,
    pub max_concurrent_files: usize,
    pub segment_retry_buffer: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostProcessingConfig {
    pub auto_par2_repair: bool,
    pub auto_extract_rar: bool,
    pub delete_rar_after_extract: bool,
    pub delete_par2_after_repair: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub file: Option<PathBuf>,
    pub format: String,
}

// Default implementations
impl Default for UsenetConfig {
    fn default() -> Self {
        Self {
            server: "news.example.com".to_string(),
            port: 119,
            username: String::new(),
            password: String::new(),
            ssl: false,
            verify_ssl_certs: true,
            connections: 20,
            timeout: 30,
            retry_attempts: 3,
            retry_delay: 1000,
        }
    }
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("downloads"),
            temp_dir: PathBuf::from(".dl-nzb-temp"),
            create_subfolders: true,
            overwrite_existing: false,
            user_agent: format!("dl-nzb/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_segments_in_memory: 100,
            stream_to_disk: true,
            io_buffer_size: 4 * 1024 * 1024, // 4MB
            max_concurrent_files: 5,
            segment_retry_buffer: 50,
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

impl Default for Config {
    fn default() -> Self {
        Self {
            usenet: UsenetConfig::default(),
            download: DownloadConfig::default(),
            memory: MemoryConfig::default(),
            post_processing: PostProcessingConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

/// Configuration builder for flexible configuration loading
pub struct ConfigBuilder {
    config: ConfigLib,
}

impl ConfigBuilder {
    /// Create a new configuration builder
    pub fn new() -> Self {
        Self {
            config: ConfigLib::builder().build().unwrap(),
        }
    }

    /// Add a configuration file
    pub fn add_file<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.config = ConfigLib::builder()
            .add_source(self.config)
            .add_source(File::from(path.as_ref()))
            .build()
            .unwrap();
        self
    }

    /// Add environment variables with prefix
    pub fn add_env_prefix(mut self, prefix: &str) -> Self {
        self.config = ConfigLib::builder()
            .add_source(self.config)
            .add_source(
                Environment::with_prefix(prefix)
                    .separator("_")
                    .try_parsing(true)
            )
            .build()
            .unwrap();
        self
    }

    /// Build and validate the configuration
    pub fn build(self) -> Result<Config> {
        let mut config: Config = self
            .config
            .try_deserialize()
            .map_err(|e| ConfigError::ParseError(e.to_string()))?;

        // Expand tilde in paths
        config.download.dir = expand_tilde(&config.download.dir);
        config.download.temp_dir = expand_tilde(&config.download.temp_dir);
        if let Some(log_file) = config.logging.file.as_ref() {
            config.logging.file = Some(expand_tilde(log_file));
        }

        config.validate()?;
        Ok(config)
    }
}

impl Config {
    /// Get the standard config file path
    pub fn config_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir().ok_or_else(|| {
            ConfigError::Invalid {
                field: "config_dir".to_string(),
                reason: "Could not determine config directory".to_string(),
            }
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
                tracing::debug!("Config file not found, creating default at: {}", standard_config.display());

                // Ensure directory exists
                if let Some(parent) = standard_config.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                // Create default config file
                Self::create_sample(&standard_config)?;

                println!("📝 Created default configuration at: {}", standard_config.display());
                println!("⚙️  Please edit this file with your Usenet server credentials.");
                println!();
            }
            tracing::debug!("Loaded configuration from: {}", standard_config.display());
            standard_config
        };

        let mut builder = ConfigBuilder::new();

        // Load config file
        builder = builder.add_file(&config_path);

        // Add environment variables (can override file settings)
        builder = builder.add_env_prefix("DL_NZB");

        builder.build()
    }

    /// Create a sample configuration file
    pub fn create_sample<P: AsRef<Path>>(path: P) -> Result<()> {
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

{}

# Additional configuration examples:
#
# [usenet]
# server = "news.usenetserver.com"
# port = 563  # Use 563 for SSL
# ssl = true
# connections = 50  # Increase for faster downloads
#
# [download]
# dir = "/path/to/downloads"
# create_subfolders = true  # Create subfolders based on NZB name
#
# [memory]
# max_segments_in_memory = 200  # Increase if you have more RAM
# io_buffer_size = 8388608  # 8MB buffers for better performance
#
# [post_processing]
# auto_par2_repair = true              # Verify and repair files with PAR2
# auto_extract_rar = true              # Extract RAR archives (using native library)
# delete_rar_after_extract = true      # Save disk space after extraction
# delete_par2_after_repair = true      # Clean up PAR2 files after repair
"#,
            content
        );

        std::fs::write(path, commented_content)?;
        Ok(())
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        // Validate Usenet settings
        if self.usenet.server.is_empty() || self.usenet.server == "news.example.com" {
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

        // Validate memory settings
        if self.memory.io_buffer_size < 1024 {
            return Err(ConfigError::Invalid {
                field: "io_buffer_size".to_string(),
                reason: "Must be at least 1KB".to_string(),
            }
            .into());
        }

        if self.memory.max_segments_in_memory == 0 {
            return Err(ConfigError::Invalid {
                field: "max_segments_in_memory".to_string(),
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

    /// Ensure required directories exist
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.download.dir)?;
        std::fs::create_dir_all(&self.download.temp_dir)?;
        
        if let Some(log_file) = &self.logging.file {
            if let Some(parent) = log_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
        }
        
        Ok(())
    }

    /// Apply command-line overrides
    pub fn apply_overrides(&mut self, overrides: ConfigOverrides) {
        if let Some(server) = overrides.server {
            self.usenet.server = server;
        }
        if let Some(port) = overrides.port {
            self.usenet.port = port;
        }
        if let Some(connections) = overrides.connections {
            self.usenet.connections = connections;
        }
        if let Some(ssl) = overrides.ssl {
            self.usenet.ssl = ssl;
        }
        if let Some(dir) = overrides.download_dir {
            self.download.dir = dir;
        }
        if let Some(level) = overrides.log_level {
            self.logging.level = level;
        }
    }
}

/// Command-line configuration overrides
#[derive(Debug, Default)]
pub struct ConfigOverrides {
    pub server: Option<String>,
    pub port: Option<u16>,
    pub connections: Option<u16>,
    pub ssl: Option<bool>,
    pub download_dir: Option<PathBuf>,
    pub log_level: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.usenet.connections, 20);
        assert!(config.memory.stream_to_disk);
    }

    #[test]
    fn test_config_validation() {
        let mut config = Config::default();
        assert!(config.validate().is_err());

        config.usenet.server = "news.example.org".to_string();
        config.usenet.username = "user".to_string();
        config.usenet.password = "pass".to_string();
        assert!(config.validate().is_ok());
    }
}