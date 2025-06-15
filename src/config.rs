use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use anyhow::{Result, anyhow};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsenetConfig {
    pub server: String,
    pub username: String,
    pub password: String,
    pub port: u16,
    pub ssl: bool,
    pub connections: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub usenet: UsenetConfig,
    pub download_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub memory: MemoryConfig,
    pub post_processing: PostProcessingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Maximum number of segments to keep in memory before flushing to disk
    pub max_segments_in_memory: usize,
    /// Whether to flush each segment immediately to disk (lower memory usage)
    pub stream_to_disk: bool,
    /// Buffer size for file I/O operations
    pub io_buffer_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostProcessingConfig {
    /// Automatically repair files using PAR2 when segments fail
    pub auto_par2_repair: bool,
    /// Automatically extract RAR archives after download
    pub auto_extract_rar: bool,
    /// Automatically extract ZIP archives after download
    pub auto_extract_zip: bool,
    /// Delete archive files after successful extraction
    pub delete_archives_after_extract: bool,
    /// Delete PAR2 files after successful repair
    pub delete_par2_after_repair: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            usenet: UsenetConfig {
                server: "your.usenet.server".to_string(),
                username: "your_username".to_string(),
                password: "your_password".to_string(),
                port: 119,
                ssl: false,
                connections: 10,
            },
            download_dir: PathBuf::from("downloads"),
            temp_dir: PathBuf::from("temp"),
            memory: MemoryConfig {
                max_segments_in_memory: 100, // Reasonable default for most systems
                stream_to_disk: true, // Enable streaming by default for lower memory usage
                io_buffer_size: 1024 * 1024, // 1MB buffer for better I/O performance
            },
            post_processing: PostProcessingConfig {
                auto_par2_repair: true, // Enable automatic PAR2 repair by default
                auto_extract_rar: true, // Enable automatic RAR extraction
                auto_extract_zip: true, // Enable automatic ZIP extraction
                delete_archives_after_extract: false, // Keep archives by default for safety
                delete_par2_after_repair: false, // Keep PAR2 files by default for safety
            },
        }
    }
}

impl Config {
    pub fn load_or_create() -> Result<Self> {
        // Try current directory first, then fall back to system config dir
        let config_path = if PathBuf::from("config.toml").exists() {
            PathBuf::from("config.toml")
        } else {
            let config_dir = dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("dl-nzb");
            std::fs::create_dir_all(&config_dir)?;
            config_dir.join("config.toml")
        };

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let mut config: Config = toml::from_str(&content).unwrap_or_else(|_| {
                // If config is malformed, use defaults and merge what we can
                println!("Warning: Config file has errors, using defaults where needed");
                Config::default()
            });

            // If no download directory specified, use current directory
            if config.download_dir == PathBuf::from("downloads") && !PathBuf::from("downloads").exists() {
                config.download_dir = PathBuf::from(".");
            }

            // Validate that credentials are not placeholder values
            if config.usenet.server == "your.usenet.server"
                || config.usenet.username == "your_username"
                || config.usenet.password == "your_password" {
                return Err(anyhow!("Please edit {} and add your Usenet server credentials", config_path.display()));
            }

            Ok(config)
        } else {
            // No config file exists, create one with current directory as download location
            let mut config = Config::default();
            config.download_dir = PathBuf::from("."); // Download to current directory by default

            let content = toml::to_string_pretty(&config)?;
            std::fs::write(&config_path, content)?;
            println!("Created default config at: {}", config_path.display());
            println!("Downloads will be saved to current directory by default.");
            println!("Please edit the config file and add your Usenet server credentials before using the tool.");
            Err(anyhow!("Config file created. Please edit it with your credentials and try again."))
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.download_dir)?;
        std::fs::create_dir_all(&self.temp_dir)?;
        Ok(())
    }
}
