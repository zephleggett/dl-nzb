//! Free-space check before a job downloads anything, so a full disk is
//! reported up front with the shortfall instead of surfacing as write errors
//! halfway through (preallocation is sparse, so it would not catch it).
//!
//! It only runs while something is left to download: once every download
//! phase finished, post-processing may have renamed or deleted the NZB's
//! files, which must not count as still to come.

use std::path::Path;

use super::sidecar::JobRecord;
use crate::config::Config;
use crate::download::Nzb;
use crate::error::{DownloadError, Result};
use crate::patterns::rar as rar_patterns;

/// Fail with [`DownloadError::DiskFull`] if `dir`'s volume can't hold what is
/// left of the job, as its resume `record` tells. `hint` is the caller's
/// figure for the free space (see `JobRequest::free_space_hint`); without one
/// the file system is asked. Silently passes when free space can't be
/// determined.
pub(crate) fn check_free_space(
    dir: &Path,
    nzb: &Nzb,
    config: &Config,
    hint: Option<u64>,
    record: &JobRecord,
) -> Result<()> {
    let Some(available) = hint.or_else(|| available_bytes(dir)) else {
        return Ok(());
    };
    let needed = estimate_needed(dir, nzb, config, record);
    if needed > available {
        return Err(DownloadError::DiskFull { needed, available }.into());
    }
    Ok(())
}

/// Peak space the rest of the job needs: the articles not yet written,
/// recovery volumes included (they may be fetched for repair), plus a second
/// copy of each RAR volume of an archive not yet extracted when extraction is
/// on (archives stay until their extraction succeeds, so the unpacked copy
/// coexists with them). The NZB's encoded sizes run a few percent above the
/// decoded data, a small built-in margin.
fn estimate_needed(dir: &Path, nzb: &Nzb, config: &Config, record: &JobRecord) -> u64 {
    let extract = config.post_processing.auto_extract_rar;
    nzb.files()
        .iter()
        .map(|file| {
            let unpacked = if extract && rar_patterns::is_rar_related(&file.filename) {
                file.bytes()
            } else {
                0
            };
            let segments: Vec<u64> = file.segments.iter().map(|s| s.bytes).collect();
            to_download(dir, &file.filename, &segments, record) + unpacked
        })
        .sum()
}

/// Bytes still to download of the file `name` (whose articles are `segments`
/// bytes each): the articles the record doesn't have written (in a file still
/// on disk), or else the whole file less what is already allocated to it.
fn to_download(dir: &Path, name: &str, segments: &[u64], record: &JobRecord) -> u64 {
    let final_path = dir.join(name);
    let partial_path = crate::patterns::partial_path(&final_path);
    let prior = record.slot(name).and_then(|slot| record.prior(slot));
    if let Some(prior) = prior.filter(|p| !p.done.is_empty()) {
        let kept = if prior.finalized {
            &final_path
        } else {
            &partial_path
        };
        if kept.is_file() {
            return segments
                .iter()
                .enumerate()
                .filter(|(i, _)| !prior.done.contains(*i as u32))
                .map(|(_, bytes)| bytes)
                .sum();
        }
    }
    let on_disk = allocated_bytes(&final_path).saturating_add(allocated_bytes(&partial_path));
    segments.iter().sum::<u64>().saturating_sub(on_disk)
}

/// Bytes actually allocated to a file (sparse `.partial` files take far less
/// than their length). 0 if it doesn't exist.
fn allocated_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks().saturating_mul(512).min(meta.len())
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// Free bytes available to this (unprivileged) process on `path`'s volume.
#[cfg(unix)]
fn available_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` is plain old data, so all-zero is a valid value.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated string and `stat` a valid,
    // writable `statvfs` for the duration of the call.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)] // field widths differ across platforms
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

#[cfg(not(unix))]
fn available_bytes(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_is_readable() {
        let dir = std::env::temp_dir();
        assert!(available_bytes(&dir).unwrap_or(1) > 0);
    }

    fn nzb(segments: &[u64]) -> Nzb {
        let segments: String = segments
            .iter()
            .enumerate()
            .map(|(i, bytes)| {
                format!(
                    r#"<segment bytes="{bytes}" number="{}">a{i}@b</segment>"#,
                    i + 1
                )
            })
            .collect();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
  <file poster="p" date="1" subject="&quot;a.bin&quot; yEnc (1/1)">
    <groups><group>alt.binaries.test</group></groups>
    <segments>{segments}</segments>
  </file>
</nzb>"#
        )
        .parse()
        .unwrap()
    }

    /// The caller's figure wins over the file system's, both ways.
    #[test]
    fn free_space_hint_replaces_statvfs() {
        let dir = tempfile::tempdir().unwrap();
        let nzb = nzb(&[1000]);
        let config = Config::default();
        let out = dir.path().join("job");
        std::fs::create_dir_all(&out).unwrap();
        let (record, _) = JobRecord::open(&out, &nzb, false);

        let err = check_free_space(&out, &nzb, &config, Some(10), &record).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::DiskFull);
        assert!(check_free_space(&out, &nzb, &config, Some(1_000_000), &record).is_ok());
    }

    /// A resumed download needs room for the articles it hasn't written, not
    /// for what its partly written file happens to have allocated.
    #[test]
    fn only_articles_not_yet_written_need_room() {
        let dir = tempfile::tempdir().unwrap();
        let nzb = nzb(&[1000, 1000, 1000, 1000]);
        let (record, _) = JobRecord::open(dir.path(), &nzb, false);
        let slot = record.slot("a.bin").unwrap();
        record.written(slot, 0, 1000);
        record.written(slot, 1, 2000);
        // A partial file allocated in full (not sparse), two articles in.
        std::fs::write(dir.path().join("a.bin.partial"), vec![1u8; 4000]).unwrap();

        let config = Config::default();
        assert_eq!(estimate_needed(dir.path(), &nzb, &config, &record), 2000);
        assert!(check_free_space(dir.path(), &nzb, &config, Some(1500), &record).is_err());
        assert!(check_free_space(dir.path(), &nzb, &config, Some(2500), &record).is_ok());

        // Its partial file gone, the whole file is downloaded again.
        std::fs::remove_file(dir.path().join("a.bin.partial")).unwrap();
        assert_eq!(estimate_needed(dir.path(), &nzb, &config, &record), 4000);
    }
}
