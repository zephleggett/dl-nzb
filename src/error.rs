//! Domain-specific error types for dl-nzb
//!
//! This module provides structured error handling with proper error chains
//! and context preservation.

use std::path::PathBuf;
use thiserror::Error;

/// Top-level error type for the dl-nzb application
#[derive(Error, Debug)]
pub enum DlNzbError {
    #[error("NZB error: {0}")]
    Nzb(#[from] NzbError),

    #[error("NNTP error: {0}")]
    Nntp(#[from] NntpError),

    #[error("Configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("Download error: {0}")]
    Download(#[from] DownloadError),

    #[error("Post-processing error: {0}")]
    PostProcessing(#[from] PostProcessingError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TLS error: {0}")]
    NativeTls(#[from] native_tls::Error),
}

/// NZB parsing and validation errors
#[derive(Error, Debug)]
pub enum NzbError {
    #[error("Failed to parse NZB file: {0}")]
    ParseError(String),

    #[error("Invalid NZB file at {path}: {reason}")]
    InvalidFile { path: PathBuf, reason: String },

    #[error("NZB file not found: {0}")]
    NotFound(PathBuf),

    #[error("No files found in NZB")]
    EmptyNzb,

    #[error("Missing required field: {0}")]
    MissingField(String),

    #[error("Invalid segment: {0}")]
    InvalidSegment(String),
}

/// NNTP protocol and connection errors
#[derive(Error, Debug)]
pub enum NntpError {
    #[error("Connection failed to {server}:{port}: {source}")]
    ConnectionFailed {
        server: String,
        port: u16,
        source: std::io::Error,
    },

    #[error("Connection timeout after {seconds}s")]
    Timeout { seconds: u64 },

    #[error("TLS handshake failed: {0}")]
    TlsError(String),

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("Server response error: {code} {message}")]
    ServerError { code: u16, message: String },

    #[error("Article not found: {message_id}")]
    ArticleNotFound { message_id: String },

    #[error("Group not found: {group}")]
    GroupNotFound { group: String },

    #[error("YEnc decode error: {0}")]
    YencDecode(String),

    #[error("Connection unhealthy")]
    UnhealthyConnection,
}

/// Configuration validation errors
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Configuration file not found: {0}")]
    NotFound(PathBuf),

    #[error("Failed to parse configuration: {0}")]
    ParseError(String),

    #[error("Invalid configuration: {field}: {reason}")]
    Invalid { field: String, reason: String },

    #[error("Server not configured")]
    NoServer,

    #[error("Credentials not configured")]
    NoCredentials,

    #[error("Invalid connection count: {count} (must be 1-100)")]
    InvalidConnections { count: u16 },

    #[error("Invalid path: {path}: {reason}")]
    InvalidPath { path: PathBuf, reason: String },

    #[error("Environment variable error: {0}")]
    EnvVar(#[from] std::env::VarError),
}

/// Download operation errors
#[derive(Error, Debug)]
pub enum DownloadError {
    #[error("Failed to download segment {number} of {total}: {reason}")]
    SegmentFailed {
        number: u32,
        total: u32,
        reason: String,
    },

    #[error("Failed to download file {filename}: {reason}")]
    FileFailed { filename: String, reason: String },

    #[error("Insufficient segments: {available}/{required} available")]
    InsufficientSegments { available: usize, required: usize },

    #[error("Connection pool exhausted")]
    PoolExhausted,

    #[error("Download cancelled")]
    Cancelled,

    #[error("Write error for {path}: {source}")]
    WriteError {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Post-processing errors (PAR2, RAR extraction)
#[derive(Error, Debug)]
pub enum PostProcessingError {
    #[error("PAR2 repair failed: {0}")]
    Par2Failed(String),

    #[error("PAR2 file not found")]
    Par2NotFound,

    #[error("RAR extraction failed for {archive}: {reason}")]
    RarFailed { archive: PathBuf, reason: String },

    #[error("No RAR archives found")]
    NoRarArchives,

    #[error("Archive corrupted: {0}")]
    CorruptedArchive(PathBuf),

    #[error("Extraction tool not found: {tool}")]
    ToolNotFound { tool: String },
}

/// Result type alias using DlNzbError
pub type Result<T> = std::result::Result<T, DlNzbError>;

/// Helper trait for adding context to errors
pub trait ErrorContext<T> {
    fn context(self, msg: impl Into<String>) -> Result<T>;
    fn with_context<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String;
}

impl<T, E> ErrorContext<T> for std::result::Result<T, E>
where
    E: Into<DlNzbError>,
{
    fn context(self, msg: impl Into<String>) -> Result<T> {
        self.map_err(|e| {
            let error: DlNzbError = e.into();
            // Log the context
            tracing::error!("{}: {}", msg.into(), error);
            error
        })
    }

    fn with_context<F>(self, f: F) -> Result<T>
    where
        F: FnOnce() -> String,
    {
        self.map_err(|e| {
            let error: DlNzbError = e.into();
            tracing::error!("{}: {}", f(), error);
            error
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = NzbError::NotFound(PathBuf::from("/test/file.nzb"));
        assert_eq!(err.to_string(), "NZB file not found: /test/file.nzb");
    }

    #[test]
    fn test_error_conversion() {
        let nzb_err = NzbError::EmptyNzb;
        let dl_err: DlNzbError = nzb_err.into();
        assert!(matches!(dl_err, DlNzbError::Nzb(_)));
    }
}
