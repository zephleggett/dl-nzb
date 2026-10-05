//! RAR archive extraction, including encrypted archives.
//!
//! Every archive set in the job folder (named by its first volume) is
//! scanned, a password is found for it if it is encrypted, and it is unpacked
//! into a hidden staging folder inside the job folder. Only once every member
//! came out intact are the files moved to their final names (a rename on the
//! same file system), so a wrong password, a damaged archive or a stop never
//! leaves half-written files under real names.
//!
//! Passwords are tried in order: the user's (newest first), then the NZB's
//! (`<meta type="password">`, then `{{password}}` in the title or file name).
//! Each is checked cheaply before anything is unpacked:
//! - encrypted headers (`rar -hp`, names hidden): listing the archive with it;
//! - encrypted data: test-unpacking the smallest encrypted member when it is
//!   small, otherwise unpacking the first member is the test. RAR5 headers
//!   carry a password check value, so a wrong password fails at once there;
//!   RAR4 has none, and a CRC error on an encrypted member is how a wrong
//!   password shows (on RAR5 a CRC error means damage).
//!
//! An archive no password opens finishes the job as "needs password", with
//! the download left in place for `Engine::reprocess`. Passwords are never
//! logged, and are wiped from memory once dropped; one longer than unRAR
//! takes is not tried (it would be cut short).
//!
//! Links (symbolic, hard, junctions) are not unpacked: unRAR resolves a hard
//! link's target, and a file copy's source, against the process's working
//! folder rather than the staging folder, so an archive could reach any file
//! on the volume. Skipping them keeps every member a plain file or folder
//! under its flattened name. A file copy (RAR5 stores identical files once)
//! is made here instead, from the member it names inside the staging folder.
//!
//! With a job record, each extracted archive is recorded as soon as it is
//! done (before its volumes are deleted), so a job stopped between two
//! archives resumes with the next one.
//!
//! unRAR runs on a blocking thread through [`super::rar_ffi`], whose callback
//! reports unpacked bytes and stops unRAR as soon as the job is stopped; the
//! job waits for that thread before it reports `Stopped`. Only one archive is
//! open at a time in the whole process (unRAR's state is global): a job
//! waits for another job's extraction, unless it is stopped.

use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use unrar::error::Code;

use super::rar_ffi::{
    lock_unrar, Entry, Mode, Password, RarArchive, RarError, Redirect, UnrarLock,
    MAX_PASSWORD_CHARS,
};
use crate::config::PostProcessingConfig;
use crate::engine::context::JobCtx;
use crate::engine::sidecar::JobRecord;
use crate::engine::JobPhase;
use crate::error::DlNzbError;
use crate::patterns::rar as rar_patterns;

type Result<T> = std::result::Result<T, DlNzbError>;

/// Encrypted members up to this size are test-unpacked to check a password
/// before extraction; a larger smallest member is checked by extracting it.
const TEST_MEMBER_MAX: u64 = 16 * 1024 * 1024;

/// Hidden staging folders in the job folder: `.dl-nzb-unpack-<pid>-<n>`.
const STAGING_PREFIX: &str = ".dl-nzb-unpack-";

/// Names of the staging folders this process is using: never removed as
/// stale (another job may still be extracting in the same folder).
static LIVE_STAGING: Mutex<Vec<OsString>> = Mutex::new(Vec::new());

/// A password to try, and whether the user supplied it (the outcome's message
/// differs when the user's own passwords failed). Deliberately not `Debug`:
/// passwords are never logged.
#[derive(Clone)]
pub(crate) struct Candidate {
    pub(crate) password: Password,
    pub(crate) from_user: bool,
}

/// One archive set, scanned.
struct ArchivePlan {
    /// First volume.
    path: PathBuf,
    file_count: u64,
    total_bytes: u64,
    key: Key,
}

/// The password an archive set needs.
enum Key {
    /// Not encrypted.
    None,
    /// A password already shown to work (it listed the encrypted headers, or
    /// a member tested clean with it).
    Verified(Candidate),
    /// Encrypted data with no member small enough to test: these are tried in
    /// order, the first member's extraction checking each.
    Try(Vec<Candidate>),
}

/// What scanning an archive set found.
enum Scan {
    Plan(ArchivePlan),
    /// Encrypted, and no candidate opened it.
    NeedsPassword {
        user_tried: bool,
    },
    /// Damaged, incomplete or not a RAR archive (a reason in a few words).
    Unreadable(String),
    Stopped,
}

/// A member, from the listing.
struct Member {
    size: u64,
    encrypted: bool,
}

struct Listing {
    members: Vec<Member>,
    headers_encrypted: bool,
    solid: bool,
}

