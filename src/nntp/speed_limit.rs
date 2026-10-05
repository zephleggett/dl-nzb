//! The engine-wide download speed limit.
//!
//! One [`SpeedLimiter`] is shared by every connection an engine opens (across
//! pools, so a server change keeps the limit). It is a lock-free token bucket
//! in GCRA form: a single "theoretical arrival time" advanced by compare-and-
//! swap, so there is no mutex anywhere. The limit can be changed from any
//! thread; with no limit set, a connection's read path costs one relaxed
//! atomic load.
//!
//! Readers reserve a slice of the budget before reading (never reading more
//! than they reserved) and sleep until their slice is due, so connections are
//! served in turn rather than racing for tokens. Sleeps are capped at
//! [`MAX_WAIT_SLICE`]: a reader waiting under the old limit notices a raised or
//! removed limit within that time, and a lowered one applies at its next read.
//!
//! Only article bodies are metered. Control traffic (greeting, login, `DATE`,
//! `GROUP`, `STAT`) is read at once and charged afterwards, so a low limit
//! never stalls a connection's health check or login. Bytes are the
//! plaintext NNTP bytes the speed display counts (after TLS decryption).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::time::Sleep;

/// The smallest burst, so a low limit still moves data in useful chunks.
const MIN_BURST: u64 = 64 * 1024;
/// The burst is this many milliseconds' worth of the limit.
const BURST_MS: u64 = 100;
/// The smallest single reservation.
const MIN_GRANT: u64 = 16 * 1024;
/// A reservation is at most this fraction of the burst, so several connections
/// read at once and a queue of waiting connections is served in turn.
const GRANT_DIVISOR: u64 = 4;
/// The longest a waiting reader sleeps before looking at the limit again.
const MAX_WAIT_SLICE: Duration = Duration::from_millis(50);

/// A download speed limit shared by many connections; see the module docs.
#[derive(Debug)]
pub struct SpeedLimiter {
    /// Bytes per second; 0 = unlimited. The only field the unlimited read path
    /// touches, and nothing writes the others while unlimited.
    rate: AtomicU64,
    /// Bumped on every change of `rate`; a reservation made under an older
    /// epoch is dropped and made again under the new limit.
    epoch: AtomicU64,
    /// The time (ns on [`clock_ns`]) by which everything reserved so far has
    /// been paid for. Behind "now" means the bucket is full.
    tat: AtomicU64,
}

impl SpeedLimiter {
    /// A limiter at `bytes_per_sec` (`None` or 0 = unlimited).
    pub fn new(bytes_per_sec: Option<u64>) -> Self {
        Self {
            rate: AtomicU64::new(bytes_per_sec.unwrap_or(0)),
            epoch: AtomicU64::new(0),
            tat: AtomicU64::new(0),
        }
    }

    /// Change the limit (`None` or 0 = unlimited). Takes effect at once for
    /// readers about to read and within 50 ms for waiting ones.
    pub fn set(&self, bytes_per_sec: Option<u64>) {
        let rate = bytes_per_sec.unwrap_or(0);
        if self.rate.swap(rate, Ordering::Relaxed) == rate {
            return;
        }
        // Start the new limit with a full bucket and no queue: every reader
        // reserves again under the new rate.
        self.tat.store(clock_ns(), Ordering::Relaxed);
        self.epoch.fetch_add(1, Ordering::Relaxed);
    }

    /// The current limit; `None` = unlimited.
    pub fn get(&self) -> Option<u64> {
        match self.rate() {
            0 => None,
            n => Some(n),
        }
    }

    #[inline]
    fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// Reserve up to `want` bytes at `rate`. Returns the bytes reserved and
    /// when (on [`clock_ns`]) they may be read.
    fn reserve(&self, want: u64, rate: u64, now: u64) -> Reservation {
        let burst = burst_bytes(rate);
        let bytes = want.min((burst / GRANT_DIVISOR).max(MIN_GRANT)).max(1);
        let cost = ns_for(bytes, rate);
        let tolerance = ns_for(burst, rate);
        let mut tat = self.tat.load(Ordering::Relaxed);
        loop {
            let next = tat.max(now).saturating_add(cost);
            match self
                .tat
                .compare_exchange_weak(tat, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => {
                    return Reservation {
                        bytes,
                        ready_at: next.saturating_sub(tolerance),
                    }
                }
                Err(actual) => tat = actual,
            }
        }
    }

    /// Pay for `bytes` already read without waiting (control traffic).
    fn charge(&self, bytes: u64, rate: u64, now: u64) {
        let cost = ns_for(bytes, rate);
        let _ = self
            .tat
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |tat| {
                Some(tat.max(now).saturating_add(cost))
            });
    }

    /// Return `bytes` reserved but never read.
    fn refund(&self, bytes: u64, rate: u64) {
        let cost = ns_for(bytes, rate);
        let _ = self
            .tat
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |tat| {
                Some(tat.saturating_sub(cost))
            });
    }
}

