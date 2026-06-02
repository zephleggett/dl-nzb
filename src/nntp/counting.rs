//! Thin `AsyncRead` adapter that tallies bytes successfully read.
//!
//! Used to report the *true* download throughput. The sum of `segment.bytes`
//! from the NZB is the encoded payload; the wire carries a little more (~2-4%)
//! for yEnc `=ybegin`/`=ypart`/`=yend` framing, escape sequences, NNTP commands
//! and responses, and dot-stuffing — plus any bytes spent on retries. Wrapping
//! the read half at the connection level captures plaintext bytes (post-TLS
//! decryption), which lines up with what NNTP clients and network monitors
//! typically report.
//!
//! The counter is shared (`Arc<AtomicU64>`) so a single connection pool can
//! aggregate across all its connections.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

pub struct CountingReader<R> {
    inner: R,
    counter: Arc<AtomicU64>,
}

impl<R> CountingReader<R> {
    pub fn new(inner: R, counter: Arc<AtomicU64>) -> Self {
        Self { inner, counter }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CountingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &result {
            let added = buf.filled().len() - before;
            if added > 0 {
                this.counter.fetch_add(added as u64, Ordering::Relaxed);
            }
        }
        result
    }
}
