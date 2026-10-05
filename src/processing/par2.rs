//! PAR2 verification and repair.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::config::PostProcessingConfig;
use crate::engine::context::JobCtx;
use crate::engine::JobPhase;
use crate::error::{DlNzbError, PostProcessingError};
use crate::patterns::par2 as par2_patterns;
use par2_rs::{MessageCallback, MessageLevel, Par2Operation, Par2Repairer, ProgressCallback};

type Result<T> = std::result::Result<T, DlNzbError>;

/// Outcome of a PAR2 verify+repair attempt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Par2Status {
    /// No PAR2 files present.
    NoPar2Files,
    /// Files verified clean or repair completed successfully.
    Success,
    /// Repair attempted but failed (insufficient recovery, missing data, etc.).
    Failed,
}

/// Details of a PAR2 run.
#[derive(Debug, Clone, PartialEq)]
pub struct Par2Result {
    pub status: Par2Status,
    /// Blocks PAR2 found damaged or missing (what a repair must rebuild).
    pub damaged_blocks: u64,
    /// Blocks rebuilt by a successful repair.
    pub repaired_blocks: u64,
    /// Why it failed, in a few words, when it did.
    pub error: Option<String>,
}

impl Par2Result {
    fn status(status: Par2Status) -> Self {
        Self {
            status,
            damaged_blocks: 0,
            repaired_blocks: 0,
            error: None,
        }
    }
}

/// Run PAR2 verification (and repair if needed) on the downloaded payload,
/// reporting `Verifying`/`Repairing` progress through `job`. par2-rs works in
/// the PAR2 files' folder (the job folder).
///
/// Errors from par2-rs that don't already classify as "verification failed" are
/// surfaced to the caller. Verification failure itself returns `Par2Status::Failed`
/// (not an error), because the caller chooses what to do with that information.
pub async fn repair_with_par2(
    config: &PostProcessingConfig,
    downloaded_par2_files: &[PathBuf],
    job: &Arc<JobCtx>,
) -> Result<Par2Result> {
    if downloaded_par2_files.is_empty() {
        return Ok(Par2Result::status(Par2Status::NoPar2Files));
    }

    // Prefer the main index file (no .vol). Fall back to smallest .par2.
    let mut par2_candidates = downloaded_par2_files.to_vec();
    let main_par2: PathBuf = if let Some(main) = par2_candidates
        .iter()
        .find(|p| par2_patterns::is_main_par2(p))
    {
        main.clone()
    } else {
        par2_candidates.sort_by_key(|p| p.metadata().ok().map(|m| m.len()).unwrap_or(u64::MAX));
        par2_candidates
            .first()
            .cloned()
            .ok_or(PostProcessingError::Par2(par2_rs::Par2Error::NotFound))?
    };

    job.set_phase(JobPhase::Verifying);
    job.set_detail(Some("Loading recovery data".to_string()));

    // par2-rs renames misnamed files to the names in the PAR2 set, joined onto
    // the job folder. A name that climbs out of it (`../x`, an absolute path)
    // would escape the folder, so refuse such a set outright.
    if let Some(bad) = unsafe_par2_name(&main_par2) {
        job.warn(format!(
            "PAR2 was skipped because the recovery set names a file outside the download folder ({bad})."
        ));
        return Ok(Par2Result {
            error: Some("the recovery set names files outside the download folder".into()),
            ..Par2Result::status(Par2Status::Failed)
        });
    }

    let repairer = Par2Repairer::new(&main_par2).map_err(PostProcessingError::Par2)?;

    // Block counts come from par2-rs's structured messages: verification reports
    // "<file>: <n>/<total> blocks damaged" per damaged file and repair announces
    // "Repairing <n> damaged blocks" (the authoritative total, missing files
    // included) once it has checked there is enough recovery data.
    let damaged_seen = Arc::new(AtomicU64::new(0));
    let repairing_blocks = Arc::new(AtomicU64::new(0));
    let last_op: Arc<Mutex<Option<Par2Operation>>> = Arc::new(Mutex::new(None));

    let progress_job = job.clone();
    let progress_cb: ProgressCallback = Arc::new(move |operation, current, total| {
        let current = current.min(total);
        let entered = match last_op.lock() {
            Ok(mut last) if *last != Some(operation) => {
                *last = Some(operation);
                true
            }
            _ => false,
        };
        match operation {
            Par2Operation::Loading | Par2Operation::Scanning => {
                if entered {
                    progress_job.set_detail(Some(
                        if operation == Par2Operation::Loading {
                            "Loading recovery data"
                        } else {
                            "Scanning files"
                        }
                        .to_string(),
                    ));
                }
            }
            Par2Operation::Verifying => {
                if entered {
                    progress_job.set_detail(None);
                }
                // Verification reports bytes hashed (the fraction follows).
                progress_job.set_bytes(current, total);
            }
            Par2Operation::Repairing => {
                if entered {
                    progress_job.set_phase(JobPhase::Repairing);
                }
                progress_job.set_fraction(fraction(current, total));
            }
        }
    });

    let message_job = job.clone();
    let damaged_for_msg = damaged_seen.clone();
    let repairing_for_msg = repairing_blocks.clone();
    let message_cb: MessageCallback = Arc::new(move |level, message| {
        tracing::debug!("par2 {:?}: {}", level, message);
        if level != MessageLevel::Info {
            return;
        }
        if let Some(n) = parse_repairing_blocks(message) {
            repairing_for_msg.store(n, Ordering::Relaxed);
            message_job.set_damaged_blocks(n);
        } else if let Some(n) = parse_damaged_blocks(message) {
            let total = damaged_for_msg.fetch_add(n, Ordering::Relaxed) + n;
            message_job.set_damaged_blocks(total);
        }
    });

    // par2-rs CPU-intensive work goes on a blocking thread. The job's cancel
    // flag is handed in so a stop mid-repair aborts promptly; par2-rs
    // reconstructs into temp files and only commits on success, so an abort
    // never corrupts the originals.
    let purge_par2 = config.delete_par2_after_repair;
    let cancel = job.cancel_flag();
    let result = tokio::task::spawn_blocking(move || {
        repairer.repair_with_callbacks_cancellable(
            true,
            purge_par2,
            Some(progress_cb),
            Some(message_cb),
            Some(cancel),
        )
    })
    .await;

    let repairing = repairing_blocks.load(Ordering::Relaxed);
    let damaged = repairing.max(damaged_seen.load(Ordering::Relaxed));
    match result {
        Ok(Ok(())) => {
            job.set_fraction(1.0);
            Ok(Par2Result {
                status: Par2Status::Success,
                damaged_blocks: damaged,
                repaired_blocks: repairing,
                error: None,
            })
        }
        // A cancelled repair is not a real failure; the caller sees the job
        // was stopped and reports that instead.
        Ok(Err(par2_rs::Par2Error::Cancelled)) => Ok(Par2Result {
            error: Some("stopped".into()),
            damaged_blocks: damaged,
            ..Par2Result::status(Par2Status::Failed)
        }),
        Ok(Err(par2_rs::Par2Error::InsufficientRecovery { needed, available })) => {
            Ok(Par2Result {
                status: Par2Status::Failed,
                damaged_blocks: (needed as u64).max(damaged),
                repaired_blocks: 0,
                error: Some(format!(
                    "not enough recovery data to repair ({needed} blocks needed, {available} available)"
                )),
            })
        }
        Ok(Err(e)) => Ok(Par2Result {
            error: Some(e.to_string()),
            damaged_blocks: damaged,
            ..Par2Result::status(Par2Status::Failed)
        }),
        Err(join_err) => Ok(Par2Result {
            error: Some(format!("internal error: {join_err}")),
            damaged_blocks: damaged,
            ..Par2Result::status(Par2Status::Failed)
        }),
    }
}

