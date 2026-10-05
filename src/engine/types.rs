//! Public data types of the engine API (`apple/CONTRACT.md` §1). Plain data:
//! no behaviour beyond small conveniences, so the FFI layer can mirror them
//! one-to-one as UniFFI records and enums.

use std::path::PathBuf;

pub use crate::error::ErrorKind;

/// Whether to `STAT` every article before downloading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Preflight {
    /// Scan only when the NZB has no PAR2 files, i.e. when learning up front
    /// that repair is impossible is the scan's only value. With PAR2 present,
    /// missing articles are found inline (430) and recovery is fetched on
    /// demand, so the scan would just delay the first byte.
    #[default]
    Auto,
    /// Always scan (gives an availability verdict before any data moves).
    Always,
    /// Never scan.
    Never,
}

/// What to do when the pre-flight scan says the download cannot be repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OnUnrepairable {
    /// Finish with [`Outcome::Unrepairable`] before downloading anything.
    #[default]
    Stop,
    /// Download anyway (the user chose "Download Anyway").
    Continue,
}

/// One job: an NZB downloaded into a folder. Printed with `{:?}`, its
/// passwords show as `<REDACTED>`.
#[derive(Clone, PartialEq, Eq)]
pub struct JobRequest {
    pub nzb_path: PathBuf,
    /// The exact, absolute job folder. The caller chooses and de-duplicates it;
    /// the engine creates it. Post-processing never touches anything outside it.
    pub output_dir: PathBuf,
    /// The user's passwords for encrypted RARs (newest first), tried in order
    /// before the NZB's own (`<meta type="password">`, then `{{password}}` in
    /// its title or file name). When every one of these is refused the job
    /// finishes `NeedsPassword` with "The password didn't work."
    pub passwords: Vec<String>,
    pub preflight: Preflight,
    pub on_unrepairable: OnUnrepairable,
    /// Bytes free for this job on the output folder's volume, as the caller
    /// sees them. When set, the free-space check uses it instead of asking
    /// the file system: on iOS `statvfs` leaves out purgeable space the system
    /// would free on demand, so the app passes
    /// `volumeAvailableCapacityForImportantUsage` instead. `None` everywhere
    /// else.
    pub free_space_hint: Option<u64>,
    /// The job's name, e.g. the release title the app shows. Renaming
    /// (`deobfuscate_file_names`) calls an obfuscated main file after it,
    /// made safe for a file name. `None`: the NZB's title
    /// ([`NzbInfo::title`]). Never the folder's name, which the caller may
    /// have de-duplicated ("Name 2").
    pub title: Option<String>,
}

// Archive passwords stay out of logs.
impl std::fmt::Debug for JobRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobRequest")
            .field("nzb_path", &self.nzb_path)
            .field("output_dir", &self.output_dir)
            .field("passwords", &vec!["<REDACTED>"; self.passwords.len()])
            .field("preflight", &self.preflight)
            .field("on_unrepairable", &self.on_unrepairable)
            .field("free_space_hint", &self.free_space_hint)
            .field("title", &self.title)
            .finish()
    }
}

impl JobRequest {
    /// A request with the default policies (`Preflight::Auto`,
    /// `OnUnrepairable::Stop`), no extra passwords, no free-space hint and
    /// the NZB's own title.
    pub fn new(nzb_path: impl Into<PathBuf>, output_dir: impl Into<PathBuf>) -> Self {
        Self {
            nzb_path: nzb_path.into(),
            output_dir: output_dir.into(),
            passwords: Vec::new(),
            preflight: Preflight::Auto,
            on_unrepairable: OnUnrepairable::Stop,
            free_space_hint: None,
            title: None,
        }
    }
}

/// Receives a job's events. Called from engine tasks and worker threads (PAR2
/// runs on a blocking/rayon thread), one event at a time and in order; keep it
/// quick (hand the event to a channel or the main thread). Calling the job's
/// [`JobHandle`](super::JobHandle) methods from inside `on_event` is fine.
pub trait JobObserver: Send + Sync {
    fn on_event(&self, event: JobEvent);
}

/// An observer that ignores every event.
pub struct NullObserver;

impl JobObserver for NullObserver {
    fn on_event(&self, _event: JobEvent) {}
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    /// The job entered a phase. Always followed by one `Progress`.
    Phase(JobPhase),
    /// At most 4 Hz while something changes, plus one on every phase change
    /// and one at the end of each download phase (its final, complete state).
    Progress(JobProgress),
    /// The verdict of a pre-flight scan.
    Availability(AvailabilityInfo),
    /// One plain-English sentence about something the user may want to know.
    Warning(String),
    /// Exactly once, always the last event.
    Finished(JobSummary),
}

