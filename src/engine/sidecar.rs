//! The resume sidecar: `.dl-nzb-job.json` in a job folder.
//!
//! It records, for the NZB identified by its fingerprint, which articles of
//! each file are already written into `<name>.partial`, which files were
//! finalized (renamed to their final name), which download phases finished,
//! whether PAR2 verified the folder (with a size/mtime stamp of every file so
//! a later run can tell nothing changed since), and which archives were
//! extracted. `start()` on a folder whose sidecar matches the NZB continues
//! from there; `reprocess()` uses it to skip a PAR2 verification that already
//! passed and archives already extracted.
//!
//! Recording is cheap on the hot path: the writer task (the only one that
//! learns an article is on disk) sets a bit under an uncontended mutex. The
//! file is written by a debounced saver task (every [`SAVE_EVERY`] or every
//! [`SAVE_AFTER_SEGMENTS`] articles, whichever comes first), on pause, at
//! phase boundaries, and when the job ends; always to a temporary file that is
//! then renamed over the old one, so a crash leaves either the previous or the
//! new version, never a torn one. The sidecar may lag the data (a crash loses
//! at most the articles since the last save, which are fetched again); it is
//! never ahead of it, because an article is recorded only after its write to
//! the `.partial` returned.
//!
//! Durability: data files are only flushed to stable storage before a save
//! when `tuning.fsync_on_finalize` is on. Otherwise a power loss (not a
//! process crash) could leave recorded articles unwritten; resumed files are
//! therefore never trusted as wire-verified, so PAR2 verifies them for real.

use std::collections::HashMap;
use std::fs::File as StdFile;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::context::JobCtx;
use crate::download::{DownloadResult, Nzb};

/// The sidecar's file name inside the job folder (hidden, so it is never
/// listed as output).
pub(crate) const FILE_NAME: &str = ".dl-nzb-job.json";
const TEMP_NAME: &str = ".dl-nzb-job.json.tmp";
/// Bumped when the format changes incompatibly; an unknown version is treated
/// as unreadable (fresh job).
const FORMAT_VERSION: u32 = 1;

/// Save at least this often while articles are being recorded.
const SAVE_EVERY: Duration = Duration::from_secs(2);
/// ... or after this many newly recorded articles, whichever comes first.
const SAVE_AFTER_SEGMENTS: usize = 1024;
/// Never save more often than this.
const SAVE_MIN_GAP: Duration = Duration::from_millis(250);

/// A stable fingerprint of an NZB's content: every file's assigned name and
/// every segment's number, size and message id, in parsed order. Parsing
/// normalises the XML (files sorted by subject, segments by number), so the
/// fingerprint survives whitespace and formatting changes but not a different
/// file or article list.
pub(crate) fn fingerprint(nzb: &Nzb) -> String {
    fn field(bytes: &[u8], buf: &mut Vec<u8>) {
        buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(bytes);
    }
    let mut buf: Vec<u8> = Vec::with_capacity(64 + nzb.total_segments() * 64);
    buf.extend_from_slice(b"dl-nzb nzb fingerprint 1\n");
    buf.extend_from_slice(&(nzb.files().len() as u64).to_le_bytes());
    for file in nzb.files() {
        field(file.filename.as_bytes(), &mut buf);
        buf.extend_from_slice(&(file.segments.len() as u64).to_le_bytes());
        for seg in &file.segments {
            buf.extend_from_slice(&seg.number.to_le_bytes());
            buf.extend_from_slice(&seg.bytes.to_le_bytes());
            field(seg.message_id.as_bytes(), &mut buf);
        }
    }
    par2_rs::hash::compute_md5(&buf)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// --- On-disk format ----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Doc {
    version: u32,
    /// [`fingerprint`] of the NZB this folder belongs to.
    nzb: String,
    /// The data phase (data files plus the PAR2 index, or everything when
    /// recovery is not deferred) ran to the end.
    #[serde(default)]
    data_done: bool,
    #[serde(default)]
    recovery: Recovery,
    files: Vec<FileDoc>,
    /// Set when PAR2 verified (or repaired) the folder: the folder's files as
    /// the job left them. Valid while every one is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    par2_verified: Option<Vec<Stamp>>,
    /// First volumes of the archives extracted so far (a stop between two
    /// archives resumes with the next one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extracted: Vec<String>,
    /// The job's name (`JobRequest::title`, else the NZB's): what renaming
    /// calls the main file, also when `reprocess` runs without the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileDoc {
    /// The assigned file name (see `NzbFile::filename`).
    name: String,
    /// Articles the NZB lists for the file.
    segments: u32,
    /// Positions (0-based, in the NZB's segment order) of articles written to
    /// the file, as inclusive `[first, last]` ranges.
    #[serde(default)]
    done: Vec<[u32; 2]>,
    /// Positions of articles that were given up (missing, damaged).
    #[serde(default)]
    failed: Vec<[u32; 2]>,
    /// Highest byte written (the decoded size once complete).
    #[serde(default)]
    max_byte: u64,
    /// Renamed from `.partial` to its final name.
    #[serde(default)]
    finalized: bool,
}