#[derive(Clone, Copy, Debug)]
struct Reservation {
    bytes: u64,
    ready_at: u64,
}

/// max(64 KiB, 100 ms of the limit).
fn burst_bytes(rate: u64) -> u64 {
    (rate / (1000 / BURST_MS)).max(MIN_BURST)
}

/// Nanoseconds to transfer `bytes` at `rate` bytes per second.
fn ns_for(bytes: u64, rate: u64) -> u64 {
    let ns = (bytes as u128 * 1_000_000_000).div_ceil(rate.max(1) as u128);
    ns.min(u64::MAX as u128) as u64
}

fn clock_origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// Monotonic nanoseconds since the first use, the limiter's time base.
fn clock_ns() -> u64 {
    let origin = clock_origin();
    Instant::now()
        .saturating_duration_since(origin)
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

/// One connection's link to the limiter, shared by the connection and its
/// reader. The connection says when reads are metered and asks how long it
/// has waited on the limit (so read timeouts don't count that time).
#[derive(Debug)]
pub(crate) struct ConnThrottle {
    limiter: Arc<SpeedLimiter>,
    /// True while an article body is being read; everything else is control
    /// traffic, read at once and charged afterwards.
    metered: AtomicBool,
    /// Total nanoseconds of finished waits.
    waited_ns: AtomicU64,
    /// `clock_ns() + 1` when the current wait began; 0 when not waiting.
    waiting_since: AtomicU64,
}

impl ConnThrottle {
    pub(crate) fn new(limiter: Arc<SpeedLimiter>) -> Self {
        Self {
            limiter,
            metered: AtomicBool::new(false),
            waited_ns: AtomicU64::new(0),
            waiting_since: AtomicU64::new(0),
        }
    }

    /// Meter (article bodies) or only charge (control traffic) later reads.
    pub(crate) fn set_metered(&self, metered: bool) {
        self.metered.store(metered, Ordering::Relaxed);
    }

    /// Time this connection has spent waiting on the limit, including a wait
    /// in progress.
    pub(crate) fn throttled(&self) -> Duration {
        let mut ns = self.waited_ns.load(Ordering::Relaxed);
        let since = self.waiting_since.load(Ordering::Relaxed);
        if since != 0 {
            ns = ns.saturating_add(clock_ns().saturating_sub(since - 1));
        }
        Duration::from_nanos(ns)
    }

    fn begin_wait(&self, now: u64) {
        self.waiting_since.store(now + 1, Ordering::Relaxed);
    }

    fn end_wait(&self) {
        let since = self.waiting_since.swap(0, Ordering::Relaxed);
        if since != 0 {
            let waited = clock_ns().saturating_sub(since - 1);
            self.waited_ns.fetch_add(waited, Ordering::Relaxed);
        }
    }
}

/// What a read may do.
pub(crate) enum Grant {
    /// No limit: read freely.
    Unlimited,
    /// Control traffic: read freely, then [`ReadThrottle::charge`] the bytes.
    Charge,
    /// Read at most this many bytes, then [`ReadThrottle::consume`] them.
    Bytes(usize),
}

/// The reader's side of the limit: the slice it has reserved or been granted,
/// and the timer it sleeps on while its slice comes due.
pub(crate) struct ReadThrottle {
    shared: Arc<ConnThrottle>,
    /// Bytes granted and not yet read (valid in `epoch`).
    credit: u64,
    /// A reservation not yet due (valid in `epoch`).
    pending: Option<Reservation>,
    epoch: u64,
    /// The rate `credit` and `pending` were paid at, for refunds.
    paid_rate: u64,
    /// Allocated on the first wait, then reset.
    sleep: Option<Pin<Box<Sleep>>>,
    waiting: bool,
}

impl ReadThrottle {
    pub(crate) fn new(shared: Arc<ConnThrottle>) -> Self {
        let epoch = shared.limiter.epoch();
        Self {
            shared,
            credit: 0,
            pending: None,
            epoch,
            paid_rate: 0,
            sleep: None,
            waiting: false,
        }
    }

    /// The fast-path check: a single relaxed load.
    #[inline]
    pub(crate) fn is_limited(&self) -> bool {
        self.shared.limiter.rate() != 0
    }

    /// Wait until up to `want` (> 0) bytes may be read. Pending means a timer
    /// is armed and `cx` will be woken.
    pub(crate) fn poll_grant(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Grant> {
        let Self {
            shared,
            credit,
            pending,
            epoch: held_epoch,
            paid_rate,
            sleep,
            waiting,
        } = self;
        let limiter = &shared.limiter;
        // Bounded: each pass either grants, reserves, or arms a timer in the
        // future (which is Pending); the bound only guards against a clock
        // that refuses to advance.
        for _ in 0..4 {
            let rate = limiter.rate();
            if rate == 0 {
                end_wait(waiting, shared);
                return Poll::Ready(Grant::Unlimited);
            }
            if !shared.metered.load(Ordering::Relaxed) {
                end_wait(waiting, shared);
                return Poll::Ready(Grant::Charge);
            }
            let epoch = limiter.epoch();
            if epoch != *held_epoch {
                // The limit changed: what was paid under the old one no
                // longer counts (the bucket was reset).
                *held_epoch = epoch;
                *credit = 0;
                *pending = None;
            }
            if *credit > 0 {
                end_wait(waiting, shared);
                let n = (*credit).min(want as u64) as usize;
                return Poll::Ready(Grant::Bytes(n));
            }
            let now = clock_ns();
            let reservation = match *pending {
                Some(r) => r,
                None => {
                    let r = limiter.reserve(want as u64, rate, now);
                    *paid_rate = rate;
                    *pending = Some(r);
                    r
                }
            };
            if reservation.ready_at <= now {
                *pending = None;
                *credit = reservation.bytes;
                continue;
            }
            if !*waiting {
                *waiting = true;
                shared.begin_wait(now);
            }
            let wake_ns = reservation
                .ready_at
                .min(now.saturating_add(MAX_WAIT_SLICE.as_nanos() as u64));
            let deadline =
                tokio::time::Instant::from_std(clock_origin() + Duration::from_nanos(wake_ns));
            let timer = match sleep.as_mut() {
                Some(timer) => {
                    timer.as_mut().reset(deadline);
                    timer
                }
                None => sleep.insert(Box::pin(tokio::time::sleep_until(deadline))),
            };
            if timer.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    /// `n` bytes of a [`Grant::Bytes`] were read.
    pub(crate) fn consume(&mut self, n: usize) {
        self.credit = self.credit.saturating_sub(n as u64);
    }

    /// `n` bytes of a [`Grant::Charge`] were read.
    pub(crate) fn charge(&self, n: usize) {
        let rate = self.shared.limiter.rate();
        if rate != 0 && n > 0 {
            self.shared.limiter.charge(n as u64, rate, clock_ns());
        }
    }
}

/// Close the reader's current wait, if any, in the connection's tally.
fn end_wait(waiting: &mut bool, shared: &ConnThrottle) {
    if *waiting {
        *waiting = false;
        shared.end_wait();
    }
}

impl Drop for ReadThrottle {
    fn drop(&mut self) {
        end_wait(&mut self.waiting, &self.shared);
        // Give back what this connection reserved but will never read (a
        // stopped job dropping its connections mid-wait).
        let unused = self.credit + self.pending.map_or(0, |r| r.bytes);
        let limiter = &self.shared.limiter;
        if unused > 0 && self.paid_rate != 0 && limiter.epoch() == self.epoch {
            limiter.refund(unused, self.paid_rate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_is_at_least_64k_or_100ms() {
        assert_eq!(burst_bytes(1024), MIN_BURST);
        assert_eq!(burst_bytes(10 * 1024 * 1024), 1024 * 1024);
    }

    #[test]
    fn an_idle_bucket_grants_a_burst_then_queues() {
        let limiter = SpeedLimiter::new(Some(1024 * 1024));
        let now = clock_ns() + 1_000_000_000;
        // 100 KiB burst, 25 KiB grants: four are due at once, the fifth later.
        let mut ready = Vec::new();
        for _ in 0..5 {
            ready.push(limiter.reserve(1 << 20, 1024 * 1024, now).ready_at);
        }
        assert!(ready[..4].iter().all(|&r| r <= now), "{ready:?}");
        assert!(ready[4] > now);
        // The fifth is due one grant's time (25 KiB at 1 MiB/s) after now.
        let grant = (burst_bytes(1024 * 1024) / GRANT_DIVISOR) as f64;
        let expected = grant / (1024.0 * 1024.0) * 1e9;
        let got = (ready[4] - now) as f64;
        assert!((got - expected).abs() < 1e6, "{got} vs {expected}");
    }

    #[test]
    fn changing_the_limit_resets_the_queue() {
        let limiter = SpeedLimiter::new(Some(1024));
        let now = clock_ns();
        for _ in 0..10 {
            limiter.reserve(1 << 20, 1024, now);
        }
        assert!(limiter.tat.load(Ordering::Relaxed) > now + 10_000_000_000);
        limiter.set(Some(1024 * 1024));
        assert!(limiter.tat.load(Ordering::Relaxed) <= clock_ns());
        assert_eq!(limiter.get(), Some(1024 * 1024));
        // Setting the same value again is not a change.
        let epoch = limiter.epoch();
        limiter.set(Some(1024 * 1024));
        assert_eq!(limiter.epoch(), epoch);
        limiter.set(None);
        assert_eq!(limiter.get(), None);
    }
}
