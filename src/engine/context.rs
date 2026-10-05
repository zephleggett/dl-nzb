//! Per-job context, replacing the old process-wide shutdown flag.
//!
//! One [`JobCtx`] is shared (behind an `Arc`) by everything working on a job:
//! the orchestration task, download workers, the writer, post-processing and
//! the blocking PAR2/unrar threads. It carries
//! - cancellation: a `CancellationToken` for async code to `select!` on, plus
//!   an `Arc<AtomicBool>` mirror for blocking code (par2-rs, unrar) that can
//!   only poll;
//! - pause: a `watch` channel download workers obey;
//! - progress: the current phase and its counters, sampled into
//!   [`JobProgress`] events by a 4 Hz ticker;
//! - statistics: per-job wire bytes and article counts;
//! - the observer, behind a gate that serialises events and guarantees
//!   `Finished` is delivered exactly once and last (an observer that panics
//!   loses that one event, nothing more).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::{CancellationToken, WaitForCancellationFuture};

use super::types::{
    AvailabilityInfo, ErrorKind, JobEvent, JobObserver, JobPhase, JobProgress, JobSummary,
    NullObserver,
};
use crate::download::DownloadResult;
use crate::error::DlNzbError;

/// Progress ticker period: 4 Hz.
const TICK: Duration = Duration::from_millis(250);
/// Time constant of the speed moving average.
const SPEED_TAU_SECS: f64 = 2.0;
/// No byte for this long is a stall (offline, a server that went quiet):
/// the speed drops to zero and the ETA goes until bytes flow again.
const STALL: Duration = Duration::from_secs(3);
/// The ETA waits for this much steady transfer after every (re)start:
/// entering a download phase, resuming, or bytes flowing again after a stall.
/// Before that the speed average is still catching up, and an ETA from it
/// is wildly long ("2 days left").
const ETA_SETTLE: Duration = Duration::from_secs(3);
/// No ETA beyond a day, nor below this speed (bytes per second): it would
/// say nothing useful.
const ETA_MAX_SECS: f64 = 24.0 * 60.0 * 60.0;
const ETA_MIN_SPEED: f64 = 1024.0;

pub(crate) struct JobCtx {
    cancel: CancellationToken,
    cancel_flag: Arc<AtomicBool>,
    pause_tx: watch::Sender<bool>,
    observer: Arc<dyn JobObserver>,
    /// Serialises observer calls, and holds the last `Progress` delivered.
    /// Holding the lock while calling out keeps events strictly ordered
    /// across the threads that emit them, and `finished` (set under it)
    /// drops anything that races in after `Finished`. Lock order: the gate,
    /// then `progress`.
    gate: Mutex<Option<JobProgress>>,
    /// Set under the gate just before `Finished` is delivered (readable from
    /// inside the observer, which holds the gate).
    finished: AtomicBool,
    progress: Mutex<ProgressState>,
    wire_bytes: AtomicU64,
    /// Byte counters of the connections the job's download workers hold,
    /// gathered into `wire_bytes` at every tick, so the speed follows bytes
    /// as they arrive rather than in whole-article steps (a slow link would
    /// otherwise look stalled between articles).
    live_counters: Mutex<Vec<Arc<AtomicU64>>>,
    articles_total: AtomicU64,
    articles_failed: AtomicU64,
    /// Of `articles_failed`, those given up because the server never got
    /// them across (connection resets, replies out of step): not missing.
    connection_failures: AtomicU64,
    /// The last error a worker gave up on when it could not get a connection
    /// at all; lets the job report "server unreachable" with its real kind
    /// instead of "articles missing".
    connection_error: Mutex<Option<(ErrorKind, String)>>,
}

struct ProgressState {
    phase: Option<JobPhase>,
    bytes_done: u64,
    bytes_total: u64,
    files_done: u32,
    files_total: u32,
    /// Explicit phase fraction (PAR2, extraction); `None` derives it from bytes.
    fraction: Option<f64>,
    detail: Option<String>,
    damaged_blocks: u64,
    speed: Speed,
}

/// The download speed average and when its ETA can be trusted.
#[derive(Debug, Clone)]
struct Speed {
    /// Exponential moving average, bytes per second.
    bps: f64,
    sampled_at: Instant,
    sampled_wire: u64,
    /// Since when bytes have flowed without a stall, since the last
    /// (re)start; `None` while paused, stalled, or not started.
    flowing_since: Option<Instant>,
    /// The last sample that saw new bytes.
    last_flow: Instant,
}