/// Whether the deferred PAR2 recovery volumes were needed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Recovery {
    /// Not decided yet (the data phase hasn't finished).
    #[default]
    Pending,
    /// The data arrived intact (or recovery isn't deferred): nothing to fetch.
    NotNeeded,
    /// Fetched.
    Done,
}

/// A file as the job left it, to tell whether it changed since.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    name: String,
    size: u64,
    /// Modification time, nanoseconds since the Unix epoch.
    mtime_ns: u64,
}

// --- Segment sets --------------------------------------------------------------

/// A set of segment positions in `0..len`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SegmentSet {
    words: Vec<u64>,
    len: u32,
    count: u32,
}

impl SegmentSet {
    pub(crate) fn new(len: u32) -> Self {
        Self {
            words: vec![0; (len as usize).div_ceil(64)],
            len,
            count: 0,
        }
    }

    pub(crate) fn insert(&mut self, i: u32) {
        if i >= self.len {
            return;
        }
        let (w, b) = ((i / 64) as usize, i % 64);
        if self.words[w] & (1 << b) == 0 {
            self.words[w] |= 1 << b;
            self.count += 1;
        }
    }

    pub(crate) fn contains(&self, i: u32) -> bool {
        i < self.len && self.words[(i / 64) as usize] & (1 << (i % 64)) != 0
    }

    /// Number of positions in the set.
    pub(crate) fn count(&self) -> u32 {
        self.count
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn clear(&mut self) {
        self.words.iter_mut().for_each(|w| *w = 0);
        self.count = 0;
    }

    /// The set as inclusive `[first, last]` runs, in order. Read a word (64
    /// positions) at a time: an empty or full word takes one step, so a save
    /// (which holds the record's lock meanwhile) stays quick on a big NZB.
    /// Positions past `len` are never set.
    fn ranges(&self) -> Vec<[u32; 2]> {
        let mut out = Vec::new();
        // Where the run being read began.
        let mut start: Option<u32> = None;
        for (w, &word) in self.words.iter().enumerate() {
            let base = w as u32 * 64;
            let mut bit = 0;
            while bit < 64 {
                // The word from `bit` on (zeros shifted in at the top).
                let rest = word >> bit;
                match start {
                    None if rest == 0 => break,
                    None => {
                        bit += rest.trailing_zeros();
                        start = Some(base + bit);
                    }
                    Some(s) => {
                        bit += rest.trailing_ones();
                        if bit < 64 {
                            out.push([s, base + bit - 1]);
                            start = None;
                        }
                    }
                }
            }
        }
        if let Some(s) = start {
            out.push([s, self.len - 1]);
        }
        out
    }

    /// `None` when a range is reversed or out of bounds.
    fn from_ranges(len: u32, ranges: &[[u32; 2]]) -> Option<Self> {
        let mut set = Self::new(len);
        for &[first, last] in ranges {
            if first > last || last >= len {
                return None;
            }
            for i in first..=last {
                set.insert(i);
            }
        }
        Some(set)
    }
}

// --- The live record -----------------------------------------------------------

/// How [`JobRecord::open`] found the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Opened {
    /// No sidecar: a fresh job.
    Fresh,
    /// A sidecar for this NZB: the job continues.
    Resumed,
    /// A sidecar for a different NZB: ignored, the job starts fresh.
    Mismatch,
    /// A sidecar that could not be read: ignored, the job starts fresh. The
    /// reason, as a plain clause: "it is damaged".
    Unreadable(String),
}

/// What a previous session left for one file.
#[derive(Debug, Clone)]
pub(crate) struct Prior {
    pub finalized: bool,
    pub done: SegmentSet,
    pub max_byte: u64,
}

/// The job's resume record: the sidecar's content, kept up to date in memory
/// and saved to the job folder.
pub(crate) struct JobRecord {
    path: PathBuf,
    /// File name -> position in `State::files` (NZB order). Fixed at open.
    slots: HashMap<String, usize>,
    state: Mutex<State>,
    /// Something changed since the last save.
    dirty: AtomicBool,
    /// Articles recorded since the last save.
    unsaved: AtomicUsize,
    wake: Notify,
    /// Serialises saves (and removal) so an older snapshot can never be
    /// renamed over a newer one.
    save_lock: Mutex<()>,
    /// A save of this record (or the sidecar it was loaded from) is on disk.
    on_disk: AtomicBool,
    /// Removed for good (the job completed): later saves do nothing.
    removed: AtomicBool,
    /// Flush written data to stable storage before each save.
    fsync: bool,
}

struct State {
    fingerprint: String,
    data_done: bool,
    recovery: Recovery,
    files: Vec<FileRec>,
    par2_verified: Option<Vec<Stamp>>,
    extracted: Vec<String>,
    title: Option<String>,
}

