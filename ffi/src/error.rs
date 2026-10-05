//! The one error type that crosses the boundary.

use std::fmt;

use dl_nzb::{DlNzbError, ErrorKind};

/// An engine error: its kind (the variant) and one plain sentence for the user
/// (the message, never credentials). Flat, so Swift sees
/// `EngineError.Auth(message:)` and so on, one case per [`ErrorKind`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Error)]
#[uniffi(flat_error)]
pub enum EngineError {
    Config(String),
    Auth(String),
    Dns(String),
    Connect(String),
    Tls(String),
    Timeout(String),
    Protocol(String),
    Nzb(String),
    Io(String),
    DiskFull(String),
}

impl EngineError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        let message = message.into();
        match kind {
            ErrorKind::Config => Self::Config(message),
            ErrorKind::Auth => Self::Auth(message),
            ErrorKind::Dns => Self::Dns(message),
            ErrorKind::Connect => Self::Connect(message),
            ErrorKind::Tls => Self::Tls(message),
            ErrorKind::Timeout => Self::Timeout(message),
            ErrorKind::Protocol => Self::Protocol(message),
            ErrorKind::Nzb => Self::Nzb(message),
            ErrorKind::Io => Self::Io(message),
            ErrorKind::DiskFull => Self::DiskFull(message),
        }
    }

    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Config(_) => ErrorKind::Config,
            Self::Auth(_) => ErrorKind::Auth,
            Self::Dns(_) => ErrorKind::Dns,
            Self::Connect(_) => ErrorKind::Connect,
            Self::Tls(_) => ErrorKind::Tls,
            Self::Timeout(_) => ErrorKind::Timeout,
            Self::Protocol(_) => ErrorKind::Protocol,
            Self::Nzb(_) => ErrorKind::Nzb,
            Self::Io(_) => ErrorKind::Io,
            Self::DiskFull(_) => ErrorKind::DiskFull,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Config(m)
            | Self::Auth(m)
            | Self::Dns(m)
            | Self::Connect(m)
            | Self::Tls(m)
            | Self::Timeout(m)
            | Self::Protocol(m)
            | Self::Nzb(m)
            | Self::Io(m)
            | Self::DiskFull(m) => m,
        }
    }

    /// A task on the engine's runtime panicked or was cancelled.
    pub(crate) fn internal(e: tokio::task::JoinError) -> Self {
        tracing::error!("engine task failed: {e}");
        Self::Io("The engine stopped because of an internal error.".to_string())
    }
}

/// The message alone: a flat error's Swift `message` is this `Display`.
impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for EngineError {}

impl From<DlNzbError> for EngineError {
    fn from(e: DlNzbError) -> Self {
        Self::new(e.kind(), e.user_message())
    }
}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        DlNzbError::from(e).into()
    }
}