fn fraction(current: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        current as f64 / total as f64
    }
}

/// `Repairing 12 damaged blocks` -> 12.
fn parse_repairing_blocks(message: &str) -> Option<u64> {
    message
        .strip_prefix("Repairing ")?
        .strip_suffix(" damaged blocks")?
        .trim()
        .parse()
        .ok()
}

/// `<file>: 3/120 blocks damaged` -> 3.
fn parse_damaged_blocks(message: &str) -> Option<u64> {
    let counts = message.strip_suffix(" blocks damaged")?;
    let (_, ratio) = counts.rsplit_once(": ")?;
    ratio.split_once('/')?.0.trim().parse().ok()
}

/// The first file name in the PAR2 set that isn't a plain relative path
/// (`..`, absolute, or a drive prefix), if any.
fn unsafe_par2_name(main_par2: &Path) -> Option<String> {
    let info = par2_rs::Par2Info::load(main_par2).ok()?;
    info.files
        .iter()
        .find(|f| {
            Path::new(&f.name)
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        })
        .map(|f| f.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_block_counts_from_par2_messages() {
        assert_eq!(
            parse_repairing_blocks("Repairing 12 damaged blocks"),
            Some(12)
        );
        assert_eq!(parse_repairing_blocks("Repairing files"), None);
        assert_eq!(
            parse_damaged_blocks("movie.part01.rar: 3/120 blocks damaged"),
            Some(3)
        );
        assert_eq!(parse_damaged_blocks("File damaged: x"), None);
    }
}