struct FileRec {
    name: String,
    segments: u32,
    done: SegmentSet,
    failed: SegmentSet,
    max_byte: u64,
    finalized: bool,
    /// The open `.partial`, to flush before a save (only with `fsync`).
    handle: Option<Arc<StdFile>>,
    unsynced: bool,
}

impl FileRec {
    fn new(name: String, segments: u32) -> Self {
        Self {
            name,
            segments,
            done: SegmentSet::new(segments),
            failed: SegmentSet::new(segments),
            max_byte: 0,
            finalized: false,
            handle: None,
            unsynced: false,
        }
    }

    fn reset(&mut self) {
        self.done.clear();
        self.failed.clear();
        self.max_byte = 0;
        self.finalized = false;
        self.unsynced = false;
    }
}

impl JobRecord {
    /// The record for `nzb` in `dir`: the folder's sidecar when it belongs to
    /// this NZB, otherwise a fresh record (nothing is written until the first
    /// save, so a stale sidecar stays until then).
    pub(crate) fn open(dir: &Path, nzb: &Nzb, fsync: bool) -> (Arc<Self>, Opened) {
        let path = dir.join(FILE_NAME);
        let fingerprint = fingerprint(nzb);
        let fresh_files = || {
            nzb.files()
                .iter()
                .map(|f| FileRec::new(f.filename.clone(), f.segments.len() as u32))
                .collect::<Vec<_>>()
        };
        let fresh = |opened: Opened| {
            let state = State {
                fingerprint: fingerprint.clone(),
                data_done: false,
                recovery: Recovery::Pending,
                files: fresh_files(),
                par2_verified: None,
                extracted: Vec::new(),
                title: None,
            };
            (
                Arc::new(Self::new(path.clone(), state, false, fsync)),
                opened,
            )
        };

        let doc = match read_doc(&path) {
            Ok(None) => return fresh(Opened::Fresh),
            Ok(Some(doc)) => doc,
            Err(reason) => return fresh(Opened::Unreadable(reason)),
        };
        if doc.nzb != fingerprint {
            return fresh(Opened::Mismatch);
        }
        // Same fingerprint, so the same files in the same order; checked
        // anyway so a hand-edited sidecar can't index out of bounds.
        let consistent = doc.files.len() == nzb.files().len()
            && doc
                .files
                .iter()
                .zip(nzb.files())
                .all(|(d, f)| d.name == f.filename && d.segments as usize == f.segments.len());
        if !consistent {
            return fresh(Opened::Unreadable(
                "it does not match the NZB's file list".into(),
            ));
        }
        match State::from_doc(doc) {
            Some(state) => (
                Arc::new(Self::new(path.clone(), state, true, fsync)),
                Opened::Resumed,
            ),
            None => fresh(Opened::Unreadable("it is damaged".into())),
        }
    }

    /// The sidecar already in `dir`, whatever NZB it belongs to (for
    /// reprocessing, which has no NZB). `None` when there is none or it can't
    /// be read.
    pub(crate) fn open_existing(dir: &Path) -> Option<Arc<Self>> {
        let path = dir.join(FILE_NAME);
        let doc = read_doc(&path).ok()??;
        let state = State::from_doc(doc)?;
        Some(Arc::new(Self::new(path, state, true, false)))
    }

