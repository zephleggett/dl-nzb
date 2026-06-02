//! Connection pool for NNTP connections.
//!
//! Backed by `deadpool` for lifecycle management. Once configured we eagerly
//! warm a quota of connections in parallel so the download saturates the
//! provider's connection budget within the first second instead of ramping up
//! over the course of the file.

use super::connection::AsyncNntpConnection;
use super::ArticleOutcome;
use crate::config::UsenetConfig;
use crate::error::{DlNzbError, NntpError};
use async_trait::async_trait;
use deadpool::managed::{Manager, Pool, RecycleError, RecycleResult};
use futures::stream::{FuturesUnordered, StreamExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Connection manager for deadpool with rate-limited concurrent connect attempts.
pub struct NntpConnectionManager {
    config: Arc<UsenetConfig>,
    tls_connector: Option<Arc<tokio_native_tls::TlsConnector>>,
    creation_semaphore: Arc<tokio::sync::Semaphore>,
    /// Plaintext bytes received from the socket across all connections in the
    /// pool. Used to report a real-world wire throughput in the summary and
    /// live progress bar — `segment.bytes` from the NZB undercounts because
    /// it omits yEnc framing, escape sequences, NNTP commands/responses, and
    /// dot-stuffing.
    bytes_read: Arc<AtomicU64>,
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
            let native_connector = tls_builder
                .build()
                .map_err(|e| NntpError::TlsError(e.to_string()))?;
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
            bytes_read: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn bytes_read_counter(&self) -> Arc<AtomicU64> {
        self.bytes_read.clone()
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

        AsyncNntpConnection::connect(
            &self.config,
            self.tls_connector.clone(),
            self.bytes_read.clone(),
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
        if conn.is_poisoned() {
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

pub struct PooledConnection {
    conn: deadpool::managed::Object<NntpConnectionManager>,
}

impl PooledConnection {
    /// Select a group once per connection (best-effort; see `ensure_group`).
    pub async fn ensure_group(&mut self, group: &str) -> Result<(), DlNzbError> {
        self.conn.ensure_group(group).await
    }

    /// Queue a `BODY <id>` request (no flush). Part of the sliding window.
    pub async fn send_body(&mut self, message_id: &str) -> Result<(), DlNzbError> {
        self.conn.send_body(message_id).await
    }

    /// Flush queued requests to the socket.
    pub async fn flush(&mut self) -> Result<(), DlNzbError> {
        self.conn.flush().await
    }

    /// Read and classify the next pending `BODY` response (in send order).
    pub async fn read_body_outcome(&mut self, message_id: &str) -> ArticleOutcome {
        self.conn.read_body_outcome(message_id).await
    }

    pub async fn check_articles_exist(
        &mut self,
        requests: &[crate::nntp::SegmentRequest],
    ) -> Result<Vec<(String, bool)>, DlNzbError> {
        self.conn.check_articles_exist(requests).await
    }

    /// Whether the connection hit an unrecoverable wire error.
    pub fn is_poisoned(&self) -> bool {
        self.conn.is_poisoned()
    }

    /// Return the connection to the pool. Poisoned connections are detached
    /// so deadpool doesn't try to recycle one whose wire state is unknown.
    pub fn release(self) {
        if self.conn.is_poisoned() {
            let _ = deadpool::managed::Object::take(self.conn);
        }
    }
}

pub struct NntpPoolBuilder {
    config: UsenetConfig,
    max_size: usize,
    timeouts: deadpool::managed::Timeouts,
    max_concurrent_connections: usize,
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
        }
    }

    pub fn max_concurrent_connections(mut self, limit: usize) -> Self {
        self.max_concurrent_connections = limit.max(1);
        self
    }

    pub fn build(self) -> Result<NntpPool, DlNzbError> {
        let manager = NntpConnectionManager::new(self.config, self.max_concurrent_connections)?;
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
    async fn get_connection(&self) -> Result<PooledConnection, DlNzbError>;
    /// Open up to `count` connections in parallel and immediately return them
    /// to the pool. Returns the number that came up successfully.
    async fn warm_up(&self, count: usize) -> usize;
    /// Plaintext bytes read across all connections — used to compute the real
    /// download throughput shown in the summary and live progress bar.
    fn bytes_read(&self) -> u64;
    fn bytes_read_counter(&self) -> Arc<AtomicU64>;
}

#[async_trait]
impl NntpPoolExt for NntpPool {
    async fn get_connection(&self) -> Result<PooledConnection, DlNzbError> {
        let conn = self.get().await.map_err(|e| {
            tracing::debug!("Failed to get connection from pool: {}", e);
            NntpError::ConnectionFailed {
                server: "pool".to_string(),
                port: 0,
                source: std::io::Error::other(e),
            }
        })?;
        Ok(PooledConnection { conn })
    }

    async fn warm_up(&self, count: usize) -> usize {
        let mut tasks = FuturesUnordered::new();
        for _ in 0..count {
            let pool = self.clone();
            tasks.push(async move { pool.get().await });
        }
        let mut connections = Vec::with_capacity(count);
        let mut ok = 0usize;
        while let Some(result) = tasks.next().await {
            if let Ok(conn) = result {
                ok += 1;
                connections.push(conn);
            }
        }
        // Returning the Objects to scope drops them back into the pool.
        drop(connections);
        ok
    }

    fn bytes_read(&self) -> u64 {
        self.manager().bytes_read.load(Ordering::Relaxed)
    }

    fn bytes_read_counter(&self) -> Arc<AtomicU64> {
        self.manager().bytes_read_counter()
    }
}
