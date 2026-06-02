//! Domain-specific error types for dl-nzb.

use std::path::PathBuf;
use thiserror::Error;

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

    #[error("JSON error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

#[derive(Error, Debug)]
pub enum NzbError {
    #[error("Failed to parse NZB file: {0}")]
    ParseError(String),
}

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

    #[error("Group not found: {group}")]
    GroupNotFound { group: String },

    #[error("YEnc decode error: {0}")]
    YencDecode(String),

    #[error("Connection unhealthy")]
    UnhealthyConnection,
}

#[derive(Error, Debug)]
pub enum ConfigError {
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
}

#[derive(Error, Debug)]
pub enum DownloadError {
    #[error("Insufficient segments: {available}/{required} available")]
    InsufficientSegments { available: usize, required: usize },

    #[error("Connection pool exhausted")]
    PoolExhausted,
}

#[derive(Error, Debug)]
pub enum PostProcessingError {
    #[error("PAR2 error: {0}")]
    Par2(#[from] par2_rs::Par2Error),

    #[error("Failed to rename file from {from} to {to}: {source}")]
    FileRenameError {
        from: PathBuf,
        to: PathBuf,
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, DlNzbError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = NzbError::ParseError("bad xml".into());
        assert_eq!(err.to_string(), "Failed to parse NZB file: bad xml");
    }

    #[test]
    fn test_error_conversion() {
        let nzb_err = NzbError::ParseError("oops".into());
        let dl_err: DlNzbError = nzb_err.into();
        assert!(matches!(dl_err, DlNzbError::Nzb(_)));
    }
}