impl Listing {
    /// Extract the set at `path` (this listing) with `key`.
    fn plan(&self, path: &Path, key: Key) -> Scan {
        Scan::Plan(ArchivePlan {
            path: path.to_path_buf(),
            file_count: self.members.len() as u64,
            total_bytes: self.members.iter().map(|m| m.size).sum(),
            key,
        })
    }
}

/// Why unpacking (or checking a password on) a set did not succeed.
enum Fault {
    /// The password, or the lack of one, was refused.
    WrongPassword,
    Stopped,
    /// Damaged, incomplete or unwritable (a reason in a few words).
    Failed(String),
}

/// How one archive set ended.
enum SetResult {
    Done(Unpacked),
    NeedsPassword { user_tried: bool },
    Stopped,
    Failed(String),
}

/// What an archive set unpacked to, besides its files.
#[derive(Debug, Default, PartialEq)]
struct Unpacked {
    /// Links left out.
    links: usize,
}

pub struct RarExtractor {
    config: PostProcessingConfig,
    /// Candidate passwords, in the order they are tried.
    candidates: Vec<Candidate>,
    /// Passwords left out because unRAR can't take them whole.
    too_long: usize,
    /// One of those was the user's.
    user_password_too_long: bool,
    /// The job's resume record: extracted archives are recorded in it.
    record: Option<Arc<JobRecord>>,
}

#[derive(Debug, Clone, Default)]
pub struct RarExtractionReport {
    pub archives_extracted: usize,
    pub archives_failed: usize,
    /// Archives left unextracted because they are encrypted and no password
    /// worked.
    pub archives_encrypted: usize,
    /// Some archive still needs a password although the user supplied some:
    /// theirs were tried and refused.
    pub user_passwords_failed: bool,
    /// First volumes of the archives that extracted successfully.
    pub extracted: Vec<PathBuf>,
}

impl RarExtractor {
    pub fn new(config: PostProcessingConfig) -> Self {
        Self {
            config,
            candidates: Vec::new(),
            too_long: 0,
            user_password_too_long: false,
            record: None,
        }
    }

    /// Passwords to try: the user's first (in their order), then the NZB's.
    /// Duplicates are tried once; one longer than unRAR takes is left out
    /// (cut short, it could never be the right one).
    pub fn with_passwords(mut self, user: &[String], nzb: &[String]) -> Self {
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        let tagged = user
            .iter()
            .map(|p| (p, true))
            .chain(nzb.iter().map(|p| (p, false)));
        for (password, from_user) in tagged {
            if password.is_empty() || seen.contains(&password.as_str()) {
                continue;
            }
            seen.push(password);
            let password = Password::new(password);
            if password.wide_len() > MAX_PASSWORD_CHARS {
                self.too_long += 1;
                self.user_password_too_long |= from_user;
                continue;
            }
            candidates.push(Candidate {
                password,
                from_user,
            });
        }
        self.candidates = candidates;
        self
    }

    /// Record extracted archives in the job's resume record (and skip those
    /// it says were extracted already).
    pub(crate) fn with_record(mut self, record: Option<Arc<JobRecord>>) -> Self {
        self.record = record;
        self
    }

