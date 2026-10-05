//! Thin `AsyncRead` adapter that tallies bytes successfully read, and applies
//! the engine's speed limit.
//!
//! Used to report the *true* download throughput. The sum of `segment.bytes`
//! from the NZB is the encoded payload; the wire carries a little more (~2-4%)
//! for yEnc `=ybegin`/`=ypart`/`=yend` framing, escape sequences, NNTP commands
//! and responses, and dot-stuffing — plus any bytes spent on retries. Wrapping
//! the read half at the connection level captures plaintext bytes (post-TLS
//! decryption), which lines up with what NNTP clients and network monitors
//! typically report.
//!
//! The speed limit sits here for the same reason: it sees every byte the
//! counter sees, so the limit and the reported speed agree. Under TLS this
//! wraps the decrypted stream, so reading less per call only leaves decrypted
//! data buffered in the TLS layer (and, beyond that, unread on the socket,
//! where TCP flow control slows the sender); records are never split here.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{ready, Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

use super::speed_limit::{Grant, ReadThrottle};

pub struct CountingReader<R> {
    inner: R,
    counter: Arc<AtomicU64>,
    throttle: Option<ReadThrottle>,
}

impl<R> CountingReader<R> {
    pub fn new(inner: R, counter: Arc<AtomicU64>) -> Self {
        Self {
            inner,
            counter,
            throttle: None,
        }
    }

    /// Also apply a speed limit to reads.
    pub(crate) fn with_throttle(mut self, throttle: ReadThrottle) -> Self {
        self.throttle = Some(throttle);
        self
    }
}

