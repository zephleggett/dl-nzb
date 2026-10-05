//! Connection pool for NNTP connections.
//!
//! Backed by `deadpool` for lifecycle management. There is no separate
//! warm-up: each download worker checks out its own connection, so they all
//! connect at once (at most `max_concurrent_connections` at a time) and each
//! starts work the moment its own is up; one slow connection holds up only
//! its worker.

use super::connection::AsyncNntpConnection;
use super::{ArticleOutcome, SpeedLimiter};
use crate::config::UsenetConfig;
use crate::error::{DlNzbError, DownloadError, NntpError};
use async_trait::async_trait;
use deadpool::managed::{Manager, Pool, PoolError, RecycleError, RecycleResult, TimeoutType};
use std::sync::Arc;
use std::time::Duration;

/// Connection manager for deadpool with rate-limited concurrent connect attempts.
pub struct NntpConnectionManager {
    config: Arc<UsenetConfig>,
    tls_connector: Option<Arc<tokio_native_tls::TlsConnector>>,
    creation_semaphore: Arc<tokio::sync::Semaphore>,
    speed_limiter: Option<Arc<SpeedLimiter>>,
}

impl NntpConnectionManager {
    pub fn new(
        config: UsenetConfig,
        max_concurrent_connections: usize,
    ) -> Result<Self, DlNzbError> {
        let tls_connector = if config.ssl {
            let mut tls_builder = native_tls::TlsConnector::builder();
            if !config.verify_ssl_certs {
                tls_builder.danger_accept_invalid_certs(true);
                tls_builder.danger_accept_invalid_hostnames(true);
            }
            let native_connector = tls_builder.build().map_err(|e| NntpError::TlsError {
                server: config.server.clone(),
                detail: e.to_string(),
            })?;
            Some(Arc::new(tokio_native_tls::TlsConnector::from(
                native_connector,
            )))
        } else {
            None
        };

        let limit = max_concurrent_connections.max(1);
        let creation_semaphore = Arc::new(tokio::sync::Semaphore::new(limit));

        Ok(Self {
            config: Arc::new(config),
            tls_connector,
            creation_semaphore,
            speed_limiter: None,
        })
    }
}

impl Manager for NntpConnectionManager {
    type Type = AsyncNntpConnection;
    type Error = DlNzbError;

    async fn create(&self) -> Result<AsyncNntpConnection, DlNzbError> {
        let _permit = self.creation_semaphore.acquire().await.map_err(|e| {
            DlNzbError::from(NntpError::ConnectionFailed {
                server: self.config.server.clone(),
                port: self.config.port,
                source: std::io::Error::other(e),
            })
        })?;

        AsyncNntpConnection::connect_limited(
            &self.config,
            self.tls_connector.clone(),
            self.speed_limiter.clone(),
        )
        .await
        .map_err(|e| {
            tracing::debug!("Failed to create NNTP connection: {}", e);
            e
        })
    }

    async fn recycle(
        &self,
        conn: &mut AsyncNntpConnection,
        _metrics: &deadpool::managed::Metrics,
    ) -> RecycleResult<DlNzbError> {
        // A connection handed back mid-exchange (a cancelled read) has unread
        // responses on the wire; never give it to another user.
        if !conn.is_clean() {
            return Err(RecycleError::Backend(NntpError::UnhealthyConnection.into()));
        }
        if conn.is_healthy().await {
            Ok(())
        } else {
            Err(RecycleError::Backend(NntpError::UnhealthyConnection.into()))
        }
    }
}

pub type NntpPool = Pool<NntpConnectionManager>;

/// A connection checked out of the pool. Dropping it returns it to the pool
/// only if its wire is in sync ([`AsyncNntpConnection::is_clean`]); a
/// connection abandoned mid-exchange (a cancelled job dropped its read) or
/// poisoned by a wire error is detached and closed instead, freeing the
/// provider's connection slot immediately.
pub struct PooledConnection {
    conn: Option<deadpool::managed::Object<NntpConnectionManager>>,
}

impl PooledConnection {
    fn inner(&mut self) -> &mut AsyncNntpConnection {
        self.conn.as_mut().expect("connection present until drop")
    }

    /// Select a group once per connection (best-effort; see `ensure_group`).
    pub async fn ensure_group(&mut self, group: &str) -> Result<(), DlNzbError> {
        self.inner().ensure_group(group).await
    }

    /// Queue a `BODY <id>` request (no flush). Part of the sliding window.
    pub async fn send_body(&mut self, message_id: &str) -> Result<(), DlNzbError> {
        self.inner().send_body(message_id).await
    }

    /// Flush queued requests to the socket.
    pub async fn flush(&mut self) -> Result<(), DlNzbError> {
        self.inner().flush().await
    }

    /// Read and classify the next pending `BODY` response (in send order).
    pub async fn read_body_outcome(&mut self, message_id: &str) -> ArticleOutcome {
        self.inner().read_body_outcome(message_id).await
    }

