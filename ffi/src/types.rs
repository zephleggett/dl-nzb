//! The records and enums Swift sees (`apple/CONTRACT.md` §1). The engine's
//! own types cross as they are: each `#[uniffi::remote]` declaration below
//! restates a `dl_nzb::engine` type field for field (the generated converters
//! fail to compile if it drifts from the real one), so there is nothing to
//! convert. Only the job event (Swift-labelled cases) and the flat
//! configuration records the Swift side builds are this crate's own. Paths
//! cross as strings; sizes and counts keep the engine's widths (`u64` is
//! Swift's `UInt64`).

use std::path::PathBuf;

use dl_nzb::config::{Config, UsenetConfig};
use dl_nzb::engine as core;

pub use dl_nzb::engine::{
    AvailabilityInfo, ContentKind, ErrorKind, FileKind, FileReport, JobPhase, JobProgress,
    JobRequest, JobSummary, NzbFile, NzbInfo, OnUnrepairable, Outcome, OutputFile, Par2Report,
    Preflight, ServerCheck, Verdict,
};

// A path is a string in Swift (`typealias PathBuf = String`).
uniffi::custom_type!(PathBuf, String, {
    remote,
    lower: |path| path.to_string_lossy().into_owned(),
    try_lift: |path| Ok(PathBuf::from(path)),
});

// MARK: Configuration

/// The Usenet server and how to talk to it. Also what `test_connection` takes.
/// Its `Debug` hides the password.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub ssl: bool,
    pub verify_certificate: bool,
    pub username: String,
    pub password: String,
    /// 1 to 100.
    pub connections: u16,
    pub retry_attempts: u8,
}

/// Everything the engine is configured with, flat: the server, what happens
/// after a download, the advanced options and the speed limit.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct EngineConfig {
    pub server: ServerConfig,
    /// Verify and repair with PAR2.
    pub auto_par2_repair: bool,
    pub auto_extract_rar: bool,
    pub delete_rar_after_extract: bool,
    pub delete_par2_after_repair: bool,
    pub deobfuscate_file_names: bool,
    /// Fetch every recovery volume with the data instead of only on demand.
    pub download_all_par2: bool,
    /// `fsync` each finished file.
    pub fsync_on_finalize: bool,
    /// Engine-wide; `None` (or 0) is unlimited.
    pub speed_limit_bytes_per_second: Option<u64>,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("ssl", &self.ssl)
            .field("verify_certificate", &self.verify_certificate)
            .field("username", &self.username)
            .field("password", &"<REDACTED>")
            .field("connections", &self.connections)
            .field("retry_attempts", &self.retry_attempts)
            .finish()
    }
}

impl ServerConfig {
    pub(crate) fn to_core(&self) -> UsenetConfig {
        UsenetConfig {
            server: self.host.trim().to_string(),
            port: self.port,
            username: self.username.clone(),
            password: self.password.clone(),
            ssl: self.ssl,
            verify_ssl_certs: self.verify_certificate,
            connections: self.connections,
            retry_attempts: self.retry_attempts,
            ..UsenetConfig::default()
        }
    }

    fn from_core(usenet: &UsenetConfig) -> Self {
        Self {
            host: usenet.server.clone(),
            port: usenet.port,
            ssl: usenet.ssl,
            verify_certificate: usenet.verify_ssl_certs,
            username: usenet.username.clone(),
            password: usenet.password.clone(),
            connections: usenet.connections,
            retry_attempts: usenet.retry_attempts,
        }
    }
}

