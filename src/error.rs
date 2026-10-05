//! Domain-specific error types for dl-nzb.

use std::path::PathBuf;
use thiserror::Error;

/// Coarse classification of an error, for front ends that react differently to
/// each (the app pauses its queue and offers Settings on `Auth`/`Dns`/`Connect`,
/// shows the shortfall on `DiskFull`, and so on). See [`DlNzbError::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The configuration is incomplete or invalid (no server, bad path, ...).
    Config,
    /// The server rejected the username or password.
    Auth,
    /// The server name could not be resolved.
    Dns,
    /// The TCP connection could not be established, or every connection was lost.
    Connect,
    /// The TLS handshake failed (certificate, protocol), or the port doesn't
    /// speak TLS at all.
    Tls,
    /// The server did not answer in time.
    Timeout,
    /// The server answered with something we do not understand.
    Protocol,
    /// The NZB file could not be read or parsed.
    Nzb,
    /// A local file operation failed.
    Io,
    /// Not enough free space for the job (checked before downloading).
    DiskFull,
}

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

    /// A job's failure carried as data (its [`ErrorKind`] and one-sentence
    /// message from the `JobSummary`), for front ends that turn a finished
    /// job back into an error.
    #[error("{message}")]
    Job { kind: ErrorKind, message: String },
}

impl DlNzbError {
    /// Classify this error for front ends (see [`ErrorKind`]). `Tls` is
    /// only a failed handshake or certificate, or a server that answered the
    /// handshake in plain text (SSL on a plaintext port); a connection the
    /// server reset or closed (also mid-handshake) is `Connect`.
    pub fn kind(&self) -> ErrorKind {
        match self {
            DlNzbError::Nzb(_) => ErrorKind::Nzb,
            DlNzbError::Config(_) => ErrorKind::Config,
            DlNzbError::Nntp(e) => match e {
                NntpError::AuthFailed(_) => ErrorKind::Auth,
                NntpError::DnsFailed { .. } => ErrorKind::Dns,
                NntpError::ConnectionFailed { source, .. }
                    if network_failure(source) == Some(NetworkFailure::TimedOut) =>
                {
                    ErrorKind::Timeout
                }
                NntpError::ConnectionFailed { .. } | NntpError::ConnectionLost { .. } => {
                    ErrorKind::Connect
                }
                NntpError::TlsError { .. } | NntpError::NotTls { .. } => ErrorKind::Tls,
                NntpError::Timeout { .. } => ErrorKind::Timeout,
                NntpError::ProtocolError(_)
                | NntpError::GroupNotFound { .. }
                | NntpError::YencDecode(_)
                | NntpError::UnhealthyConnection => ErrorKind::Protocol,
            },
            DlNzbError::Download(e) => match e {
                DownloadError::PoolExhausted => ErrorKind::Connect,
                DownloadError::DiskFull { .. } => ErrorKind::DiskFull,
            },
            DlNzbError::PostProcessing(_) | DlNzbError::SerdeJson(_) => ErrorKind::Io,
            DlNzbError::Io(e) => match network_failure(e) {
                Some(NetworkFailure::TimedOut) => ErrorKind::Timeout,
                Some(_) => ErrorKind::Connect,
                None if e.kind() == std::io::ErrorKind::StorageFull => ErrorKind::DiskFull,
                None => ErrorKind::Io,
            },
            DlNzbError::NativeTls(_) => ErrorKind::Tls,
            DlNzbError::Job { kind, .. } => *kind,
        }
    }