    /// Extract every archive set in `download_dir` into it, reporting the
    /// `Extracting` phase through `job` (entered only if there is something
    /// to extract).
    pub(crate) async fn extract_archives(
        &self,
        download_dir: &Path,
        job: &Arc<JobCtx>,
    ) -> Result<RarExtractionReport> {
        let mut rar_files: Vec<PathBuf> = std::fs::read_dir(download_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| rar_patterns::is_extractable_archive(p))
            .collect();
        rar_files.sort();

        let mut report = RarExtractionReport::default();
        // Archives an earlier run of this job extracted are done (their
        // volumes still count as extracted, so they aren't listed as output).
        if let Some(record) = &self.record {
            rar_files.retain(|path| {
                let done = record.archive_extracted(&display_name(path));
                if done {
                    report.extracted.push(path.clone());
                }
                !done
            });
        }
        if rar_files.is_empty() {
            return Ok(report);
        }
        remove_stale_staging(download_dir);

        // Scan every set first (headers, plus a password check where that is
        // cheap), so the phase's byte total is known up front.
        let candidates = self.candidates.clone();
        let cancel = job.cancel_flag();
        let scans = run_blocking(move || {
            let Some(lock) = lock_unrar(&cancel) else {
                return Vec::new();
            };
            let mut scans = Vec::with_capacity(rar_files.len());
            for path in rar_files {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let scan = scan_archive(&path, &candidates, &cancel, &lock);
                scans.push((path, scan));
            }
            scans
        })
        .await
        .unwrap_or_default();

        let needs_password = scans.iter().any(|(_, scan)| match scan {
            Scan::Plan(plan) => !matches!(plan.key, Key::None),
            Scan::NeedsPassword { .. } => true,
            _ => false,
        });
        if needs_password && self.too_long > 0 {
            job.warn(if self.too_long == 1 {
                format!("A password longer than {MAX_PASSWORD_CHARS} characters was not tried.")
            } else {
                format!(
                    "{} passwords longer than {MAX_PASSWORD_CHARS} characters were not tried.",
                    self.too_long
                )
            });
        }

        let mut plans = Vec::new();
        for (path, scan) in scans {
            match scan {
                Scan::Plan(plan) => plans.push(plan),
                Scan::NeedsPassword { user_tried } => {
                    tracing::debug!("{} needs a password", path.display());
                    report.archives_encrypted += 1;
                    report.user_passwords_failed |= user_tried || self.user_password_too_long;
                }
                Scan::Unreadable(reason) => {
                    tracing::warn!("Failed to scan RAR archive {}: {}", path.display(), reason);
                    report.archives_failed += 1;
                    job.warn(format!(
                        "Could not read the archive {}: {reason}.",
                        display_name(&path)
                    ));
                }
                Scan::Stopped => {}
            }
        }
        if plans.is_empty() || job.is_cancelled() {
            return Ok(report);
        }

        job.set_phase(JobPhase::Extracting);
        let progress = Progress {
            job: job.clone(),
            base: Arc::new(AtomicU64::new(0)),
            total: plans.iter().map(|plan| plan.total_bytes).sum(),
        };
        progress.at(0);
        let archive_count = plans.len();

        for (index, plan) in plans.into_iter().enumerate() {
            // Stop starting new archives once the job is stopped.
            if job.is_cancelled() {
                break;
            }
            let filename = display_name(&plan.path);
            if archive_count > 1 {
                job.set_detail(Some(format!("{} of {}", index + 1, archive_count)));
            }
            let plan = Arc::new(plan);

            match extract_set(&plan, download_dir, job, &progress, archive_count == 1).await {
                SetResult::Done(unpacked) => {
                    report.archives_extracted += 1;
                    report.extracted.push(plan.path.clone());
                    if unpacked.links > 0 {
                        let s = if unpacked.links == 1 { "" } else { "s" };
                        job.warn(format!("Skipped {} link{s} in {filename}.", unpacked.links));
                    }
                    self.finish_archive(&plan.path, download_dir).await;
                }
                SetResult::NeedsPassword { user_tried } => {
                    report.archives_encrypted += 1;
                    report.user_passwords_failed |= user_tried || self.user_password_too_long;
                }
                SetResult::Stopped => break,
                SetResult::Failed(reason) => {
                    tracing::warn!("RAR extraction error in {}: {}", filename, reason);
                    report.archives_failed += 1;
                    job.warn(format!("Could not extract {filename}: {reason}."));
                }
            }
            progress.advance(plan.total_bytes);
        }

        Ok(report)
    }

    /// An archive extracted: record it (with the volumes about to be deleted,
    /// so their absence isn't taken for damage), then delete its volumes when
    /// the configuration says so. Recorded first, so a crash in between
    /// leaves volumes behind rather than a record that misses them.
    async fn finish_archive(&self, first_volume: &Path, download_dir: &Path) {
        let volumes = if self.config.delete_rar_after_extract {
            archive_volumes(first_volume, download_dir)
        } else {
            Vec::new()
        };
        if let Some(record) = &self.record {
            let names: Vec<String> = volumes.iter().map(|p| display_name(p)).collect();
            record.set_archive_extracted(&display_name(first_volume), &names);
            if let Err(e) = record.save().await {
                tracing::debug!("could not save resume data: {e}");
            }
        }
        if volumes.is_empty() {
            return;
        }
        crate::util::blocking(move || {
            for volume in volumes {
                if let Err(e) = std::fs::remove_file(&volume) {
                    tracing::warn!("Failed to delete {}: {}", volume.display(), e);
                }
            }
        })
        .await;
    }
}

/// Extract one set, trying passwords as its plan says.
async fn extract_set(
    plan: &Arc<ArchivePlan>,
    output_dir: &Path,
    job: &Arc<JobCtx>,
    progress: &Progress,
    show_members: bool,
) -> SetResult {
    let attempts: Vec<(Option<&Candidate>, bool)> = match &plan.key {
        Key::None => vec![(None, true)],
        Key::Verified(candidate) => vec![(Some(candidate), true)],
        Key::Try(candidates) => candidates.iter().map(|c| (Some(c), false)).collect(),
    };
    let mut user_tried = false;
    for (candidate, verified) in attempts {
        if job.is_cancelled() {
            return SetResult::Stopped;
        }
        user_tried |= candidate.is_some_and(|c| c.from_user);
        progress.at(0);
        let password = candidate.map(|c| c.password.clone());
        let (plan, output_dir, progress) =
            (plan.clone(), output_dir.to_path_buf(), progress.clone());
        let outcome = run_blocking(move || {
            extract_blocking(
                &plan,
                &output_dir,
                password.as_ref().map(Password::as_str),
                verified,
                &progress,
                show_members,
            )
        })
        .await;
        match outcome {
            Some(Ok(unpacked)) => return SetResult::Done(unpacked),
            Some(Err(Fault::WrongPassword)) => continue,
            Some(Err(Fault::Stopped)) => return SetResult::Stopped,
            Some(Err(Fault::Failed(reason))) => return SetResult::Failed(reason),
            None if job.is_cancelled() => return SetResult::Stopped,
            None => return SetResult::Failed("extraction ended unexpectedly".into()),
        }
    }
    SetResult::NeedsPassword { user_tried }
}