impl EngineConfig {
    /// The engine's `Config`, speed limit included (the engine applies it on
    /// `new` and `update_config`). Logging, notification and tuning settings
    /// the apps do not expose keep the CLI's defaults.
    pub(crate) fn to_core(&self) -> Config {
        let mut config = Config {
            usenet: self.server.to_core(),
            ..Config::default()
        };
        config.download.speed_limit = self.speed_limit_bytes_per_second.filter(|&n| n > 0);
        let pp = &mut config.post_processing;
        pp.auto_par2_repair = self.auto_par2_repair;
        pp.auto_extract_rar = self.auto_extract_rar;
        pp.delete_rar_after_extract = self.delete_rar_after_extract;
        pp.delete_par2_after_repair = self.delete_par2_after_repair;
        pp.deobfuscate_file_names = self.deobfuscate_file_names;
        pp.download_all_par2 = self.download_all_par2;
        config.tuning.fsync_on_finalize = self.fsync_on_finalize;
        config
    }

    /// The CLI's settings as the apps take them; its speed limit is not imported.
    pub(crate) fn from_core(config: &Config) -> Self {
        let pp = &config.post_processing;
        Self {
            server: ServerConfig::from_core(&config.usenet),
            auto_par2_repair: pp.auto_par2_repair,
            auto_extract_rar: pp.auto_extract_rar,
            delete_rar_after_extract: pp.delete_rar_after_extract,
            delete_par2_after_repair: pp.delete_par2_after_repair,
            deobfuscate_file_names: pp.deobfuscate_file_names,
            download_all_par2: pp.download_all_par2,
            fsync_on_finalize: config.tuning.fsync_on_finalize,
            speed_limit_bytes_per_second: None,
        }
    }
}

/// The dl-nzb CLI's settings, read from its `config.toml`, password included.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ImportedConfig {
    pub config: EngineConfig,
    /// The CLI's `download.dir` when it is absolute (a leading `~` is
    /// expanded with the process's home directory, which in a sandbox is the
    /// container). Only a suggestion: the app may not write there.
    pub download_dir: Option<String>,
    /// The file it came from.
    pub source: String,
}

// MARK: Requests

#[uniffi::remote(Enum)]
pub enum Preflight {
    /// Scan only when the NZB has no PAR2 files.
    Auto,
    Always,
    Never,
}

#[uniffi::remote(Enum)]
pub enum OnUnrepairable {
    /// Finish with `Outcome::Unrepairable` before downloading anything.
    Stop,
    /// Download anyway.
    Continue,
}

/// One job: an NZB downloaded into a folder.
#[uniffi::remote(Record)]
pub struct JobRequest {
    pub nzb_path: PathBuf,
    /// The exact, absolute job folder; the engine creates it.
    pub output_dir: PathBuf,
    /// Tried in order for encrypted RARs, after the NZB's own passwords.
    pub passwords: Vec<String>,
    pub preflight: Preflight,
    pub on_unrepairable: OnUnrepairable,
    /// Free bytes on the output folder's volume as the app sees them (iOS:
    /// `volumeAvailableCapacityForImportantUsage`). `None` asks the file system.
    pub free_space_hint: Option<u64>,
    /// The job's name (the release title the app shows): renaming calls an
    /// obfuscated main file after it, never after the de-duplicated folder.
    /// `None` uses the NZB's title.
    pub title: Option<String>,
}

// MARK: Events

/// `dl_nzb::engine::JobEvent` with labelled cases (`.phase(phase:)`).
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum JobEvent {
    /// The job entered a phase. Always followed by one `Progress`.
    Phase { phase: JobPhase },
    /// At most 4 Hz, plus one on every phase change and one at the end of each
    /// download phase.
    Progress { progress: JobProgress },
    /// After a pre-flight scan.
    Availability { info: AvailabilityInfo },
    /// One plain-English sentence.
    Warning { message: String },
    /// Exactly once, always last.
    Finished { summary: JobSummary },
}

impl From<core::JobEvent> for JobEvent {
    fn from(e: core::JobEvent) -> Self {
        match e {
            core::JobEvent::Phase(phase) => Self::Phase { phase },
            core::JobEvent::Progress(progress) => Self::Progress { progress },
            core::JobEvent::Availability(info) => Self::Availability { info },
            core::JobEvent::Warning(message) => Self::Warning { message },
            core::JobEvent::Finished(summary) => Self::Finished { summary },
        }
    }
}

