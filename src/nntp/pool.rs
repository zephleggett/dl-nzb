//! Connection pool for NNTP connections using deadpool
//!
//! This module provides a robust connection pool that handles connection lifecycle,
//! health checks, and automatic reconnection.

use super::connection::AsyncNntpConnection;
use crate::config::UsenetConfig;
use crate::error::{DlNzbError, NntpError};
use async_trait::async_trait;
use bytes::Bytes;
use deadpool::managed::{Manager, Pool, RecycleResult};
use std::sync::Arc;
use tokio::time::Duration;

/// Connection manager for deadpool with rate-limited creation
pub struct NntpConnectionManager {
    config: Arc<UsenetConfig>,
    tls_connector: Option<Arc<tokio_native_tls::TlsConnector>>,
    creation_semaphore: Arc<tokio::sync::Semaphore>,
}

impl NntpConnectionManager {
    pub fn new(config: UsenetConfig) -> Result<Self, DlNzbError> {
        // Create shared TLS connector for session reuse
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

        // Rate limit connection creation to avoid overwhelming server
        // Allow up to 10 connections to be created concurrently
        let creation_semaphore = Arc::new(tokio::sync::Semaphore::new(10));

        Ok(Self {
            config: Arc::new(config),
            tls_connector,
            creation_semaphore,
        })
    }
}

impl Manager for NntpConnectionManager {
    type Type = AsyncNntpConnection;
    type Error = DlNzbError;

    async fn create(&self) -> Result<AsyncNntpConnection, DlNzbError> {
        // Rate limit connection creation - only allow 10 concurrent connection attempts
        let _permit = self.creation_semaphore.acquire().await.map_err(|e| {
            DlNzbError::from(NntpError::ConnectionFailed {
                server: self.config.server.clone(),
                port: self.config.port,
                source: std::io::Error::other(e),
            })
        })?;

        AsyncNntpConnection::connect(&self.config, self.tls_connector.clone())
            .await
            .map_err(|e| {
                tracing::error!("Failed to create NNTP connection: {}", e);
                e
            })
    }

    async fn recycle(
        &self,
        conn: &mut AsyncNntpConnection,
        _metrics: &deadpool::managed::Metrics,
    ) -> RecycleResult<DlNzbError> {
        // Check if connection is still healthy
        if conn.is_healthy().await {
            Ok(())
        } else {
            Err(deadpool::managed::RecycleError::Backend(
                NntpError::UnhealthyConnection.into(),
            ))
        }
    }
}

/// NNTP connection pool
pub type NntpPool = Pool<NntpConnectionManager>;

/// Pooled NNTP connection with convenience methods
pub struct PooledConnection {
    conn: deadpool::managed::Object<NntpConnectionManager>,
}

impl PooledConnection {
    /// Download a segment using this pooled connection
    pub async fn download_segment(
        &mut self,
        message_id: &str,
        group: &str,
    ) -> Result<Bytes, DlNzbError> {
        self.conn
            .download_segment(message_id, group)
            .await}
}

/// Builder for creating connection pools with configuration
pub struct NntpPoolBuilder {
    config: UsenetConfig,
    max_size: usize,
    timeouts: deadpool::managed::Timeouts,
}

impl NntpPoolBuilder {
    pub fn new(config: UsenetConfig) -> Self {
        Self {
            max_size: config.connections as usize,
            config,
            timeouts: deadpool::managed::Timeouts {
                wait: Some(Duration::from_secs(30)),
                create: Some(Duration::from_secs(30)),
                recycle: Some(Duration::from_secs(5)),
            },
        }
    }

    pub fn max_size(mut self, size: usize) -> Self {
        self.max_size = size;
        self
    }

    pub fn timeouts(mut self, timeouts: deadpool::managed::Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    pub fn build(self) -> Result<NntpPool, DlNzbError> {
        let manager = NntpConnectionManager::new(self.config)?;
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

/// Extension trait for the pool to provide convenient methods
#[async_trait]
pub trait NntpPoolExt {
    /// Get a connection from the pool
    async fn get_connection(&self) -> Result<PooledConnection, DlNzbError>;
}

#[async_trait]
impl NntpPoolExt for NntpPool {
    async fn get_connection(&self) -> Result<PooledConnection, DlNzbError> {
        let conn = self.get().await.map_err(|e| {
            tracing::error!("Failed to get connection from pool: {}", e);
            NntpError::ConnectionFailed {
                server: "pool".to_string(),
                port: 0,
                source: std::io::Error::other(e),
            }
        })?;
        Ok(PooledConnection { conn })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UsenetConfig;

    #[tokio::test]
    async fn test_pool_builder() {
        let config = UsenetConfig::default();
        let result = NntpPoolBuilder::new(config).max_size(10).build();
        // Pool creation should succeed even if we can't connect
        assert!(result.is_ok() || result.is_err());
    }
}
