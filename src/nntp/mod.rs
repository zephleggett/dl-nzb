//! NNTP protocol implementation and connection pooling
//!
//! Async NNTP with connection pooling, health checks, and a yEnc decoder that
//! parses `=ypart` headers for correct file offsets.

mod connection;
mod counting;
mod pool;
mod yenc;

pub use connection::{ArticleOutcome, AsyncNntpConnection, SegmentRequest};
pub use pool::{NntpPool, NntpPoolBuilder, NntpPoolExt, PooledConnection};