impl<R: AsyncRead + Unpin> CountingReader<R> {
    fn poll_read_counted(
        inner: &mut R,
        counter: &AtomicU64,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<usize>> {
        let before = buf.filled().len();
        ready!(Pin::new(inner).poll_read(cx, buf))?;
        let added = buf.filled().len() - before;
        if added > 0 {
            counter.fetch_add(added as u64, Ordering::Relaxed);
        }
        Poll::Ready(Ok(added))
    }

    /// The read path while a limit is set.
    #[cold]
    fn poll_read_limited(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let Self {
            inner,
            counter,
            throttle,
        } = self;
        let Some(throttle) = throttle.as_mut() else {
            return Self::poll_read_counted(inner, counter, cx, buf).map_ok(|_| ());
        };
        if buf.remaining() == 0 {
            return Pin::new(inner).poll_read(cx, buf);
        }
        match ready!(throttle.poll_grant(cx, buf.remaining())) {
            Grant::Unlimited => Self::poll_read_counted(inner, counter, cx, buf).map_ok(|_| ()),
            Grant::Charge => {
                let added = ready!(Self::poll_read_counted(inner, counter, cx, buf))?;
                throttle.charge(added);
                Poll::Ready(Ok(()))
            }
            Grant::Bytes(granted) => {
                // Read into at most `granted` bytes of the caller's buffer
                // (as `tokio::io::Take` does), then advance the caller's
                // buffer by what arrived.
                let mut limited = buf.take(granted);
                let start = limited.filled().as_ptr();
                ready!(Pin::new(&mut *inner).poll_read(cx, &mut limited))?;
                assert_eq!(
                    limited.filled().as_ptr(),
                    start,
                    "reader swapped the buffer"
                );
                let added = limited.filled().len();
                // SAFETY: `limited` is the start of `buf`'s unfilled region and
                // its first `added` bytes were filled (so initialized) by the
                // inner reader.
                unsafe { buf.assume_init(added) };
                buf.advance(added);
                if added > 0 {
                    counter.fetch_add(added as u64, Ordering::Relaxed);
                    throttle.consume(added);
                }
                Poll::Ready(Ok(()))
            }
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CountingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        // Unlimited (or no limiter): one relaxed load, then a plain read.
        if this.throttle.as_ref().is_some_and(ReadThrottle::is_limited) {
            return this.poll_read_limited(cx, buf);
        }
        Self::poll_read_counted(&mut this.inner, &this.counter, cx, buf).map_ok(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::super::speed_limit::{ConnThrottle, SpeedLimiter};
    use super::*;
    use std::time::{Duration, Instant};
    use tokio::io::AsyncReadExt;

    const MIB: u64 = 1024 * 1024;

    /// An endless reader of zeros, metered (as an article body is) by `limiter`.
    fn limited_zeros(limiter: &Arc<SpeedLimiter>) -> CountingReader<tokio::io::Repeat> {
        let shared = Arc::new(ConnThrottle::new(limiter.clone()));
        shared.set_metered(true);
        CountingReader::new(tokio::io::repeat(0), Arc::new(AtomicU64::new(0)))
            .with_throttle(ReadThrottle::new(shared))
    }

    /// Read from `reader` with a 256 KiB buffer until `deadline`; bytes read.
    async fn read_until(reader: &mut (impl AsyncRead + Unpin), deadline: Instant) -> u64 {
        let mut buf = vec![0u8; 256 * 1024];
        let mut total = 0u64;
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, reader.read(&mut buf)).await {
                Ok(n) => total += n.unwrap() as u64,
                Err(_) => break,
            }
        }
        total
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn readers_share_the_limit() {
        let limiter = Arc::new(SpeedLimiter::new(Some(4 * MIB)));
        let started = Instant::now();
        let deadline = started + Duration::from_millis(1500);
        let tasks: Vec<_> = (0..6)
            .map(|_| {
                let mut reader = limited_zeros(&limiter);
                tokio::spawn(async move { read_until(&mut reader, deadline).await })
            })
            .collect();
        let mut total = 0;
        let mut each = Vec::new();
        for t in tasks {
            let n = t.await.unwrap();
            each.push(n);
            total += n;
        }
        let rate = total as f64 / started.elapsed().as_secs_f64();
        let target = (4 * MIB) as f64;
        eprintln!("6 readers at 4 MiB/s: {rate:.0} B/s, each {each:?}");
        assert!(
            (rate - target).abs() / target < 0.15,
            "rate {rate:.0} vs {target}"
        );
        // Served in turn: nobody is starved.
        let fair = total / 6;
        assert!(each.iter().all(|&n| n > fair / 3), "{each:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lowering_and_removing_the_limit_apply_promptly() {
        let limiter = Arc::new(SpeedLimiter::new(Some(16 * MIB)));
        let mut reader = limited_zeros(&limiter);
        read_until(&mut reader, Instant::now() + Duration::from_millis(300)).await;

        limiter.set(Some(MIB));
        // Allow 200 ms for the change, then measure for 600 ms.
        read_until(&mut reader, Instant::now() + Duration::from_millis(200)).await;
        let started = Instant::now();
        let n = read_until(&mut reader, started + Duration::from_millis(600)).await;
        let rate = n as f64 / started.elapsed().as_secs_f64();
        eprintln!("16 MiB/s lowered to 1 MiB/s: {rate:.0} B/s");
        let target = MIB as f64;
        assert!(
            (rate - target).abs() / target < 0.2,
            "rate after lowering: {rate:.0}"
        );

        // A reader waiting at a very low limit wakes promptly once it's removed.
        limiter.set(Some(1024));
        let mut buf = vec![0u8; 256 * 1024];
        // Spend the burst; the next read has to wait about 16 s.
        let mut read = 0;
        while read < 64 * 1024 {
            read += reader.read(&mut buf).await.unwrap();
        }
        let waiter = tokio::spawn(async move {
            let started = Instant::now();
            assert!(reader.read(&mut buf).await.unwrap() > 0);
            started.elapsed()
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        limiter.set(None);
        let waited = waiter.await.unwrap();
        eprintln!("read waiting at 1 KiB/s returned {waited:?} after a removal at 100 ms");
        assert!(waited < Duration::from_millis(300), "waited {waited:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unlimited_reads_cost_nothing_measurable() {
        const BYTES: u64 = 512 * MIB;
        async fn time(mut reader: impl AsyncRead + Unpin) -> Duration {
            let mut buf = vec![0u8; 256 * 1024];
            let started = Instant::now();
            let mut total = 0u64;
            while total < BYTES {
                total += reader.read(&mut buf).await.unwrap() as u64;
            }
            started.elapsed()
        }
        let limiter = Arc::new(SpeedLimiter::new(None));
        let mut plain = Duration::MAX;
        let mut throttled = Duration::MAX;
        for _ in 0..3 {
            let reader = CountingReader::new(tokio::io::repeat(0), Arc::new(AtomicU64::new(0)));
            plain = plain.min(time(reader).await);
            throttled = throttled.min(time(limited_zeros(&limiter)).await);
        }
        eprintln!("512 MiB: unlimited throttle {throttled:?}, no throttle {plain:?}");
        assert!(
            throttled.as_secs_f64() <= plain.as_secs_f64() * 1.25 + 0.005,
            "unlimited {throttled:?} vs no limiter {plain:?}"
        );
    }
}