impl Speed {
    fn new(now: Instant) -> Self {
        Self {
            bps: 0.0,
            sampled_at: now,
            sampled_wire: 0,
            flowing_since: None,
            last_flow: now,
        }
    }

    /// Start over (a new phase, a pause, a stall): the average restarts from
    /// the next sample with bytes, and the ETA waits to settle again.
    fn restart(&mut self) {
        self.bps = 0.0;
        self.flowing_since = None;
    }

    /// Fold the job's wire byte count at `now` into the average (samples
    /// closer together than 50 ms are skipped).
    fn sample(&mut self, wire: u64, paused: bool, now: Instant) {
        let dt = now.duration_since(self.sampled_at).as_secs_f64();
        if dt < 0.05 {
            return;
        }
        let new = wire.saturating_sub(self.sampled_wire);
        self.sampled_at = now;
        self.sampled_wire = wire;
        if paused {
            // Nothing is read while paused; resuming starts afresh.
            self.restart();
            return;
        }
        if new > 0 {
            self.last_flow = now;
            self.flowing_since.get_or_insert(now);
        } else if now.duration_since(self.last_flow) >= STALL {
            // Offline, or the server went quiet: no phantom speed trailing
            // off, and an ETA only once bytes flow steadily again.
            self.restart();
            return;
        }
        let instant = new as f64 / dt;
        self.bps = if self.bps == 0.0 {
            instant
        } else {
            let alpha = 1.0 - (-dt / SPEED_TAU_SECS).exp();
            self.bps + alpha * (instant - self.bps)
        };
        // Let the average settle to a true zero instead of trailing off.
        if self.bps < 1.0 {
            self.bps = 0.0;
        }
    }

    /// Seconds to move `remaining` bytes at this speed: only once bytes have
    /// flowed steadily for [`ETA_SETTLE`], and only when it is under a day
    /// at a speed above [`ETA_MIN_SPEED`].
    fn eta_secs(&self, remaining: u64, now: Instant) -> Option<u64> {
        let settled = self
            .flowing_since
            .is_some_and(|since| now.duration_since(since) >= ETA_SETTLE);
        if !settled || remaining == 0 || self.bps < ETA_MIN_SPEED {
            return None;
        }
        let secs = (remaining as f64 / self.bps).ceil();
        (secs <= ETA_MAX_SECS).then_some(secs as u64)
    }
}

