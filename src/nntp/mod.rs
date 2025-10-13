//! NNTP protocol implementation and connection pooling
//!
//! This module provides async NNTP connection handling with connection pooling,
//! health checks, and optimized yEnc decoding.

mod connection;
mod pool;

pub use connection::AsyncNntpConnection;
pub use pool::{NntpPool, NntpPoolBuilder, NntpPoolExt, PooledConnection};