    /// One or two short plain-English sentences describing the error, for a
    /// UI status line. Never credentials (auth failures carry only the
    /// response code), and never a library's own wording: that stays in
    /// `Display`, for logs.
    pub fn user_message(&self) -> String {
        match self {
            DlNzbError::Nntp(e) => nntp_message(e),
            DlNzbError::NativeTls(_) => "Could not make a secure connection to the server.".into(),
            DlNzbError::Download(DownloadError::PoolExhausted) => {
                "Could not get a connection to the server.".into()
            }
            // Already a full sentence of our own.
            DlNzbError::Download(e @ DownloadError::DiskFull { .. }) => e.to_string(),
            DlNzbError::Job { message, .. } => message.clone(),
            DlNzbError::Nzb(e) => match e {
                NzbError::ParseError(_) => "The NZB file could not be read.".into(),
                NzbError::Empty => "The NZB file lists no files.".into(),
                NzbError::NoValidArticles => {
                    "No article in the NZB file has a valid message ID.".into()
                }
            },
            DlNzbError::Config(e) => match e {
                ConfigError::ParseError(_) => "The settings could not be read.".into(),
                ConfigError::Invalid { field, .. } => format!("The setting {field} is not valid."),
                ConfigError::NoServer => "No server is set up.".into(),
                ConfigError::NoCredentials => "No username or password is set up.".into(),
                ConfigError::InvalidConnections { .. } => {
                    "The number of connections must be between 1 and 100.".into()
                }
                ConfigError::InvalidPath { path, .. } => {
                    format!("The folder {} can't be used.", path.display())
                }
            },
            DlNzbError::PostProcessing(PostProcessingError::Par2(_)) => {
                "PAR2 could not finish checking the files.".into()
            }
            DlNzbError::Io(e) => io_message(e),
            DlNzbError::SerdeJson(_) => "Saved job data could not be read.".into(),
        }
    }
}

fn nntp_message(e: &NntpError) -> String {
    match e {
        NntpError::AuthFailed(_) => "The server rejected the username or password.".into(),
        NntpError::DnsFailed { server, .. } => format!("Could not find the server {server}."),
        NntpError::ConnectionFailed {
            server,
            port,
            source,
        } => match network_failure(source) {
            Some(NetworkFailure::Refused) => {
                format!("{server} refused the connection on port {port}.")
            }
            Some(NetworkFailure::TimedOut) => format!("{server} did not respond in time."),
            Some(NetworkFailure::Unreachable) => {
                format!("Could not reach {server}. Check the internet connection.")
            }
            Some(NetworkFailure::Lost) => format!("The connection to {server} was lost."),
            None => format!("Could not connect to {server} on port {port}."),
        },
        NntpError::ConnectionLost { server, .. } => {
            format!("The connection to {server} was lost.")
        }
        NntpError::TlsError { server, .. } => {
            format!("Could not make a secure connection to {server}.")
        }
        NntpError::NotTls { .. } => "The server doesn't use SSL on this port.".into(),
        NntpError::Timeout { .. } => "The server did not respond in time.".into(),
        NntpError::ProtocolError(_)
        | NntpError::GroupNotFound { .. }
        | NntpError::YencDecode(_)
        | NntpError::UnhealthyConnection => "The server sent an unexpected response.".into(),
    }
}

fn io_message(e: &std::io::Error) -> String {
    use std::io::ErrorKind as K;
    match network_failure(e) {
        Some(NetworkFailure::TimedOut) => return "The server did not respond in time.".into(),
        Some(NetworkFailure::Refused) => return "The server refused the connection.".into(),
        Some(NetworkFailure::Unreachable) => {
            return "Could not reach the server. Check the internet connection.".into()
        }
        Some(NetworkFailure::Lost) => return "The connection to the server was lost.".into(),
        None => {}
    }
    match e.kind() {
        K::StorageFull => "The disk is full.".into(),
        K::NotFound => "A file or folder could not be found.".into(),
        K::PermissionDenied => "dl-nzb is not allowed to use a file or folder it needs.".into(),
        K::ReadOnlyFilesystem => "The download folder is on a read-only disk.".into(),
        _ => "A file could not be read or written.".into(),
    }
}