/// Extracting-phase progress shared with the blocking thread: bytes are
/// counted from `base` (the sets already done) toward `total`.
#[derive(Clone)]
struct Progress {
    job: Arc<JobCtx>,
    base: Arc<AtomicU64>,
    total: u64,
}

impl Progress {
    /// `done` bytes into the current set (the phase's fraction follows).
    fn at(&self, done: u64) {
        let at = (self.base.load(Ordering::Relaxed) + done).min(self.total);
        self.job.set_bytes(at, self.total);
    }

    /// The current set is finished (or given up): move the base past it.
    fn advance(&self, set_bytes: u64) {
        self.base.fetch_add(set_bytes, Ordering::Relaxed);
        self.at(0);
    }
}

/// Run blocking unRAR work and return its result (`None` if it panicked).
/// A stopped job waits for it as well: the work sees the stop by itself
/// (unRAR's callback refuses the next chunk or volume, and waiting for the
/// unRAR lock gives up), and only once it has returned is its staging folder
/// gone and nothing left writing into the job folder.
async fn run_blocking<T, F>(work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(work).await.ok()
}

/// List `path` and work out the password it needs (blocking).
fn scan_archive(
    path: &Path,
    candidates: &[Candidate],
    cancel: &Arc<AtomicBool>,
    lock: &UnrarLock,
) -> Scan {
    let listing = match list_archive(path, None, cancel, lock) {
        Ok(listing) if !listing.headers_encrypted => listing,
        // Encrypted headers: a password is needed even to list it.
        Ok(_) | Err((RarError::Code(Code::MissingPassword | Code::BadPassword), _)) => {
            return scan_encrypted_headers(path, candidates, cancel, lock)
        }
        Err((RarError::Cancelled, _)) => return Scan::Stopped,
        Err((RarError::Code(code), _)) => return Scan::Unreadable(describe(code)),
    };
    if listing.members.is_empty() {
        return Scan::Unreadable("it has no files".into());
    }
    let plan = |key| listing.plan(path, key);
    if !listing.members.iter().any(|m| m.encrypted) {
        return plan(Key::None);
    }
    if candidates.is_empty() {
        return Scan::NeedsPassword { user_tried: false };
    }

    // Encrypted data. Test a small member with each candidate: the smallest
    // one, or in a solid archive (where a member can only be unpacked after
    // the ones before it) the first.
    let smallest = if listing.solid {
        listing.members.first().map(|m| (0, m))
    } else {
        listing
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.encrypted)
            .min_by_key(|(_, m)| m.size)
    };
    let Some((test_member, _)) = smallest.filter(|(_, m)| m.encrypted && m.size <= TEST_MEMBER_MAX)
    else {
        return plan(Key::Try(candidates.to_vec()));
    };

    let mut user_tried = false;
    for candidate in candidates {
        if cancel.load(Ordering::Relaxed) {
            return Scan::Stopped;
        }
        user_tried |= candidate.from_user;
        match check_member(path, test_member, candidate.password.as_str(), cancel, lock) {
            Ok(()) => return plan(Key::Verified(candidate.clone())),
            Err(Fault::WrongPassword) => continue,
            Err(Fault::Stopped) => return Scan::Stopped,
            Err(Fault::Failed(reason)) => return Scan::Unreadable(reason),
        }
    }
    Scan::NeedsPassword { user_tried }
}

/// Header-encrypted: the first candidate that lists the archive is the one.
fn scan_encrypted_headers(
    path: &Path,
    candidates: &[Candidate],
    cancel: &Arc<AtomicBool>,
    lock: &UnrarLock,
) -> Scan {
    let mut user_tried = false;
    for candidate in candidates {
        if cancel.load(Ordering::Relaxed) {
            return Scan::Stopped;
        }
        user_tried |= candidate.from_user;
        match list_archive(path, Some(candidate.password.as_str()), cancel, lock) {
            Ok(listing) if listing.members.is_empty() => {
                return Scan::Unreadable("it has no files".into())
            }
            Ok(listing) => return listing.plan(path, Key::Verified(candidate.clone())),
            Err((RarError::Cancelled, _)) => return Scan::Stopped,
            // Some headers decrypted, so the password is right but the set is
            // damaged or a volume is missing.
            Err((RarError::Code(code), read)) if read > 0 => {
                return Scan::Unreadable(describe(code))
            }
            // Wrong password: the headers don't decrypt (RAR5 says so, RAR4
            // reports them as damaged).
            Err(_) => continue,
        }
    }
    Scan::NeedsPassword { user_tried }
}