    /// The same, waiting at most `head_timeout` for the reply's status line.
    pub async fn read_body_outcome_within(
        &mut self,
        message_id: &str,
        head_timeout: std::time::Duration,
    ) -> ArticleOutcome {
        self.inner()
            .read_body_outcome_within(message_id, head_timeout)
            .await
    }

    /// `STAT` a batch: `Some(exists)`, or `None` when the server's reply
    /// says nothing either way (see `AsyncNntpConnection::check_articles_exist`).
    pub async fn check_articles_exist(
        &mut self,
        requests: &[crate::nntp::SegmentRequest],
    ) -> Result<Vec<(String, Option<bool>)>, DlNzbError> {
        self.inner().check_articles_exist(requests).await
    }

    /// Whether the connection hit an unrecoverable wire error.
    pub fn is_poisoned(&self) -> bool {
        self.conn.as_ref().is_none_or(|c| c.is_poisoned())
    }

    /// Return and reset the plaintext bytes read since the last call.
    pub fn take_bytes_read(&self) -> u64 {
        self.conn.as_ref().map_or(0, |c| c.take_bytes_read())
    }

    /// See [`AsyncNntpConnection::bytes_counter`].
    pub fn bytes_counter(&self) -> Option<Arc<std::sync::atomic::AtomicU64>> {
        self.conn.as_ref().map(|c| c.bytes_counter())
    }

    /// Close the connection now instead of handing it back to the pool (a
    /// paused job keeps no socket open), whatever state its wire is in. Its
    /// pool slot is freed at once.
    pub fn discard(&mut self) {
        if let Some(obj) = self.conn.take() {
            drop(deadpool::managed::Object::take(obj));
        }
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        if let Some(obj) = self.conn.take() {
            if !obj.is_clean() {
                // Detach so deadpool never recycles it; dropping closes the socket.
                drop(deadpool::managed::Object::take(obj));
            }
        }
    }
}

pub struct NntpPoolBuilder {
    config: UsenetConfig,
    max_size: usize,
    timeouts: deadpool::managed::Timeouts,
    max_concurrent_connections: usize,
    speed_limiter: Option<Arc<SpeedLimiter>>,
}

impl NntpPoolBuilder {
    pub fn new(config: UsenetConfig) -> Self {
        let max_size = config.connections as usize;
        Self {
            max_size,
            config,
            timeouts: deadpool::managed::Timeouts {
                wait: Some(Duration::from_secs(30)),
                create: Some(Duration::from_secs(30)),
                recycle: Some(Duration::from_secs(5)),
            },
            max_concurrent_connections: max_size,
            speed_limiter: None,
        }
    }

    pub fn max_concurrent_connections(mut self, limit: usize) -> Self {
        self.max_concurrent_connections = limit.max(1);
        self
    }

    /// Limit the pool's download speed with `limiter`, which may be shared
    /// with other pools (the engine shares one across server changes).
    pub fn speed_limiter(mut self, limiter: Arc<SpeedLimiter>) -> Self {
        self.speed_limiter = Some(limiter);
        self
    }

    pub fn build(self) -> Result<NntpPool, DlNzbError> {
        let mut manager = NntpConnectionManager::new(self.config, self.max_concurrent_connections)?;
        manager.speed_limiter = self.speed_limiter;
        Pool::builder(manager)
            .max_size(self.max_size)
            .runtime(deadpool::Runtime::Tokio1)
            .timeouts(self.timeouts)
            .build()
            .map_err(|e| {
                NntpError::ConnectionFailed {
                    server: "pool".to_string(),
                    port: 0,
                    source: std::io::Error::other(e),
                }
                .into()
            })
    }
}

#[async_trait]
pub trait NntpPoolExt {
    /// Check out a connection. Errors keep their real cause (authentication,
    /// DNS, TLS, timeout) so callers can classify them with
    /// [`DlNzbError::kind`](crate::error::DlNzbError::kind).
    async fn get_connection(&self) -> Result<PooledConnection, DlNzbError>;
}

/// Unwrap deadpool's error so the backend's real cause survives.
fn pool_error(e: PoolError<DlNzbError>) -> DlNzbError {
    match e {
        PoolError::Backend(inner) => inner,
        PoolError::Timeout(TimeoutType::Wait) | PoolError::Closed => {
            DownloadError::PoolExhausted.into()
        }
        PoolError::Timeout(_) => NntpError::Timeout { seconds: 30 }.into(),
        other => NntpError::ConnectionFailed {
            server: "pool".to_string(),
            port: 0,
            source: std::io::Error::other(other.to_string()),
        }
        .into(),
    }
}

#[async_trait]
impl NntpPoolExt for NntpPool {
    async fn get_connection(&self) -> Result<PooledConnection, DlNzbError> {
        let conn = self.get().await.map_err(|e| {
            tracing::debug!("Failed to get connection from pool: {}", e);
            pool_error(e)
        })?;
        Ok(PooledConnection { conn: Some(conn) })
    }
}