/// Why writing into the job folder failed, as a plain clause for a warning
/// that names the file itself ("Could not extract X: the folder isn't
/// writable."). The I/O error's own wording ("Permission denied (os error
/// 13)") is for the logs only.
pub(crate) fn write_problem(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::PermissionDenied => "the folder isn't writable",
        K::StorageFull => "the disk is full",
        K::ReadOnlyFilesystem => "the disk is read-only",
        K::NotFound => "a folder it needs is gone",
        K::FileTooLarge => "a file is too large for the disk",
        K::InvalidFilename => "a file name isn't allowed on this disk",
        _ => "files could not be written",
    }
}

/// How a socket operation failed, by its I/O error kind (`None`: not a
/// network failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkFailure {
    /// Nothing listens there, or the server turned us away.
    Refused,
    TimedOut,
    /// No route to the server: offline, or the network is down.
    Unreachable,
    /// An established connection was reset or closed.
    Lost,
}

fn network_failure(e: &std::io::Error) -> Option<NetworkFailure> {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::ConnectionRefused => Some(NetworkFailure::Refused),
        K::TimedOut => Some(NetworkFailure::TimedOut),
        K::HostUnreachable | K::NetworkUnreachable | K::NetworkDown | K::AddrNotAvailable => {
            Some(NetworkFailure::Unreachable)
        }
        K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe | K::NotConnected => {
            Some(NetworkFailure::Lost)
        }
        _ => None,
    }
}

#[derive(Error, Debug)]
pub enum NzbError {
    #[error("Failed to parse NZB file: {0}")]
    ParseError(String),

    #[error("the NZB lists no files")]
    Empty,

    #[error("no article in the NZB has a valid message id")]
    NoValidArticles,
}

#[derive(Error, Debug)]
pub enum NntpError {
    #[error("Connection failed to {server}:{port}: {source}")]
    ConnectionFailed {
        server: String,
        port: u16,
        source: std::io::Error,
    },

    #[error("Could not resolve {server}: {source}")]
    DnsFailed {
        server: String,
        source: std::io::Error,
    },

    #[error("Connection timeout after {seconds}s")]
    Timeout { seconds: u64 },

    /// The TLS handshake failed on its own terms: certificate, protocol or
    /// version. A connection the server reset or closed meanwhile is
    /// [`ConnectionLost`](Self::ConnectionLost) instead.
    #[error("TLS handshake with {server} failed: {detail}")]
    TlsError { server: String, detail: String },

    /// SSL is on, but the server answered the TLS handshake in plain text
    /// (an NNTP greeting): the port is not an SSL one.
    #[error("{server}:{port} answered the TLS handshake in plain text")]
    NotTls { server: String, port: u16 },

    /// The server reset or closed a connection that was up (including one
    /// still setting up TLS or logging in).
    #[error("Connection to {server} lost: {source}")]
    ConnectionLost {
        server: String,
        source: std::io::Error,
    },

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

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
    #[error("Connection pool exhausted")]
    PoolExhausted,

    #[error(
        "Not enough free space: this download needs about {} but only {} is free ({} short).",
        crate::util::format_bytes(*needed),
        crate::util::format_bytes(*available),
        crate::util::format_bytes(needed.saturating_sub(*available))
    )]
    DiskFull { needed: u64, available: u64 },
}