/// Phases in the order they can occur; any may be skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum JobPhase {
    Connecting,
    Checking,
    Downloading,
    DownloadingRecovery,
    Verifying,
    Repairing,
    Extracting,
    Renaming,
}

impl JobPhase {
    /// The phases that move article data (and can be paused).
    pub fn is_download(self) -> bool {
        matches!(self, JobPhase::Downloading | JobPhase::DownloadingRecovery)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobProgress {
    pub phase: JobPhase,
    /// Download phases: bytes of this phase's file set, counted in the NZB's
    /// per-article sizes (the unit of [`NzbInfo::total_bytes`]), advanced as
    /// each article settles (written, or given up), so it always ends exactly at
    /// `bytes_total`. A resumed job's download phase starts at what earlier
    /// runs already wrote (its first `Progress`, sent with the `Phase` event,
    /// carries exactly that; speed and ETA only count new bytes). Verifying
    /// and Extracting carry their own byte counts (bytes hashed / unpacked);
    /// other phases report 0.
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Per-job wire speed, smoothed (exponential moving average, ~2 s), with
    /// bytes counted as they arrive. Zero outside the download phases, while
    /// paused, and after 3 s without a byte (offline, a stalled server).
    pub speed_bps: f64,
    /// Seconds left at `speed_bps`, once it can be trusted: `None` until
    /// bytes have flowed steadily for 3 s since the phase began, a resume or
    /// a stall, and when it would be over a day or the speed is under 1 KiB/s.
    pub eta_secs: Option<u64>,
    pub files_done: u32,
    pub files_total: u32,
    /// Articles given up so far in this job (missing, damaged, or unreachable).
    pub articles_failed: u64,
    /// 0...1 within the current phase.
    pub fraction: f64,
    /// e.g. "2 of 5" while extracting, "Loading recovery data" while verifying.
    pub detail: Option<String>,
    pub paused: bool,
    /// PAR2 blocks needing repair: found so far while Verifying, the total
    /// being rebuilt while Repairing ("Repairing 12 damaged blocks"); 0 in
    /// every other phase.
    pub damaged_blocks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    Completed,
    /// The payload is intact but something after it was not (an archive did not
    /// extract, PAR2 reported a problem on clean data).
    CompletedWithIssues,
    /// The payload is incomplete and could not be repaired, or the job hit an
    /// error (see [`JobSummary::error_kind`]).
    Failed,
    Stopped,
    /// The download is complete but an archive is encrypted and no password
    /// worked; [`Engine::reprocess`](super::Engine::reprocess) can finish it.
    /// The message is "This archive needs a password." when there was no
    /// password to try (or only the NZB's), "The password didn't work." when
    /// the request's passwords were all refused.
    NeedsPassword,
    /// The pre-flight scan found too much missing for PAR2 and the request
    /// said [`OnUnrepairable::Stop`]. Nothing was downloaded.
    Unrepairable,
}

/// A final, user-facing file in the job folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    /// Path relative to the job folder (extraction may create subfolders).
    pub name: String,
    pub bytes: u64,
}

/// What happened to one file listed in the NZB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReport {
    pub name: String,
    /// Where it was written (later renaming or extraction may have moved it).
    pub path: PathBuf,
    /// Decoded size on disk.
    pub bytes: u64,
    pub articles_total: u64,
    pub articles_failed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Par2Report {
    /// PAR2 verification actually ran.
    pub ran: bool,
    /// The payload is verified: PAR2 found it clean or repaired it, or every
    /// article's yEnc checksum matched on download (then `ran` is false).
    pub verified_ok: bool,
    /// Damaged or missing blocks PAR2 found (needed for repair).
    pub damaged_blocks: u64,
    pub repaired_blocks: u64,
    /// A repair ran and succeeded.
    pub repaired: bool,
    /// Why verification did not run, when it didn't.
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobSummary {
    pub outcome: Outcome,
    /// One sentence for the UI when not `Completed`.
    pub message: Option<String>,
    /// Set when the job ended because of an error (authentication, server
    /// unreachable, disk full, ...) rather than because of the payload. Lets the
    /// app pause its queue and offer Settings instead of failing every job.
    pub error_kind: Option<ErrorKind>,
    pub output_dir: PathBuf,
    /// Final user-facing files in `output_dir`, excluding recovery and release
    /// metadata files and archive volumes that were extracted.
    pub files: Vec<OutputFile>,
    /// Per-file download results, in NZB order.
    pub nzb_files: Vec<FileReport>,
    /// Decoded bytes written for non-PAR2 files.
    pub data_bytes: u64,
    /// Plaintext bytes read from the server while transferring articles.
    pub wire_bytes: u64,
    pub elapsed_secs: f64,
    /// Transfer time: first article to last, summed over download phases.
    /// `wire_bytes / download_secs` is the average speed.
    pub download_secs: f64,
    /// Time spent in the pre-flight scan (0 when skipped).
    pub check_secs: f64,
    /// Time spent verifying, repairing, extracting and renaming.
    pub post_secs: f64,
    pub articles_total: u64,
    pub articles_failed: u64,
    pub par2: Par2Report,
    pub archives_extracted: u32,
    pub archives_failed: u32,
    pub files_renamed: u32,
    pub availability: Option<AvailabilityInfo>,
    /// `start()` on the same folder would continue the job: it stopped or
    /// failed, its resume sidecar (`.dl-nzb-job.json`) is in the folder, and
    /// work is left (downloading, or post-processing that didn't finish).
    pub resumable: bool,
}

impl JobSummary {
    /// An empty summary for `output_dir`, to be filled in.
    pub(crate) fn new(outcome: Outcome, output_dir: PathBuf) -> Self {
        Self {
            outcome,
            message: None,
            error_kind: None,
            output_dir,
            files: Vec::new(),
            nzb_files: Vec::new(),
            data_bytes: 0,
            wire_bytes: 0,
            elapsed_secs: 0.0,
            download_secs: 0.0,
            check_secs: 0.0,
            post_secs: 0.0,
            articles_total: 0,
            articles_failed: 0,
            par2: Par2Report::default(),
            archives_extracted: 0,
            archives_failed: 0,
            files_renamed: 0,
            availability: None,
            resumable: false,
        }
    }
}

/// Result of a pre-flight scan. Byte counts use the NZB's article sizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailabilityInfo {
    pub articles_total: u64,
    pub articles_missing: u64,
    /// Data (non-PAR2) bytes missing, counting articles the scan could not
    /// check as missing (so repairability is never overstated).
    pub missing_bytes: u64,
    /// PAR2 bytes available on the server.
    pub recovery_bytes: u64,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Every essential article is present (only `.nfo`/`.sfv`/`.srr` may be missing).
    Complete,
    /// Data is missing but the available recovery data should cover it.
    Repairable,
    /// More is missing than the recovery data can repair.
    Unrepairable,
    /// The scan could not check every article, and the ones it couldn't
    /// check decide the verdict. The download goes ahead.
    Unknown,
}

/// What an NZB contains, from parsing alone (no network).
#[derive(Debug, Clone, PartialEq)]
pub struct NzbInfo {
    /// `<meta type="title">`, or the NZB file's name without extension; a
    /// `{{password}}` in either is removed (it is in `passwords`), so the
    /// title is safe to name a folder after.
    pub title: String,
    /// `<meta type="password">` values, then a `{{password}}` from the title
    /// or the file name (`Name{{password}}.nzb`).
    pub passwords: Vec<String>,
    pub category: Option<String>,
    /// Sum of all article sizes (the posted size).
    pub total_bytes: u64,
    /// `total_bytes` minus PAR2 files.
    pub data_bytes: u64,
    pub par2_bytes: u64,
    pub files: Vec<NzbFile>,
    /// The kind of content, by dominant bytes (see [`ContentKind`]).
    pub content_kind: ContentKind,
}

/// One file listed in an NZB (not to be confused with the parser's internal
/// `download::Nzb` file type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NzbFile {
    /// The name it will be written under (sanitized and de-duplicated).
    pub name: String,
    pub bytes: u64,
    pub segments: u32,
    pub kind: FileKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// Payload: anything that is not one of the other kinds.
    Data,
    Par2,
    /// RAR/7z/zip volumes and split files (`.r00`, `.001`).
    Archive,
    /// Release metadata (`.nfo`, `.sfv`, `.srr`, `.nzb`, checksums).
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentKind {
    Video,
    Audio,
    Archive,
    Image,
    Document,
    Software,
    Other,
}

/// A successful connection test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCheck {
    /// The server's greeting line.
    pub greeting: String,
    pub tls: bool,
    /// Round trip of one command after logging in.
    pub latency_ms: u32,
}