    fn new(path: PathBuf, state: State, on_disk: bool, fsync: bool) -> Self {
        let slots = state
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.clone(), i))
            .collect();
        Self {
            path,
            slots,
            state: Mutex::new(state),
            dirty: AtomicBool::new(false),
            unsaved: AtomicUsize::new(0),
            wake: Notify::new(),
            save_lock: Mutex::new(()),
            on_disk: AtomicBool::new(on_disk),
            removed: AtomicBool::new(false),
            fsync,
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> Option<R> {
        self.state.lock().ok().map(|mut s| f(&mut s))
    }

    fn changed(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    /// Something changed that should reach the disk soon (a file finalized, a
    /// phase ended): wake the saver.
    fn changed_now(&self) {
        self.changed();
        self.wake.notify_one();
    }

    // --- Queries ---------------------------------------------------------

    /// The record's position for a file of the NZB.
    pub(crate) fn slot(&self, name: &str) -> Option<usize> {
        self.slots.get(name).copied()
    }

    /// What earlier sessions left for a file.
    pub(crate) fn prior(&self, slot: usize) -> Option<Prior> {
        self.with_state(|s| {
            s.files.get(slot).map(|f| Prior {
                finalized: f.finalized,
                done: f.done.clone(),
                max_byte: f.max_byte,
            })
        })
        .flatten()
    }

    /// Whether the record holds any downloaded data.
    pub(crate) fn has_progress(&self) -> bool {
        self.with_state(|s| s.data_done || s.files.iter().any(|f| !f.done.is_empty()))
            .unwrap_or(false)
    }

    pub(crate) fn data_done(&self) -> bool {
        self.with_state(|s| s.data_done).unwrap_or(false)
    }

    pub(crate) fn recovery(&self) -> Recovery {
        self.with_state(|s| s.recovery).unwrap_or_default()
    }

    /// Every download phase finished: only post-processing is left.
    pub(crate) fn download_complete(&self) -> bool {
        self.with_state(|s| s.data_done && s.recovery != Recovery::Pending)
            .unwrap_or(false)
    }

    /// Every named file was dealt with: finalized, or given up (a file none
    /// of whose articles could be fetched is never finalized).
    pub(crate) fn all_settled<'a>(&self, mut names: impl Iterator<Item = &'a str>) -> bool {
        self.with_state(|s| {
            names.all(|name| {
                self.slot(name)
                    .and_then(|i| s.files.get(i))
                    .is_some_and(|f| f.finalized || !f.failed.is_empty())
            })
        })
        .unwrap_or(false)
    }

    /// Download results for the recorded files that pass `keep(name,
    /// attempted)`, in NZB order, as a finished download would have reported
    /// them. `attempted`: some article of the file settled (a file of a phase
    /// that never ran has none). Their wire checksums aren't known any more,
    /// so PAR2 verifies them for real.
    pub(crate) fn results(
        &self,
        dir: &Path,
        keep: impl Fn(&str, bool) -> bool,
    ) -> Vec<DownloadResult> {
        self.with_state(|s| {
            s.files
                .iter()
                .filter(|f| {
                    let attempted = f.finalized || !f.done.is_empty() || !f.failed.is_empty();
                    keep(&f.name, attempted)
                })
                .map(|f| {
                    let path = dir.join(&f.name);
                    let size = std::fs::metadata(&path)
                        .map(|m| m.len())
                        .unwrap_or(f.max_byte);
                    DownloadResult {
                        filename: f.name.clone(),
                        path,
                        size,
                        segments_total: f.segments as usize,
                        segments_downloaded: f.done.count() as usize,
                        segments_failed: f.segments.saturating_sub(f.done.count()) as usize,
                        all_segments_crc_verified: false,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
    }

    /// PAR2 verified the folder in an earlier run and none of its files has
    /// changed since (same names, sizes and modification times). Archive
    /// volumes deleted after their extraction no longer count.
    pub(crate) fn par2_still_verified(&self, dir: &Path) -> bool {
        let Some(stamps) = self.with_state(|s| s.par2_verified.clone()).flatten() else {
            return false;
        };
        stamps
            .iter()
            .all(|s| stamp(&dir.join(&s.name), &s.name).as_ref() == Some(s))
    }

    /// The job's name, as [`set_title`](Self::set_title) recorded it.
    pub(crate) fn title(&self) -> Option<String> {
        self.with_state(|s| s.title.clone()).flatten()
    }

    /// The archive whose first volume is `name` was extracted (in this run or
    /// an earlier one).
    pub(crate) fn archive_extracted(&self, name: &str) -> bool {
        self.with_state(|s| s.extracted.iter().any(|n| n == name))
            .unwrap_or(false)
    }

    /// The sidecar exists on disk (loaded, or saved by this job).
    pub(crate) fn on_disk(&self) -> bool {
        self.on_disk.load(Ordering::Acquire) && !self.removed.load(Ordering::Acquire)
    }

    // --- Recording -------------------------------------------------------

    /// Start a file over: forget everything recorded for it.
    pub(crate) fn reset_file(&self, slot: usize) {
        self.with_state(|s| {
            if let Some(f) = s.files.get_mut(slot) {
                f.reset();
            }
        });
        self.changed();
    }

    /// Continue a file from its written articles: its failed articles are
    /// tried again and it is no longer finalized.
    pub(crate) fn reopen_file(&self, slot: usize) {
        self.with_state(|s| {
            if let Some(f) = s.files.get_mut(slot) {
                f.failed.clear();
                f.finalized = false;
            }
        });
        self.changed();
    }

    /// The open `.partial` of a file, flushed before each save when the job
    /// syncs its writes.
    pub(crate) fn attach(&self, slot: usize, handle: Arc<StdFile>) {
        if !self.fsync {
            return;
        }
        self.with_state(|s| {
            if let Some(f) = s.files.get_mut(slot) {
                f.handle = Some(handle);
            }
        });
    }

    /// An article's data is in the file (the write returned), ending at byte
    /// `end`. Called by the writer only.
    pub(crate) fn written(&self, slot: usize, index: u32, end: u64) {
        self.with_state(|s| {
            if let Some(f) = s.files.get_mut(slot) {
                f.done.insert(index);
                f.max_byte = f.max_byte.max(end);
                f.unsynced = true;
            }
        });
        self.changed();
        if self.unsaved.fetch_add(1, Ordering::Relaxed) + 1 == SAVE_AFTER_SEGMENTS {
            self.wake.notify_one();
        }
    }

    /// An article was given up.
    pub(crate) fn failed(&self, slot: usize, index: u32) {
        self.with_state(|s| {
            if let Some(f) = s.files.get_mut(slot) {
                f.failed.insert(index);
            }
        });
        self.changed();
    }

    /// A file was renamed to its final name (or removed, when nothing of it
    /// could be written).
    pub(crate) fn finalized(&self, slot: usize) {
        self.with_state(|s| {
            if let Some(f) = s.files.get_mut(slot) {
                f.finalized = true;
                f.handle = None;
                f.unsynced = false;
            }
        });
        self.changed_now();
    }

    /// Record the job's name (saved with the next save).
    pub(crate) fn set_title(&self, title: &str) {
        let changed = self
            .with_state(|s| {
                let changed = s.title.as_deref() != Some(title);
                s.title = Some(title.to_string());
                changed
            })
            .unwrap_or(false);
        if changed {
            self.changed();
        }
    }

    pub(crate) fn set_data_done(&self) {
        self.with_state(|s| s.data_done = true);
        self.changed_now();
    }

    pub(crate) fn set_recovery(&self, recovery: Recovery) {
        self.with_state(|s| s.recovery = recovery);
        self.changed_now();
    }

    /// PAR2 verified the folder (`true`: stamp its files as they are now) or
    /// found it damaged beyond repair (`false`: forget an earlier verdict).
    pub(crate) fn set_par2_verified(&self, dir: &Path, verified: bool) {
        // No files to stamp (the folder can't be read): no verdict to keep.
        let stamps = verified
            .then(|| folder_stamps(dir))
            .filter(|stamps| !stamps.is_empty());
        self.with_state(|s| s.par2_verified = stamps);
        self.changed();
    }

    /// The archive whose first volume is `name` was extracted, and its
    /// volumes `deleting` are about to be deleted on purpose: they leave
    /// PAR2's verdict (their absence is not damage).
    pub(crate) fn set_archive_extracted(&self, name: &str, deleting: &[String]) {
        self.with_state(|s| {
            if !s.extracted.iter().any(|n| n == name) {
                s.extracted.push(name.to_string());
            }
            if let Some(stamps) = s.par2_verified.as_mut() {
                stamps.retain(|stamp| !deleting.contains(&stamp.name));
            }
        });
        self.changed();
    }

    /// The folder's file `from` was renamed to `to` (renaming by PAR2's
    /// file table or by the job's title): PAR2's verdict follows the file to
    /// its new name, so a resumed job still knows the file is the one PAR2
    /// verified. Only when the file now called `to` is the one stamped as
    /// `from` (same size and modification time, which a rename keeps);
    /// otherwise the stamp stays under the old name, which no longer matches
    /// anything, as for any other change. Returns whether a stamp moved.
    pub(crate) fn renamed(&self, dir: &Path, from: &str, to: &str) -> bool {
        let Some(now) = stamp(&dir.join(to), to) else {
            return false;
        };
        let moved = self
            .with_state(|s| {
                let Some(stamps) = s.par2_verified.as_mut() else {
                    return false;
                };
                let same = |st: &Stamp| {
                    st.name == from && st.size == now.size && st.mtime_ns == now.mtime_ns
                };
                if !stamps.iter().any(same) {
                    return false;
                }
                // Whatever `to` was before the rename, it is now this file.
                stamps.retain(|st| st.name != from && st.name != to);
                stamps.push(now);
                stamps.sort_by(|a, b| a.name.cmp(&b.name));
                true
            })
            .unwrap_or(false);
        if moved {
            self.changed();
        }
        moved
    }

    // --- Saving ----------------------------------------------------------

    /// Save now (off the async threads). Errors are returned for the caller
    /// to report; the record stays dirty so a later save tries again.
    pub(crate) async fn save(self: &Arc<Self>) -> std::io::Result<()> {
        let record = self.clone();
        tokio::task::spawn_blocking(move || record.save_blocking())
            .await
            .map_err(std::io::Error::other)?
    }

    /// [`save`](Self::save) on the calling thread, for blocking code.
    pub(crate) fn save_blocking(&self) -> std::io::Result<()> {
        let _guard = self.save_lock.lock().map_err(|_| poisoned())?;
        if self.removed.load(Ordering::Acquire) {
            return Ok(());
        }
        // Snapshot under the state lock (cheap), serialise outside it.
        let (doc, to_sync) = {
            let mut s = self.state.lock().map_err(|_| poisoned())?;
            self.dirty.store(false, Ordering::Release);
            self.unsaved.store(0, Ordering::Relaxed);
            let to_sync: Vec<Arc<StdFile>> = if self.fsync {
                s.files
                    .iter_mut()
                    .filter_map(|f| {
                        std::mem::take(&mut f.unsynced)
                            .then(|| f.handle.clone())
                            .flatten()
                    })
                    .collect()
            } else {
                Vec::new()
            };
            (s.to_doc(), to_sync)
        };
        let result = (|| {
            // The data the snapshot describes reaches the disk before the
            // sidecar that describes it.
            for file in &to_sync {
                file.sync_data()?;
            }
            let bytes = serde_json::to_vec(&doc).map_err(std::io::Error::other)?;
            let temp = self.path.with_file_name(TEMP_NAME);
            {
                let mut f = StdFile::create(&temp)?;
                f.write_all(&bytes)?;
                if self.fsync {
                    f.sync_all()?;
                }
            }
            std::fs::rename(&temp, &self.path)
        })();
        match result {
            Ok(()) => {
                self.on_disk.store(true, Ordering::Release);
                Ok(())
            }
            Err(e) => {
                // Try again next time, flushing every open file anew.
                self.with_state(|s| {
                    for f in s.files.iter_mut().filter(|f| f.handle.is_some()) {
                        f.unsynced = true;
                    }
                });
                self.dirty.store(true, Ordering::Release);
                Err(e)
            }
        }
    }

    /// Delete the sidecar for good: the job is complete.
    pub(crate) async fn remove(self: &Arc<Self>) {
        let record = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _guard = record.save_lock.lock();
            record.removed.store(true, Ordering::Release);
            let _ = std::fs::remove_file(&record.path);
            let _ = std::fs::remove_file(record.path.with_file_name(TEMP_NAME));
        })
        .await;
    }

    /// Save in the background while a download runs: every [`SAVE_EVERY`]
    /// while something changed, sooner after [`SAVE_AFTER_SEGMENTS`] articles
    /// or a finalized file, and right away when the job is paused. While
    /// paused it saves again within [`SAVE_MIN_GAP`] of any change: articles
    /// decoded just before the pause are written just after it, and a paused
    /// app may be suspended at any moment. (Requests a pause abandoned never
    /// reach the writer, so they are never recorded.) Abort the returned task
    /// when the download ends (then save once more).
    pub(crate) fn spawn_saver(self: &Arc<Self>, ctx: &Arc<JobCtx>) -> tokio::task::JoinHandle<()> {
        let record = self.clone();
        let mut pause_rx = ctx.pause_rx();
        tokio::spawn(async move {
            loop {
                let period = if *pause_rx.borrow() {
                    SAVE_MIN_GAP
                } else {
                    SAVE_EVERY
                };
                tokio::select! {
                    _ = tokio::time::sleep(period) => {}
                    _ = record.wake.notified() => {}
                    changed = pause_rx.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                }
                if record.dirty.load(Ordering::Acquire) {
                    if let Err(e) = record.save().await {
                        tracing::debug!("could not save resume data: {e}");
                    }
                    tokio::time::sleep(SAVE_MIN_GAP).await;
                }
            }
        })
    }
}

impl State {
    /// `None` when a range is out of bounds (`read_doc` checked the version).
    fn from_doc(doc: Doc) -> Option<Self> {
        let files = doc
            .files
            .into_iter()
            .map(|d| {
                Some(FileRec {
                    done: SegmentSet::from_ranges(d.segments, &d.done)?,
                    failed: SegmentSet::from_ranges(d.segments, &d.failed)?,
                    max_byte: d.max_byte,
                    finalized: d.finalized,
                    handle: None,
                    unsynced: false,
                    name: d.name,
                    segments: d.segments,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            fingerprint: doc.nzb,
            data_done: doc.data_done,
            recovery: doc.recovery,
            files,
            par2_verified: doc.par2_verified,
            extracted: doc.extracted,
            title: doc.title,
        })
    }

    fn to_doc(&self) -> Doc {
        Doc {
            version: FORMAT_VERSION,
            nzb: self.fingerprint.clone(),
            data_done: self.data_done,
            recovery: self.recovery,
            files: self
                .files
                .iter()
                .map(|f| FileDoc {
                    name: f.name.clone(),
                    segments: f.segments,
                    done: f.done.ranges(),
                    failed: f.failed.ranges(),
                    max_byte: f.max_byte,
                    finalized: f.finalized,
                })
                .collect(),
            par2_verified: self.par2_verified.clone(),
            extracted: self.extracted.clone(),
            title: self.title.clone(),
        }
    }
}

/// `Ok(None)`: no sidecar. `Err`: one that can't be read or parsed, with a
/// plain clause why ("it is damaged").
fn read_doc(path: &Path) -> Result<Option<Doc>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            tracing::debug!("could not read {}: {e}", path.display());
            return Err("it could not be read".into());
        }
    };
    let doc: Doc = serde_json::from_slice(&bytes).map_err(|_| "it is damaged".to_string())?;
    if doc.version != FORMAT_VERSION {
        return Err("it was written by a different version of dl-nzb".into());
    }
    Ok(Some(doc))
}

fn poisoned() -> std::io::Error {
    std::io::Error::other("resume record lock poisoned")
}

/// The job folder's files (top level, not hidden, not `.partial`) as they are now.
fn folder_stamps(dir: &Path) -> Vec<Stamp> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stamps: Vec<Stamp> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if name.starts_with('.') || name.ends_with(crate::patterns::PARTIAL_EXT) {
                return None;
            }
            stamp(&e.path(), &name)
        })
        .collect();
    stamps.sort_by(|a, b| a.name.cmp(&b.name));
    stamps
}