impl JobCtx {
    pub(crate) fn new(observer: Arc<dyn JobObserver>) -> Arc<Self> {
        let (pause_tx, _) = watch::channel(false);
        Arc::new(Self {
            cancel: CancellationToken::new(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            pause_tx,
            observer,
            gate: Mutex::new(None),
            finished: AtomicBool::new(false),
            progress: Mutex::new(ProgressState {
                phase: None,
                bytes_done: 0,
                bytes_total: 0,
                files_done: 0,
                files_total: 0,
                fraction: None,
                detail: None,
                damaged_blocks: 0,
                speed: Speed::new(Instant::now()),
            }),
            wire_bytes: AtomicU64::new(0),
            live_counters: Mutex::new(Vec::new()),
            articles_total: AtomicU64::new(0),
            articles_failed: AtomicU64::new(0),
            connection_failures: AtomicU64::new(0),
            connection_error: Mutex::new(None),
        })
    }

    /// A context nobody observes, pauses or stops: for the low-level
    /// `Downloader`/`PostProcessor` entry points used outside a job.
    pub(crate) fn detached() -> Arc<Self> {
        Self::new(Arc::new(NullObserver))
    }

    // --- cancellation ---------------------------------------------------

    pub(crate) fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Release);
        self.cancel.cancel();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Resolves once the job is stopped; for `select!`.
    pub(crate) fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.cancel.cancelled()
    }

    /// The flag blocking code polls (handed to par2-rs and the unrar loop).
    pub(crate) fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel_flag.clone()
    }

    // --- pause -----------------------------------------------------------

    /// The next tick (within 250 ms) reports the change. Deliberately not
    /// emitted here: an observer may call `pause()` from inside `on_event`,
    /// and emitting would re-enter the gate it is holding.
    pub(crate) fn set_paused(&self, paused: bool) {
        self.pause_tx.send_replace(paused);
    }

    pub(crate) fn is_paused(&self) -> bool {
        *self.pause_tx.borrow()
    }

    pub(crate) fn pause_rx(&self) -> watch::Receiver<bool> {
        self.pause_tx.subscribe()
    }

    // --- statistics ------------------------------------------------------

    pub(crate) fn add_wire_bytes(&self, n: u64) {
        if n > 0 {
            self.wire_bytes.fetch_add(n, Ordering::Relaxed);
        }
    }

    pub(crate) fn wire_bytes(&self) -> u64 {
        self.wire_bytes.load(Ordering::Relaxed)
    }

    /// Count `counter` (a connection's bytes-read tally, which its holder
    /// also empties into [`add_wire_bytes`](Self::add_wire_bytes)) at every
    /// tick until [`detach_counter`](Self::detach_counter).
    pub(crate) fn attach_counter(&self, counter: Arc<AtomicU64>) {
        lock(&self.live_counters).push(counter);
    }

    /// Stop counting `counter`, taking what it holds.
    pub(crate) fn detach_counter(&self, counter: &Arc<AtomicU64>) {
        lock(&self.live_counters).retain(|c| !Arc::ptr_eq(c, counter));
        self.add_wire_bytes(counter.swap(0, Ordering::Relaxed));
    }

    /// Take the bytes the attached connections have read so far.
    fn gather_live_bytes(&self) {
        let taken: u64 = lock(&self.live_counters)
            .iter()
            .map(|c| c.swap(0, Ordering::Relaxed))
            .sum();
        self.add_wire_bytes(taken);
    }

    pub(crate) fn add_articles_total(&self, n: u64) {
        self.articles_total.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn add_articles_failed(&self, n: u64) {
        self.articles_failed.fetch_add(n, Ordering::Relaxed);
    }

    /// Count the articles of files an earlier session settled (reported
    /// from the job record, not downloaded again).
    pub(crate) fn add_recorded(&self, results: &[DownloadResult]) {
        for r in results {
            self.add_articles_total(r.segments_total as u64);
            self.add_articles_failed(r.segments_failed as u64);
        }
    }

    pub(crate) fn articles_total(&self) -> u64 {
        self.articles_total.load(Ordering::Relaxed)
    }

    pub(crate) fn articles_failed(&self) -> u64 {
        self.articles_failed.load(Ordering::Relaxed)
    }

    /// Count failed articles that failed only on the connection (they are
    /// also in [`articles_failed`](Self::articles_failed)).
    pub(crate) fn add_connection_failures(&self, n: u64) {
        self.connection_failures.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn connection_failures(&self) -> u64 {
        self.connection_failures.load(Ordering::Relaxed)
    }

    pub(crate) fn record_connection_error(&self, e: &DlNzbError) {
        *lock(&self.connection_error) = Some((e.kind(), e.user_message()));
    }

    pub(crate) fn connection_error(&self) -> Option<(ErrorKind, String)> {
        lock(&self.connection_error).clone()
    }

    // --- progress --------------------------------------------------------

    /// Enter `phase`: resets the per-phase counters and emits `Phase` followed
    /// by one `Progress`. Re-entering the current phase is a no-op.
    pub(crate) fn set_phase(&self, phase: JobPhase) {
        self.enter_phase(phase, |_| {});
    }

    /// Enter a download phase with its totals, counting what a previous
    /// session already left on disk as done, so the phase's first `Progress`
    /// starts where the job was (speed and ETA only ever see new bytes).
    pub(crate) fn set_download_phase(
        &self,
        phase: JobPhase,
        bytes_done: u64,
        bytes_total: u64,
        files_done: u32,
        files_total: u32,
    ) {
        self.enter_phase(phase, |p| {
            p.bytes_done = bytes_done;
            p.bytes_total = bytes_total;
            p.files_done = files_done;
            p.files_total = files_total;
        });
    }

    fn enter_phase(&self, phase: JobPhase, init: impl FnOnce(&mut ProgressState)) {
        // The gate is held while the phase changes, so nothing (the ticker
        // included) can report progress in the new phase before its `Phase`.
        let mut last_progress = lock(&self.gate);
        {
            let mut p = lock(&self.progress);
            if p.phase == Some(phase) {
                return;
            }
            p.phase = Some(phase);
            p.bytes_done = 0;
            p.bytes_total = 0;
            p.files_done = 0;
            p.files_total = 0;
            p.fraction = None;
            p.detail = None;
            // Repairing continues the count verification found.
            if phase != JobPhase::Repairing {
                p.damaged_blocks = 0;
            }
            p.speed.restart();
            init(&mut p);
        }
        if self.is_finished() {
            return;
        }
        self.deliver(JobEvent::Phase(phase));
        if let Some(snapshot) = self.snapshot() {
            *last_progress = Some(snapshot.clone());
            self.deliver(JobEvent::Progress(snapshot));
        }
    }

    fn with_progress(&self, f: impl FnOnce(&mut ProgressState)) {
        f(&mut lock(&self.progress));
    }

    pub(crate) fn add_bytes_done(&self, n: u64) {
        self.with_progress(|p| p.bytes_done = p.bytes_done.saturating_add(n));
    }

    pub(crate) fn add_files_done(&self, n: u32) {
        self.with_progress(|p| p.files_done = p.files_done.saturating_add(n));
    }

    pub(crate) fn set_bytes(&self, done: u64, total: u64) {
        self.with_progress(|p| {
            p.bytes_done = done;
            p.bytes_total = total;
        });
    }

    pub(crate) fn set_fraction(&self, fraction: f64) {
        self.with_progress(|p| p.fraction = Some(fraction.clamp(0.0, 1.0)));
    }

    pub(crate) fn set_detail(&self, detail: Option<String>) {
        self.with_progress(|p| p.detail = detail);
    }

    pub(crate) fn set_damaged_blocks(&self, blocks: u64) {
        self.with_progress(|p| p.damaged_blocks = blocks);
    }

    /// The current progress, or `None` before the first phase.
    fn snapshot(&self) -> Option<JobProgress> {
        let paused = self.is_paused();
        let articles_failed = self.articles_failed();
        let p = lock(&self.progress);
        let phase = p.phase?;
        let downloading = phase.is_download();
        let active = downloading && !paused;
        let speed_bps = if active { p.speed.bps } else { 0.0 };
        let remaining = p.bytes_total.saturating_sub(p.bytes_done);
        let eta_secs = if active {
            p.speed.eta_secs(remaining, Instant::now())
        } else {
            None
        };
        let fraction = p
            .fraction
            .unwrap_or_else(|| {
                if p.bytes_total > 0 {
                    p.bytes_done as f64 / p.bytes_total as f64
                } else {
                    0.0
                }
            })
            .clamp(0.0, 1.0);
        Some(JobProgress {
            phase,
            bytes_done: p.bytes_done,
            bytes_total: p.bytes_total,
            speed_bps,
            eta_secs,
            files_done: p.files_done,
            files_total: p.files_total,
            articles_failed,
            fraction,
            detail: p.detail.clone(),
            paused: paused && downloading,
            damaged_blocks: if matches!(phase, JobPhase::Verifying | JobPhase::Repairing) {
                p.damaged_blocks
            } else {
                0
            },
        })
    }

    /// Update the speed average, then emit a `Progress` if anything changed.
    pub(crate) fn tick(&self) {
        self.gather_live_bytes();
        let wire = self.wire_bytes();
        let paused = self.is_paused();
        self.with_progress(|p| p.speed.sample(wire, paused, Instant::now()));
        self.emit_progress_if_changed();
    }

    /// Emit a `Progress` now if anything changed since the last one; used at
    /// the end of a download phase so its final (100%) state is always seen.
    pub(crate) fn emit_progress_if_changed(&self) {
        let mut last_progress = lock(&self.gate);
        if self.is_finished() {
            return;
        }
        let Some(snapshot) = self.snapshot() else {
            return;
        };
        if last_progress.as_ref() == Some(&snapshot) {
            return;
        }
        *last_progress = Some(snapshot.clone());
        self.deliver(JobEvent::Progress(snapshot));
    }

    /// Run [`tick`](Self::tick) at 4 Hz until the returned guard is dropped.
    pub(crate) fn spawn_ticker(self: &Arc<Self>) -> AbortOnDrop {
        let weak = Arc::downgrade(self);
        AbortOnDrop(tokio::spawn(async move {
            let mut interval = tokio::time::interval(TICK);
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            interval.tick().await; // the first tick fires immediately
            loop {
                interval.tick().await;
                match weak.upgrade() {
                    Some(ctx) => ctx.tick(),
                    None => return,
                }
            }
        }))
    }

    // --- other events ----------------------------------------------------

    fn emit(&self, event: JobEvent) {
        let _gate = lock(&self.gate);
        if self.is_finished() {
            return;
        }
        if matches!(event, JobEvent::Finished(_)) {
            self.finished.store(true, Ordering::Release);
        }
        self.deliver(event);
    }

    /// Hand `event` to the observer (the caller holds the gate). A panic in
    /// the observer is contained: the job and its later events carry on.
    fn deliver(&self, event: JobEvent) {
        if catch_unwind(AssertUnwindSafe(|| self.observer.on_event(event))).is_err() {
            tracing::error!("a job observer panicked handling an event");
        }
    }

    pub(crate) fn warn(&self, message: impl Into<String>) {
        let message = message.into();
        tracing::debug!("job warning: {message}");
        self.emit(JobEvent::Warning(message));
    }

    pub(crate) fn availability(&self, info: AvailabilityInfo) {
        self.emit(JobEvent::Availability(info));
    }

    /// Deliver `Finished`. Later events of any kind are dropped.
    pub(crate) fn finish(&self, summary: JobSummary) {
        self.emit(JobEvent::Finished(summary));
    }

    /// `Finished` is being or has been delivered.
    pub(crate) fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

/// Lock `mutex`, also after a panic elsewhere poisoned it: none of the
/// context's state can be left half-updated by one.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Aborts a spawned task when dropped.
pub(crate) struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Outcome;
    use std::sync::mpsc;

    /// Records events; blocks inside the warning "hold" until released.
    struct Holding {
        entered: Mutex<mpsc::Sender<()>>,
        release: Mutex<mpsc::Receiver<()>>,
        events: Mutex<Vec<JobEvent>>,
    }

    impl JobObserver for Holding {
        fn on_event(&self, event: JobEvent) {
            let hold = event == JobEvent::Warning("hold".into());
            self.events.lock().unwrap().push(event);
            if hold {
                self.entered.lock().unwrap().send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
        }
    }

    /// The ticker can't report progress in a phase before the phase itself
    /// was announced (it used to slip in between).
    #[test]
    fn a_phase_is_announced_before_any_progress_in_it() {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let observer = Arc::new(Holding {
            entered: Mutex::new(entered_tx),
            release: Mutex::new(release_rx),
            events: Mutex::default(),
        });
        let ctx = JobCtx::new(observer.clone());
        ctx.set_phase(JobPhase::Downloading);

        // An event is being delivered (the observer holds the gate)...
        let holder = {
            let ctx = ctx.clone();
            std::thread::spawn(move || ctx.warn("hold"))
        };
        entered.recv().unwrap();
        // ...while the job enters Verifying and the ticker fires.
        let mover = {
            let ctx = ctx.clone();
            std::thread::spawn(move || ctx.set_phase(JobPhase::Verifying))
        };
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            ctx.snapshot().map(|p| p.phase),
            Some(JobPhase::Downloading),
            "the phase changed before it could be announced"
        );
        let ticker = {
            let ctx = ctx.clone();
            std::thread::spawn(move || ctx.emit_progress_if_changed())
        };
        std::thread::sleep(Duration::from_millis(20));
        release.send(()).unwrap();
        for thread in [holder, mover, ticker] {
            thread.join().unwrap();
        }

        let events = observer.events.lock().unwrap().clone();
        let announced = events
            .iter()
            .position(|e| *e == JobEvent::Phase(JobPhase::Verifying))
            .expect("Verifying announced");
        assert!(
            !events[..announced]
                .iter()
                .any(|e| matches!(e, JobEvent::Progress(p) if p.phase == JobPhase::Verifying)),
            "{events:?}"
        );
    }

    /// `speed` sampled every 250 ms from `from` for `secs`, `bps` bytes per
    /// second arriving; returns the time of the last sample.
    fn flow(speed: &mut Speed, wire: &mut u64, from: Instant, secs: f64, bps: u64) -> Instant {
        let mut now = from;
        let ticks = (secs * 4.0).round() as u32;
        for _ in 0..ticks {
            now += TICK;
            *wire += bps / 4;
            speed.sample(*wire, false, now);
        }
        now
    }

    /// The ETA waits for the speed to settle after a start, and is never
    /// over a day: right after a resume or a reconnect the average was still
    /// low and the ETA read "2 days" or "4 days, 10 hr left".
    #[test]
    fn the_eta_waits_for_a_settled_speed() {
        let remaining = 600_000_000; // 600 MB to go
        let start = Instant::now();
        let mut speed = Speed::new(start);
        let mut wire = 0;

        // Starting: no ETA for the first seconds of transfer, then one from
        // the settled speed (6 MB/s: 100 s).
        let now = flow(&mut speed, &mut wire, start, 2.0, 6_000_000);
        assert_eq!(speed.eta_secs(remaining, now), None);
        let now = flow(&mut speed, &mut wire, now, 2.0, 6_000_000);
        let eta = speed.eta_secs(remaining, now).expect("an ETA once settled");
        assert!((95..=110).contains(&eta), "{eta}");

        // Paused, then resumed: the old speed is gone, and the ETA waits again.
        let mut now = now;
        for _ in 0..40 {
            now += TICK;
            speed.sample(wire, true, now);
        }
        assert_eq!(speed.bps, 0.0);
        let now = flow(&mut speed, &mut wire, now, 1.0, 6_000_000);
        assert_eq!(speed.eta_secs(remaining, now), None, "right after resuming");
        let now = flow(&mut speed, &mut wire, now, 3.0, 6_000_000);
        assert!(speed.eta_secs(remaining, now).is_some());

        // Offline: nothing arrives. The speed drops to zero within the stall
        // time instead of trailing off into a days-long ETA...
        let mut now = now;
        let mut etas = Vec::new();
        for _ in 0..20 {
            now += TICK;
            speed.sample(wire, false, now);
            etas.extend(speed.eta_secs(remaining, now));
        }
        assert_eq!(speed.bps, 0.0);
        assert_eq!(speed.eta_secs(remaining, now), None);
        assert!(etas.iter().all(|&e| e <= 24 * 3600), "{etas:?}");
        // ...and back online, the ETA waits for the new speed to settle.
        let now = flow(&mut speed, &mut wire, now, 1.0, 6_000_000);
        assert_eq!(
            speed.eta_secs(remaining, now),
            None,
            "right after reconnecting"
        );
        let now = flow(&mut speed, &mut wire, now, 3.0, 6_000_000);
        let eta = speed.eta_secs(remaining, now).expect("an ETA once settled");
        assert!((95..=110).contains(&eta), "{eta}");

        // Over a day at a crawl: no ETA. Near zero: none either.
        let mut crawl = Speed::new(start);
        let mut wire = 0;
        let now = flow(&mut crawl, &mut wire, start, 10.0, 4_000);
        assert_eq!(crawl.eta_secs(remaining, now), None, "150,000 s");
        assert!(crawl.eta_secs(40_000_000, now).is_some(), "10,000 s");
        let mut trickle = Speed::new(start);
        let mut wire = 0;
        let now = flow(&mut trickle, &mut wire, start, 10.0, 400);
        assert_eq!(trickle.eta_secs(1_000, now), None);
    }

    /// The progress events carry the gate: no ETA in the first moments of a
    /// download phase.
    #[test]
    fn a_new_download_phase_reports_no_eta_at_first() {
        let ctx = JobCtx::new(Arc::new(NullObserver));
        ctx.set_download_phase(JobPhase::Downloading, 0, 1_000_000_000, 0, 1);
        ctx.add_wire_bytes(50_000_000);
        std::thread::sleep(Duration::from_millis(60));
        ctx.tick();
        let progress = ctx.snapshot().unwrap();
        assert!(progress.speed_bps > 0.0);
        assert_eq!(progress.eta_secs, None);
    }

    /// Panics on warnings.
    #[derive(Default)]
    struct Panicky(Mutex<Vec<JobEvent>>);

    impl JobObserver for Panicky {
        fn on_event(&self, event: JobEvent) {
            let warning = matches!(event, JobEvent::Warning(_));
            self.0.lock().unwrap().push(event);
            if warning {
                panic!("observer bug");
            }
        }
    }

    /// An observer that panics still gets `Finished` (the panic used to
    /// poison the gate, and every later event was dropped).
    #[test]
    fn an_observer_that_panics_still_gets_finished() {
        let observer = Arc::new(Panicky::default());
        let ctx = JobCtx::new(observer.clone());
        let _ = catch_unwind(AssertUnwindSafe(|| ctx.warn("boom")));
        ctx.set_phase(JobPhase::Downloading);
        ctx.finish(JobSummary::new(Outcome::Completed, "/job".into()));
        assert!(ctx.is_finished());
        let events = observer.0.lock().unwrap();
        assert!(
            matches!(events.last(), Some(JobEvent::Finished(_))),
            "{events:?}"
        );
        assert!(events.contains(&JobEvent::Phase(JobPhase::Downloading)));
    }
}
