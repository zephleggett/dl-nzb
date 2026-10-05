//! NNTP protocol implementation and connection pooling
//!
//! Async NNTP with connection pooling, health checks, a speed limit shared by
//! every connection, and a yEnc decoder that parses `=ypart` headers for
//! correct file offsets.

mod connection;
mod counting;
mod pool;
mod speed_limit;
mod yenc;

pub use connection::{
    is_valid_group_name, is_valid_message_id, ArticleOutcome, AsyncNntpConnection, SegmentRequest,
};
pub use pool::{NntpPool, NntpPoolBuilder, NntpPoolExt, PooledConnection};
pub use speed_limit::SpeedLimiter;