fn stamp(path: &Path, name: &str) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime_ns = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos()
        .min(u64::MAX as u128) as u64;
    Some(Stamp {
        name: name.to_string(),
        size: meta.len(),
        mtime_ns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nzb(ids: &[&str]) -> Nzb {
        let segments: String = ids
            .iter()
            .enumerate()
            .map(|(i, id)| format!(r#"<segment bytes="100" number="{}">{id}</segment>"#, i + 1))
            .collect();
        format!(
            r#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><file poster="p" date="1700000000" subject="&quot;a.bin&quot; yEnc (1/1)"><groups><group>a.b</group></groups><segments>{segments}</segments></file></nzb>"#
        )
        .parse()
        .unwrap()
    }

    #[test]
    fn segment_sets_round_trip_as_ranges() {
        let mut set = SegmentSet::new(130);
        for i in [0, 1, 2, 5, 64, 65, 66, 129] {
            set.insert(i);
        }
        set.insert(2); // already there
        set.insert(500); // out of range: ignored
        assert_eq!(set.count(), 8);
        let ranges = set.ranges();
        assert_eq!(ranges, vec![[0, 2], [5, 5], [64, 66], [129, 129]]);
        assert_eq!(SegmentSet::from_ranges(130, &ranges), Some(set));
        assert_eq!(SegmentSet::from_ranges(130, &[[3, 2]]), None);
        assert_eq!(SegmentSet::from_ranges(130, &[[0, 130]]), None);
    }

    /// Reading a word at a time gives exactly the runs reading position by
    /// position does, for sets of every density, scattered or in runs, and
    /// sizes on and around word edges.
    #[test]
    fn ranges_match_a_position_by_position_reading() {
        fn by_position(set: &SegmentSet) -> Vec<[u32; 2]> {
            let mut out = Vec::new();
            let mut start: Option<u32> = None;
            for i in 0..set.len {
                match (set.contains(i), start) {
                    (true, None) => start = Some(i),
                    (false, Some(s)) => {
                        out.push([s, i - 1]);
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                out.push([s, set.len - 1]);
            }
            out
        }
        // xorshift64: random enough, and the same every run.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move |below: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % u64::from(below.max(1))) as u32
        };
        for len in [
            0, 1, 2, 63, 64, 65, 127, 128, 129, 191, 192, 193, 1000, 4096,
        ] {
            for percent in [0, 1, 10, 50, 90, 99, 100] {
                for _ in 0..16 {
                    // Scattered positions...
                    let mut scattered = SegmentSet::new(len);
                    for i in 0..len {
                        if next(100) < percent {
                            scattered.insert(i);
                        }
                    }
                    // ... and runs of any length.
                    let mut runs = SegmentSet::new(len);
                    for _ in 0..next(8) {
                        let first = next(len);
                        let last = first.saturating_add(next(200)).min(len.saturating_sub(1));
                        (first..=last).for_each(|i| runs.insert(i));
                    }
                    for set in [scattered, runs] {
                        let ranges = set.ranges();
                        assert_eq!(ranges, by_position(&set), "{len} positions");
                        assert_eq!(SegmentSet::from_ranges(len, &ranges), Some(set));
                    }
                }
            }
        }
    }

    #[test]
    fn fingerprint_ignores_formatting_but_not_content() {
        let a = nzb(&["x1@t", "x2@t"]);
        let spaced: Nzb = r#"<?xml version="1.0"?>
            <nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
              <file poster="p" date="1700000000" subject="&quot;a.bin&quot; yEnc (1/1)">
                <groups> <group>a.b</group> </groups>
                <segments>
                  <segment number="2" bytes="100">x2@t</segment>
                  <segment number="1" bytes="100">x1@t</segment>
                </segments>
              </file>
            </nzb>"#
            .parse()
            .unwrap();
        assert_eq!(fingerprint(&a), fingerprint(&spaced));
        assert_ne!(fingerprint(&a), fingerprint(&nzb(&["x1@t", "x3@t"])));
    }

    #[tokio::test]
    async fn record_survives_a_save_and_rejects_another_nzb() {
        let dir = tempfile::tempdir().unwrap();
        let first = nzb(&["x1@t", "x2@t", "x3@t"]);
        let (record, opened) = JobRecord::open(dir.path(), &first, false);
        assert_eq!(opened, Opened::Fresh);
        assert!(!record.on_disk());
        let slot = record.slot("a.bin").unwrap();
        record.written(slot, 0, 100);
        record.written(slot, 2, 300);
        record.failed(slot, 1);
        record.set_data_done();
        record.save().await.unwrap();
        assert!(record.on_disk());

        let (again, opened) = JobRecord::open(dir.path(), &first, false);
        assert_eq!(opened, Opened::Resumed);
        let prior = again.prior(slot).unwrap();
        assert!(prior.done.contains(0) && !prior.done.contains(1) && prior.done.contains(2));
        assert_eq!(prior.max_byte, 300);
        assert!(again.data_done() && !again.download_complete());

        let (_, opened) = JobRecord::open(dir.path(), &nzb(&["y@t"]), false);
        assert_eq!(opened, Opened::Mismatch);

        std::fs::write(dir.path().join(FILE_NAME), b"{not json").unwrap();
        let (_, opened) = JobRecord::open(dir.path(), &first, false);
        assert!(matches!(opened, Opened::Unreadable(_)));

        again.remove().await;
        assert!(!dir.path().join(FILE_NAME).exists());
        // A save after removal leaves the folder clean.
        again.save().await.unwrap();
        assert!(!dir.path().join(FILE_NAME).exists());
    }

    #[test]
    fn par2_verdict_holds_only_while_files_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), b"hello").unwrap();
        std::fs::write(dir.path().join("a.par2"), b"par2").unwrap();
        let (record, _) = JobRecord::open(dir.path(), &nzb(&["x1@t"]), false);
        assert!(!record.par2_still_verified(dir.path()));
        record.set_par2_verified(dir.path(), true);
        assert!(record.par2_still_verified(dir.path()));
        // A new file doesn't matter; a changed one does.
        std::fs::write(dir.path().join("extra.txt"), b"x").unwrap();
        assert!(record.par2_still_verified(dir.path()));
        std::fs::write(dir.path().join("a.bin"), b"hello world").unwrap();
        assert!(!record.par2_still_verified(dir.path()));
        record.set_par2_verified(dir.path(), false);
        assert!(!record.par2_still_verified(dir.path()));
    }

    /// A file renamed after PAR2 verified it keeps the verdict under its new
    /// name; a file changed before the rename doesn't.
    #[tokio::test]
    async fn a_renamed_file_keeps_the_par2_verdict() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["B.bin", "c.bin", "x.par2"] {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        let first = nzb(&["x1@t"]);
        let (record, _) = JobRecord::open(dir.path(), &first, false);
        record.set_par2_verified(dir.path(), true);
        std::fs::rename(dir.path().join("B.bin"), dir.path().join("job.bin")).unwrap();
        assert!(!record.par2_still_verified(dir.path()));
        assert!(record.renamed(dir.path(), "B.bin", "job.bin"));
        assert!(record.par2_still_verified(dir.path()));
        record.save().await.unwrap();
        let (again, _) = JobRecord::open(dir.path(), &first, false);
        assert!(again.par2_still_verified(dir.path()));

        // Changed, then renamed: not the file PAR2 verified.
        std::fs::write(dir.path().join("c.bin"), b"something else").unwrap();
        std::fs::rename(dir.path().join("c.bin"), dir.path().join("d.bin")).unwrap();
        assert!(!again.renamed(dir.path(), "c.bin", "d.bin"));
        assert!(!again.par2_still_verified(dir.path()));
        // A file PAR2 never saw.
        assert!(!again.renamed(dir.path(), "nope.bin", "d.bin"));
    }

    /// Volumes deleted after their archive was extracted are not damage: the
    /// verdict stands without them, and the extraction is remembered.
    #[tokio::test]
    async fn deleting_an_extracted_archive_keeps_the_par2_verdict() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["A.part1.rar", "A.part2.rar", "B.rar", "x.par2"] {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        let first = nzb(&["x1@t"]);
        let (record, _) = JobRecord::open(dir.path(), &first, false);
        record.set_par2_verified(dir.path(), true);
        let volumes = ["A.part1.rar".to_string(), "A.part2.rar".to_string()];
        record.set_archive_extracted("A.part1.rar", &volumes);
        for name in &volumes {
            std::fs::remove_file(dir.path().join(name)).unwrap();
        }
        assert!(record.par2_still_verified(dir.path()));
        assert!(record.archive_extracted("A.part1.rar"));
        assert!(!record.archive_extracted("B.rar"));
        record.save().await.unwrap();

        let (again, opened) = JobRecord::open(dir.path(), &first, false);
        assert_eq!(opened, Opened::Resumed);
        assert!(again.par2_still_verified(dir.path()));
        assert!(again.archive_extracted("A.part1.rar"));
        // Anything else missing is still damage.
        std::fs::remove_file(dir.path().join("B.rar")).unwrap();
        assert!(!again.par2_still_verified(dir.path()));
    }
}