/// Walk an archive set's headers. On error, also says how many headers were
/// read before it.
fn list_archive(
    path: &Path,
    password: Option<&str>,
    cancel: &Arc<AtomicBool>,
    lock: &UnrarLock,
) -> std::result::Result<Listing, (RarError, usize)> {
    let mut archive =
        RarArchive::open(path, Mode::List, password, cancel.clone(), lock).map_err(|e| (e, 0))?;
    let mut listing = Listing {
        members: Vec::new(),
        headers_encrypted: archive.headers_encrypted(),
        solid: archive.is_solid(),
    };
    let mut read = 0usize;
    while let Some(entry) = archive.read_header().map_err(|e| (e, read))? {
        read += 1;
        if !entry.directory {
            listing.members.push(Member {
                size: entry.unpacked_size,
                encrypted: entry.encrypted,
            });
        }
        archive.skip().map_err(|e| (e, read))?;
    }
    Ok(listing)
}

/// Test-unpack file member `target` (counting files in archive order) with
/// `password`, writing nothing (blocking).
fn check_member(
    path: &Path,
    target: usize,
    password: &str,
    cancel: &Arc<AtomicBool>,
    lock: &UnrarLock,
) -> std::result::Result<(), Fault> {
    let mut archive = RarArchive::open(path, Mode::Extract, Some(password), cancel.clone(), lock)
        .map_err(|e| classify(e, None, false))?;
    let mut index = 0usize;
    loop {
        let entry = archive
            .read_header()
            .map_err(|e| classify(e, None, false))?
            .ok_or_else(|| Fault::Failed("a file listed in it is missing".into()))?;
        let file = !entry.directory && !entry.split_before;
        if file && index == target {
            return archive
                .test(None)
                .map_err(|e| classify(e, Some(&entry), false));
        }
        index += usize::from(file);
        archive
            .skip()
            .map_err(|e| classify(e, Some(&entry), false))?;
    }
}

/// What an unRAR error means for the password being tried. `verified`: the
/// password already proved itself on this set.
fn classify(error: RarError, entry: Option<&Entry>, verified: bool) -> Fault {
    match error {
        RarError::Cancelled => Fault::Stopped,
        RarError::Code(Code::MissingPassword | Code::BadPassword) => Fault::WrongPassword,
        // RAR4 encrypted members have no password check: a wrong password
        // decrypts to garbage that fails the CRC.
        RarError::Code(Code::BadData)
            if !verified && entry.is_some_and(|e| e.encrypted && !e.is_rar5()) =>
        {
            Fault::WrongPassword
        }
        RarError::Code(code) => Fault::Failed(describe(code)),
    }
}

/// Unpack a set into a fresh staging folder and, if every member came out
/// intact, move the files into `output_dir` (blocking). The staging folder is
/// removed whatever happens, before unRAR is free for another job.
fn extract_blocking(
    plan: &ArchivePlan,
    output_dir: &Path,
    password: Option<&str>,
    verified: bool,
    progress: &Progress,
    show_members: bool,
) -> std::result::Result<Unpacked, Fault> {
    // Declared first, so released last.
    let Some(lock) = lock_unrar(&progress.job.cancel_flag()) else {
        return Err(Fault::Stopped);
    };
    let staging =
        Staging::create(output_dir).map_err(|e| write_failed("create a working folder", &e))?;
    let unpacked = unpack(
        plan,
        staging.path(),
        password,
        verified,
        progress,
        show_members,
        &lock,
    )?;
    move_tree(staging.path(), output_dir)
        .map_err(|e| write_failed("move the files into place", &e))?;
    Ok(unpacked)
}

/// Only plain relative components: a member named `../x` or `/etc/x` can't
/// escape the folder it is unpacked into.
fn flatten(name: &Path) -> PathBuf {
    name.components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect()
}