#[derive(Error, Debug)]
pub enum PostProcessingError {
    #[error("PAR2 error: {0}")]
    Par2(#[from] par2_rs::Par2Error),
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
    fn error_kinds_classify_connection_failures() {
        let auth: DlNzbError = NntpError::AuthFailed("481".into()).into();
        assert_eq!(auth.kind(), ErrorKind::Auth);
        let dns: DlNzbError = NntpError::DnsFailed {
            server: "nope.invalid".into(),
            source: std::io::Error::other("no such host"),
        }
        .into();
        assert_eq!(dns.kind(), ErrorKind::Dns);
        let full: DlNzbError = DownloadError::DiskFull {
            needed: 5_000_000_000,
            available: 1_000_000_000,
        }
        .into();
        assert_eq!(full.kind(), ErrorKind::DiskFull);
        assert!(full.user_message().contains("4.0 GB short"));
    }

    /// Resets and closed connections are `Connect` (never `Tls`, `Io` or
    /// `Protocol`), and every message is plain sentences: no library wording
    /// in parentheses ("Couldn't Connect Securely ... (connection closed via
    /// error)" was one).
    #[test]
    fn errors_read_as_plain_sentences_of_the_right_kind() {
        use std::io::{Error as IoError, ErrorKind as Io};
        let server = || "news.example.com".to_string();
        let cases: Vec<(DlNzbError, ErrorKind)> = vec![
            (
                NntpError::ConnectionLost {
                    server: server(),
                    source: IoError::new(Io::ConnectionReset, "connection closed via error"),
                }
                .into(),
                ErrorKind::Connect,
            ),
            (
                NntpError::ConnectionFailed {
                    server: server(),
                    port: 563,
                    source: IoError::new(Io::ConnectionRefused, "Connection refused (os error 61)"),
                }
                .into(),
                ErrorKind::Connect,
            ),
            (
                NntpError::ConnectionFailed {
                    server: server(),
                    port: 563,
                    source: IoError::new(Io::TimedOut, "timed out (os error 60)"),
                }
                .into(),
                ErrorKind::Timeout,
            ),
            (
                NntpError::ConnectionFailed {
                    server: server(),
                    port: 563,
                    source: IoError::new(Io::NetworkUnreachable, "unreachable (os error 51)"),
                }
                .into(),
                ErrorKind::Connect,
            ),
            (
                NntpError::TlsError {
                    server: server(),
                    detail: "certificate not trusted (-9807)".into(),
                }
                .into(),
                ErrorKind::Tls,
            ),
            (
                NntpError::ProtocolError("Server greeting failed: 400 (go away)".into()).into(),
                ErrorKind::Protocol,
            ),
            (NntpError::UnhealthyConnection.into(), ErrorKind::Protocol),
            (
                NntpError::NotTls {
                    server: server(),
                    port: 119,
                }
                .into(),
                ErrorKind::Tls,
            ),
            (
                IoError::new(
                    Io::ConnectionReset,
                    "Connection reset by peer (os error 54)",
                )
                .into(),
                ErrorKind::Connect,
            ),
            (
                IoError::new(Io::BrokenPipe, "Broken pipe (os error 32)").into(),
                ErrorKind::Connect,
            ),
            (
                IoError::new(Io::NotFound, "No such file or directory (os error 2)").into(),
                ErrorKind::Io,
            ),
            (
                IoError::new(Io::PermissionDenied, "Permission denied (os error 13)").into(),
                ErrorKind::Io,
            ),
            (
                NzbError::ParseError("bad (xml)".into()).into(),
                ErrorKind::Nzb,
            ),
            (NzbError::Empty.into(), ErrorKind::Nzb),
            (
                ConfigError::InvalidConnections { count: 0 }.into(),
                ErrorKind::Config,
            ),
            (
                ConfigError::ParseError("expected `=` (line 3)".into()).into(),
                ErrorKind::Config,
            ),
            (DownloadError::PoolExhausted.into(), ErrorKind::Connect),
        ];
        for (error, kind) in cases {
            assert_eq!(error.kind(), kind, "{error}");
            let message = error.user_message();
            assert!(
                !message.contains('(') && !message.contains(')') && message.ends_with('.'),
                "{message}"
            );
        }
        let lost: DlNzbError = NntpError::ConnectionLost {
            server: server(),
            source: IoError::new(Io::UnexpectedEof, "closed"),
        }
        .into();
        assert_eq!(
            lost.user_message(),
            "The connection to news.example.com was lost."
        );
    }

    #[test]
    fn test_error_conversion() {
        let nzb_err = NzbError::ParseError("oops".into());
        let dl_err: DlNzbError = nzb_err.into();
        assert!(matches!(dl_err, DlNzbError::Nzb(_)));
    }
}