#[uniffi::remote(Enum)]
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

#[uniffi::remote(Record)]
pub struct JobProgress {
    pub phase: JobPhase,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub speed_bps: f64,
    pub eta_secs: Option<u64>,
    pub files_done: u32,
    pub files_total: u32,
    pub articles_failed: u64,
    /// 0...1 within the current phase.
    pub fraction: f64,
    pub detail: Option<String>,
    pub paused: bool,
    /// Verifying: damaged blocks found so far; Repairing: blocks being rebuilt.
    pub damaged_blocks: u64,
}

#[uniffi::remote(Enum)]
pub enum Outcome {
    Completed,
    CompletedWithIssues,
    Failed,
    Stopped,
    NeedsPassword,
    Unrepairable,
}

#[uniffi::remote(Enum)]
pub enum ErrorKind {
    Config,
    Auth,
    Dns,
    Connect,
    Tls,
    Timeout,
    Protocol,
    Nzb,
    Io,
    DiskFull,
}

#[uniffi::remote(Record)]
pub struct OutputFile {
    /// Relative to the job folder.
    pub name: String,
    pub bytes: u64,
}

#[uniffi::remote(Record)]
pub struct FileReport {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub articles_total: u64,
    pub articles_failed: u64,
}

#[uniffi::remote(Record)]
pub struct Par2Report {
    pub ran: bool,
    pub verified_ok: bool,
    pub damaged_blocks: u64,
    pub repaired_blocks: u64,
    pub repaired: bool,
    pub skipped_reason: Option<String>,
}

#[uniffi::remote(Record)]
pub struct JobSummary {
    pub outcome: Outcome,
    pub message: Option<String>,
    pub error_kind: Option<ErrorKind>,
    pub output_dir: PathBuf,
    pub files: Vec<OutputFile>,
    pub nzb_files: Vec<FileReport>,
    pub data_bytes: u64,
    pub wire_bytes: u64,
    pub elapsed_secs: f64,
    pub download_secs: f64,
    pub check_secs: f64,
    pub post_secs: f64,
    pub articles_total: u64,
    pub articles_failed: u64,
    pub par2: Par2Report,
    pub archives_extracted: u32,
    pub archives_failed: u32,
    pub files_renamed: u32,
    pub availability: Option<AvailabilityInfo>,
    pub resumable: bool,
}

#[uniffi::remote(Record)]
pub struct AvailabilityInfo {
    pub articles_total: u64,
    pub articles_missing: u64,
    pub missing_bytes: u64,
    pub recovery_bytes: u64,
    pub verdict: Verdict,
}

#[uniffi::remote(Enum)]
pub enum Verdict {
    Complete,
    Repairable,
    Unrepairable,
    Unknown,
}

// MARK: Inspection and connection tests

#[uniffi::remote(Record)]
pub struct NzbInfo {
    pub title: String,
    pub passwords: Vec<String>,
    pub category: Option<String>,
    pub total_bytes: u64,
    pub data_bytes: u64,
    pub par2_bytes: u64,
    pub files: Vec<NzbFile>,
    pub content_kind: ContentKind,
}

#[uniffi::remote(Record)]
pub struct NzbFile {
    pub name: String,
    pub bytes: u64,
    pub segments: u32,
    pub kind: FileKind,
}

#[uniffi::remote(Enum)]
pub enum FileKind {
    Data,
    Par2,
    Archive,
    Other,
}

#[uniffi::remote(Enum)]
pub enum ContentKind {
    Video,
    Audio,
    Archive,
    Image,
    Document,
    Software,
    Other,
}

#[uniffi::remote(Record)]
pub struct ServerCheck {
    pub greeting: String,
    pub tls: bool,
    pub latency_ms: u32,
}