fn unpack(
    plan: &ArchivePlan,
    staging: &Path,
    password: Option<&str>,
    mut verified: bool,
    progress: &Progress,
    show_members: bool,
    lock: &UnrarLock,
) -> std::result::Result<Unpacked, Fault> {
    let cancel = progress.job.cancel_flag();
    let mut archive = RarArchive::open(&plan.path, Mode::Extract, password, cancel.clone(), lock)
        .map_err(|e| classify(e, None, verified))?;
    let unpacked = Rc::new(Cell::new(0u64));
    let mut files = 0u64;
    let mut result = Unpacked::default();
    // File copies to make once every member is out: (copy, original).
    let mut copies: Vec<(PathBuf, PathBuf)> = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Fault::Stopped);
        }
        let Some(entry) = archive
            .read_header()
            .map_err(|e| classify(e, None, verified))?
        else {
            break;
        };
        let relative = flatten(&entry.name);
        // Links are left out, and copies made below, rather than letting
        // unRAR resolve their targets outside the staging folder.
        let redirected = match &entry.redirect {
            Redirect::None => false,
            Redirect::Link => {
                result.links += 1;
                true
            }
            Redirect::Copy(original) => {
                if !relative.as_os_str().is_empty() {
                    copies.push((relative.clone(), flatten(original)));
                }
                true
            }
        };
        if redirected || entry.directory || entry.split_before || relative.as_os_str().is_empty() {
            if !redirected && entry.directory && !relative.as_os_str().is_empty() {
                std::fs::create_dir_all(staging.join(&relative)).map_err(cannot_create)?;
            }
            archive
                .skip()
                .map_err(|e| classify(e, Some(&entry), verified))?;
            continue;
        }

        files += 1;
        if show_members {
            progress
                .job
                .set_detail(Some(format!("{files} of {}", plan.file_count)));
        }
        let dest = staging.join(&relative);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(cannot_create)?;
        }
        let sink = {
            let unpacked = unpacked.clone();
            let progress = progress.clone();
            Box::new(move |n: u64| {
                unpacked.set(unpacked.get() + n);
                progress.at(unpacked.get());
            })
        };
        archive
            .extract_to(&dest, Some(sink))
            .map_err(|e| classify(e, Some(&entry), verified))?;
        // A member that unpacked clean proves the password for the rest.
        verified |= entry.encrypted;
    }
    drop(archive);
    for (copy, original) in copies {
        if cancel.load(Ordering::Relaxed) {
            return Err(Fault::Stopped);
        }
        let from = staging.join(&original);
        let is_member = !original.as_os_str().is_empty()
            && std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_file());
        if !is_member {
            return Err(Fault::Failed("a file it copies is not in it".into()));
        }
        let to = staging.join(&copy);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(cannot_create)?;
        }
        std::fs::copy(&from, &to).map_err(|e| write_failed("copy a file", &e))?;
        files += 1;
    }
    if files == 0 {
        return Err(Fault::Failed("no files extracted".into()));
    }
    progress.at(plan.total_bytes);
    Ok(result)
}

fn cannot_create(e: std::io::Error) -> Fault {
    write_failed("create a folder", &e)
}

/// Writing into the job folder failed while trying to `doing`: a plain
/// reason for the warning ("the folder isn't writable"), the I/O error's
/// own wording in the log.
fn write_failed(doing: &str, e: &std::io::Error) -> Fault {
    tracing::debug!("unRAR extraction: could not {doing}: {e}");
    Fault::Failed(crate::error::write_problem(e).into())
}

/// A hidden folder in the job folder that members are unpacked into; removed
/// on drop (after a successful extraction it is already empty).
struct Staging(PathBuf);

impl Staging {
    fn create(output_dir: &Path) -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!("{STAGING_PREFIX}{}-{n}", std::process::id());
        let path = output_dir.join(&name);
        // Registered under the lock `remove_stale_staging` holds, so it never
        // sees the folder without its registration.
        let mut live = LIVE_STAGING.lock().unwrap_or_else(PoisonError::into_inner);
        std::fs::create_dir_all(&path)?;
        live.push(name.into());
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("Could not remove {}: {}", self.0.display(), e);
            }
        }
        if let Some(name) = self.0.file_name() {
            LIVE_STAGING
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|live| live != name);
        }
    }
}

/// Staging folders left by a run that crashed or was killed mid-extraction
/// (not those another job of this process is extracting into).
fn remove_stale_staging(dir: &Path) {
    let live = LIVE_STAGING.lock().unwrap_or_else(PoisonError::into_inner);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(STAGING_PREFIX)
            && !live.contains(&name)
            && entry.file_type().is_ok_and(|t| t.is_dir())
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Move everything in `from` into `to`, merging folders and replacing files
/// of the same name (as unRAR does when extracting in place). A link in `to`
/// is replaced, never followed.
fn move_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let target_is_dir = std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir());
        if entry.file_type()?.is_dir() && target_is_dir {
            move_tree(&source, &target)?;
        } else {
            std::fs::rename(&source, &target)?;
        }
    }
    Ok(())
}

/// A few words for the user about an unRAR error.
fn describe(code: Code) -> String {
    match code {
        Code::BadData => "the data is damaged".into(),
        Code::EOpen => "a volume is missing".into(),
        Code::ECreate | Code::EWrite => "files could not be written".into(),
        Code::ERead => "it could not be read".into(),
        Code::BadArchive => "it is not a RAR archive".into(),
        Code::UnknownFormat => "its format is not supported".into(),
        Code::NoMemory => "not enough memory".into(),
        Code::MissingPassword | Code::BadPassword => "the password was refused".into(),
        other => {
            tracing::debug!("unRAR error {}", other as i32);
            "unRAR reported an unexpected error".into()
        }
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_string())
}

/// Every volume of the archive set whose first volume is `rar_path`.
fn archive_volumes(rar_path: &Path, download_dir: &Path) -> Vec<PathBuf> {
    let Some(filename) = rar_path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let base_name = rar_patterns::extract_base_name(filename).unwrap_or(filename);
    let Ok(entries) = std::fs::read_dir(download_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| rar_patterns::is_same_archive(base_name, &e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // `version.rar` from the `unrar` crate's test data (MIT/Apache-2.0): one
    // plain member, VERSION.
    const PLAIN_RAR: &[u8] = &[
        0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x00, 0xcf, 0x90, 0x73, 0x00, 0x00, 0x0d, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x0c, 0x74, 0x20, 0x80, 0x27, 0x00, 0x15, 0x00, 0x00,
        0x00, 0x0b, 0x00, 0x00, 0x00, 0x03, 0x45, 0xf3, 0x7d, 0xc6, 0xa4, 0x8a, 0x07, 0x47, 0x1d,
        0x33, 0x07, 0x00, 0xa4, 0x81, 0x00, 0x00, 0x56, 0x45, 0x52, 0x53, 0x49, 0x4f, 0x4e, 0x0c,
        0x00, 0x8f, 0xec, 0x8a, 0x45, 0xcc, 0x23, 0xc8, 0x48, 0x08, 0x83, 0x62, 0xfe, 0x5f, 0xdd,
        0x5c, 0x53, 0x88, 0xf0, 0x72, 0xc4, 0x3d, 0x7b, 0x00, 0x40, 0x07, 0x00,
    ];

    fn plain_archive() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("version.rar");
        std::fs::write(&path, PLAIN_RAR).unwrap();
        (dir, path)
    }

    #[test]
    fn candidates_put_the_users_first_and_drop_duplicates() {
        let extractor = RarExtractor::new(PostProcessingConfig::default()).with_passwords(
            &["new".into(), "old".into()],
            &["nzb".into(), "old".into(), String::new()],
        );
        let tried: Vec<(&str, bool)> = extractor
            .candidates
            .iter()
            .map(|c| (c.password.as_str(), c.from_user))
            .collect();
        assert_eq!(tried, vec![("new", true), ("old", true), ("nzb", false)]);
    }

    /// unRAR for a test (whose own stop flag must not end the wait).
    fn unrar() -> UnrarLock {
        lock_unrar(&AtomicBool::new(false)).unwrap()
    }

    #[test]
    fn passwords_too_long_for_unrar_are_left_out() {
        let long = "x".repeat(MAX_PASSWORD_CHARS + 1);
        let extractor = RarExtractor::new(PostProcessingConfig::default())
            .with_passwords(&[long.clone(), "short".into()], &[long]);
        let tried: Vec<&str> = extractor
            .candidates
            .iter()
            .map(|c| c.password.as_str())
            .collect();
        assert_eq!(tried, vec!["short"]);
        assert_eq!(extractor.too_long, 1, "duplicates are counted once");
        assert!(extractor.user_password_too_long);
    }

    #[test]
    fn the_data_callback_counts_bytes_and_a_stop_aborts_the_member() {
        let (dir, path) = plain_archive();
        let cancel = Arc::new(AtomicBool::new(false));
        let lock = unrar();

        let mut archive =
            RarArchive::open(&path, Mode::Extract, None, cancel.clone(), &lock).unwrap();
        let entry = archive.read_header().unwrap().expect("one member");
        let seen = Rc::new(Cell::new(0u64));
        let counter = seen.clone();
        let sink = Box::new(move |n: u64| counter.set(counter.get() + n));
        assert_eq!(archive.test(Some(sink)), Ok(()));
        assert_eq!(seen.get(), entry.unpacked_size);
        drop(archive);

        // A stop lands while unRAR is writing: the next chunk it hands over is
        // refused and the member ends there.
        cancel.store(true, Ordering::Relaxed);
        let mut archive = RarArchive::open(&path, Mode::Extract, None, cancel, &lock).unwrap();
        archive.read_header().unwrap().expect("one member");
        let dest = dir.path().join("VERSION");
        assert_eq!(archive.extract_to(&dest, None), Err(RarError::Cancelled));
    }

    #[test]
    fn listing_reports_plain_members() {
        let (_dir, path) = plain_archive();
        let cancel = Arc::new(AtomicBool::new(false));
        let listing = list_archive(&path, None, &cancel, &unrar()).ok().unwrap();
        assert_eq!(listing.members.len(), 1);
        assert!(!listing.headers_encrypted);
        assert!(!listing.members[0].encrypted);
    }

    #[test]
    fn moving_a_tree_merges_folders_and_replaces_files() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        let to = dir.path().join("to");
        std::fs::create_dir_all(from.join("Sub")).unwrap();
        std::fs::create_dir_all(to.join("Sub")).unwrap();
        std::fs::write(from.join("a.mkv"), b"new").unwrap();
        std::fs::write(from.join("Sub").join("b.srt"), b"b").unwrap();
        std::fs::write(to.join("a.mkv"), b"old").unwrap();
        std::fs::write(to.join("Sub").join("c.srt"), b"c").unwrap();

        move_tree(&from, &to).unwrap();
        assert_eq!(std::fs::read(to.join("a.mkv")).unwrap(), b"new");
        assert!(to.join("Sub").join("b.srt").exists());
        assert!(to.join("Sub").join("c.srt").exists());
    }

    /// A link already in the job folder is replaced, never followed: files
    /// can't be moved through it to wherever it points.
    #[cfg(unix)]
    #[test]
    fn moving_a_tree_does_not_follow_links() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to, elsewhere) = (
            dir.path().join("from"),
            dir.path().join("to"),
            dir.path().join("elsewhere"),
        );
        for folder in [from.join("Sub"), to.clone(), elsewhere.clone()] {
            std::fs::create_dir_all(folder).unwrap();
        }
        std::fs::write(from.join("Sub").join("b.srt"), b"b").unwrap();
        std::os::unix::fs::symlink(&elsewhere, to.join("Sub")).unwrap();

        assert!(move_tree(&from, &to).is_err());
        assert!(!elsewhere.join("b.srt").exists());
    }

    /// A job's extraction that is stopped is waited for, however long unRAR
    /// takes to return: until then it may still be writing into the job
    /// folder. (The job used to give up after five seconds and report
    /// `Stopped` while unRAR carried on.)
    #[tokio::test]
    async fn a_stopped_job_waits_for_unrar_to_return() {
        let returned = Arc::new(AtomicBool::new(false));
        let done = returned.clone();
        let started = std::time::Instant::now();
        let result = run_blocking(move || {
            std::thread::sleep(std::time::Duration::from_millis(5_200));
            done.store(true, Ordering::Relaxed);
            7
        })
        .await;
        assert_eq!(result, Some(7));
        assert!(returned.load(Ordering::Relaxed));
        assert!(started.elapsed() >= std::time::Duration::from_millis(5_200));
    }

    fn extracting_config() -> PostProcessingConfig {
        PostProcessingConfig {
            auto_extract_rar: true,
            ..PostProcessingConfig::default()
        }
    }

    /// unRAR is one per process: a job waits while another job's archive is
    /// open, stops waiting as soon as it is stopped, and extracts once unRAR
    /// is free.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    // Holding unRAR across the awaits is the point: it stands for another job.
    #[allow(clippy::await_holding_lock)]
    async fn a_job_waits_for_another_jobs_unrar_unless_stopped() {
        let (dir, _) = plain_archive();
        let other_job = unrar();

        let job = JobCtx::detached();
        let extraction = {
            let (job, dir) = (job.clone(), dir.path().to_path_buf());
            tokio::spawn(async move {
                RarExtractor::new(extracting_config())
                    .extract_archives(&dir, &job)
                    .await
                    .unwrap()
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            !extraction.is_finished(),
            "used unRAR while another job had it"
        );
        let stopped_at = std::time::Instant::now();
        job.cancel();
        let report = extraction.await.unwrap();
        assert!(stopped_at.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(report.archives_extracted, 0);
        assert!(!dir.path().join("VERSION").exists());

        let job = JobCtx::detached();
        let extraction = {
            let (job, dir) = (job.clone(), dir.path().to_path_buf());
            tokio::spawn(async move {
                RarExtractor::new(extracting_config())
                    .extract_archives(&dir, &job)
                    .await
                    .unwrap()
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        drop(other_job);
        assert_eq!(extraction.await.unwrap().archives_extracted, 1);
        assert!(dir.path().join("VERSION").is_file());
    }

    /// A job starting in a folder clears staging folders a crashed run left,
    /// but not one another job of this process is extracting into.
    #[test]
    fn only_staging_nobody_uses_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let live = Staging::create(dir.path()).unwrap();
        std::fs::write(live.path().join("half.mkv"), b"x").unwrap();
        let crashed = dir.path().join(format!("{STAGING_PREFIX}1-1"));
        std::fs::create_dir_all(&crashed).unwrap();

        remove_stale_staging(dir.path());
        assert!(!crashed.exists());
        assert!(live.path().join("half.mkv").is_file());

        let path = live.path().to_path_buf();
        drop(live);
        assert!(!path.exists());
        std::fs::create_dir_all(&path).unwrap();
        remove_stale_staging(dir.path());
        assert!(!path.exists(), "no longer in use, so stale");
    }
}
