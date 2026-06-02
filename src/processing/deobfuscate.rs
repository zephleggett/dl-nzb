//! File name deobfuscation
//!
//! This module provides functionality to detect and rename obfuscated files
//! to more meaningful names based on the NZB name.

use super::file_extension;
use crate::error::{DlNzbError, PostProcessingError};
use crate::patterns::par2 as par2_patterns;
use par2_rs::Par2Info;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::{fs, io::Read};

type Result<T> = std::result::Result<T, DlNzbError>;

/// Outcome of the PAR2-driven filename recovery pass.
pub struct Par2NameRecovery {
    pub files_renamed: usize,
}

/// Rename obfuscated files to their real names using the authoritative file
/// table embedded in the PAR2 set.
///
/// Each protected file is identified by the MD5 of its first 16 KiB (the PAR2
/// `hash_16k`), so this works even when every filename on disk is scrambled —
/// the most reliable deobfuscation method (this is what SABnzbd does). It must
/// run BEFORE PAR2 repair so the repairer's name-match fast path sees real names
/// and before any `delete_par2_after_repair` purge removes the par2 files.
pub fn recover_par2_names(directory: &Path, par2_files: &[PathBuf]) -> Result<Par2NameRecovery> {
    // Prefer the index par2 (no `.vol`), else any par2 file; `Par2Info::load`
    // discovers sibling volumes by recovery-set id regardless.
    let index = par2_files
        .iter()
        .find(|p| par2_patterns::is_main_par2(p))
        .or_else(|| par2_files.first());
    let Some(index) = index else {
        return Ok(Par2NameRecovery { files_renamed: 0 });
    };

    let info = match Par2Info::load(index) {
        Ok(i) => i,
        Err(e) => {
            tracing::debug!(
                "PAR2 name recovery: could not parse {}: {}",
                index.display(),
                e
            );
            return Ok(Par2NameRecovery { files_renamed: 0 });
        }
    };

    // Map first-16k hash -> real name. Drop any hash shared by two distinct
    // names (ambiguous — can't safely pick), mirroring SABnzbd's duplicate guard.
    let mut by_hash: HashMap<[u8; 16], String> = HashMap::new();
    let mut ambiguous: HashSet<[u8; 16]> = HashSet::new();
    let mut known_names: HashSet<String> = HashSet::new();
    for f in &info.files {
        known_names.insert(f.name.clone());
        match by_hash.get(&f.hash_16k) {
            Some(existing) if existing != &f.name => {
                ambiguous.insert(f.hash_16k);
            }
            _ => {
                by_hash.insert(f.hash_16k, f.name.clone());
            }
        }
    }
    for h in &ambiguous {
        by_hash.remove(h);
    }
    if by_hash.is_empty() {
        return Ok(Par2NameRecovery { files_renamed: 0 });
    }

    let entries: Vec<PathBuf> = match fs::read_dir(directory) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect(),
        Err(_) => return Ok(Par2NameRecovery { files_renamed: 0 }),
    };

    let mut files_renamed = 0;
    for path in entries {
        if par2_patterns::is_par2_file(&path) {
            continue;
        }
        let cur_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if cur_name.is_empty() || known_names.contains(cur_name) {
            continue; // already correctly named
        }
        let hash = match md5_first_16k(&path) {
            Ok(h) => h,
            Err(_) => continue,
        };
        let Some(real_name) = by_hash.get(&hash) else {
            continue;
        };
        if real_name == cur_name {
            continue;
        }
        let target = path.with_file_name(real_name);
        // Authoritative rename: if the destination already exists, skip rather
        // than create a `name_1` variant that par2 verify can't match by name.
        if target.exists() {
            tracing::debug!(
                "PAR2 name recovery: target {} exists, skipping",
                target.display()
            );
            continue;
        }
        match fs::rename(&path, &target) {
            Ok(()) => {
                tracing::debug!("PAR2 name recovery: {} -> {}", path.display(), real_name);
                files_renamed += 1;
            }
            Err(e) => tracing::debug!("PAR2 name recovery rename failed: {}", e),
        }
    }

    Ok(Par2NameRecovery { files_renamed })
}

/// MD5 of the first 16 KiB of a file (or the whole file if smaller), matching
/// the PAR2 `hash_16k` definition.
fn md5_first_16k(path: &Path) -> std::io::Result<[u8; 16]> {
    let mut f = fs::File::open(path)?;
    let mut buf = vec![0u8; 16384];
    let mut filled = 0;
    while filled < buf.len() {
        let n = f.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(par2_rs::hash::compute_md5(&buf[..filled]))
}

/// Heuristic: looks like a hash/scrambled name rather than a human-meaningful one.
fn is_probably_obfuscated(filename: &str) -> bool {
    let name = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    let lower = name.to_lowercase();
    let len = name.len();
    if len < 5 {
        return true;
    }

    if lower.starts_with("f7f8f9") || lower.contains("yenc") {
        return true;
    }

    // Word-boundary count: any contiguous segment of alphanumerics. Real
    // release names typically have many short tokens separated by `.`, `_`,
    // `-`, or space. Hashes have one giant token.
    let mut word_count = 0usize;
    let mut in_word = false;
    let mut longest_digit_run = 0usize;
    let mut current_digit_run = 0usize;
    let mut longest_hex_run = 0usize;
    let mut current_hex_run = 0usize;
    for c in name.chars() {
        if c.is_alphanumeric() {
            if !in_word {
                in_word = true;
                word_count += 1;
            }
        } else {
            in_word = false;
        }
        if c.is_ascii_digit() {
            current_digit_run += 1;
            longest_digit_run = longest_digit_run.max(current_digit_run);
        } else {
            current_digit_run = 0;
        }
        if c.is_ascii_hexdigit() {
            current_hex_run += 1;
            longest_hex_run = longest_hex_run.max(current_hex_run);
        } else {
            current_hex_run = 0;
        }
    }

    // Single long alphanumeric blob that's mostly hex digits → likely a hash.
    // Hashes typically run together as one token with no separators.
    if word_count <= 1 && len >= 8 && longest_hex_run * 4 >= len * 3 {
        return true;
    }

    let digits = name.chars().filter(|c| c.is_ascii_digit()).count();
    let alpha = name.chars().filter(|c| c.is_alphabetic()).count();
    let specials = name
        .chars()
        .filter(|c| !c.is_alphanumeric() && *c != ' ' && *c != '-' && *c != '_' && *c != '.')
        .count();

    if specials > len / 2 {
        return true;
    }
    // Mostly digits with very little text.
    if digits > len / 2 && alpha < 3 {
        return true;
    }

    // Random consonant strings (very low vowel density in a single long word).
    if word_count <= 1 {
        let vowels = name
            .chars()
            .filter(|c| matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u'))
            .count();
        if alpha >= 8 && vowels < alpha / 4 {
            return true;
        }
    }

    false
}

/// Get the file extension including the dot
fn get_ext(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{}", s))
        .unwrap_or_default()
}

/// Get the base name without extension
fn get_basename(path: &Path) -> PathBuf {
    path.with_extension("")
}

/// Get file size in bytes
fn get_file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Find the biggest file in the list
fn get_biggest_file(files: &[PathBuf]) -> Option<(PathBuf, u64)> {
    files
        .iter()
        .map(|f| (f.clone(), get_file_size(f)))
        .max_by_key(|(_, size)| *size)
}

/// Generate a unique filename by appending numbers if needed
fn get_unique_filename(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");

    for i in 1..1000 {
        let new_name = if ext.is_empty() {
            format!("{}_{}", stem, i)
        } else {
            format!("{}_{}.{}", stem, i, ext)
        };
        let new_path = parent.join(new_name);
        if !new_path.exists() {
            return new_path;
        }
    }

    path.to_path_buf()
}

/// Rename a file, returning the new path
fn rename_file(old_path: &Path, new_path: &Path) -> Result<PathBuf> {
    fs::rename(old_path, new_path).map_err(|e| PostProcessingError::FileRenameError {
        from: old_path.to_path_buf(),
        to: new_path.to_path_buf(),
        source: e,
    })?;
    Ok(new_path.to_path_buf())
}

/// Sanitize a name to be filesystem-safe
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

pub struct DeobfuscateResult {
    pub files_renamed: usize,
    pub extensions_fixed: usize,
}

/// Deobfuscate files in a directory
///
/// This function:
/// 1. Adds missing extensions to files based on magic bytes
/// 2. Renames the largest obfuscated file to a meaningful name
/// 3. Renames related files (same basename) to match
pub fn deobfuscate_files(directory: &Path, useful_name: &str) -> Result<DeobfuscateResult> {
    let mut files_renamed = 0;
    let mut extensions_fixed = 0;

    // Get all files in directory (not recursively)
    let mut file_list: Vec<PathBuf> = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();

    if file_list.is_empty() {
        return Ok(DeobfuscateResult {
            files_renamed: 0,
            extensions_fixed: 0,
        });
    }

    // Check for DVD/Bluray directories - skip deobfuscation if found
    for file in &file_list {
        if let Some(parent) = file.parent() {
            let parent_str = parent.to_string_lossy();
            for ignored in file_extension::IGNORED_MOVIE_FOLDERS {
                if parent_str.contains(&format!("{}/", ignored))
                    || parent_str.contains(&format!("\\{}", ignored))
                {
                    tracing::debug!(
                        "Skipping deobfuscation due to DVD/Bluray directory: {}",
                        parent_str
                    );
                    return Ok(DeobfuscateResult {
                        files_renamed: 0,
                        extensions_fixed: 0,
                    });
                }
            }
        }
    }

    // Step 1: Fix missing extensions
    let mut new_file_list = Vec::new();
    for file in &file_list {
        if file_extension::has_popular_extension(file) {
            // Extension looks fine
            new_file_list.push(file.clone());
        } else if let Some(new_ext) = file_extension::what_is_most_likely_extension(file) {
            // Detected file type - add extension
            let new_path = file.with_extension(&new_ext[1..]); // Remove leading dot
            let new_path = get_unique_filename(&new_path);

            tracing::debug!(
                "Adding extension: {} -> {}",
                file.display(),
                new_path.display()
            );
            match rename_file(file, &new_path) {
                Ok(renamed) => {
                    new_file_list.push(renamed);
                    extensions_fixed += 1;
                }
                Err(e) => {
                    tracing::debug!("Failed to rename {}: {}", file.display(), e);
                    new_file_list.push(file.clone());
                }
            }
        } else {
            new_file_list.push(file.clone());
        }
    }
    file_list = new_file_list;

    // Step 2: Find biggest file and check if it needs deobfuscation
    let Some((biggest_file, biggest_size)) = get_biggest_file(&file_list) else {
        return Ok(DeobfuscateResult {
            files_renamed,
            extensions_fixed,
        });
    };

    // Check if biggest file should be excluded
    let ext = get_ext(&biggest_file);
    if file_extension::EXCLUDED_FILE_EXTS.contains(&ext.as_str()) {
        tracing::debug!(
            "Biggest file {} excluded due to extension",
            biggest_file.display()
        );
        return Ok(DeobfuscateResult {
            files_renamed,
            extensions_fixed,
        });
    }

    // Check if filename looks obfuscated
    let filename = biggest_file
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    if !is_probably_obfuscated(filename) {
        tracing::debug!(
            "Biggest file {} doesn't look obfuscated",
            biggest_file.display()
        );
        return Ok(DeobfuscateResult {
            files_renamed,
            extensions_fixed,
        });
    }

    // Check if it's significantly bigger than the second biggest file
    let second_biggest_size = file_list
        .iter()
        .filter(|f| *f != &biggest_file)
        .map(|f| get_file_size(f))
        .max()
        .unwrap_or(0);

    // Only rename if biggest is at least 1.5x bigger than second biggest
    if second_biggest_size > 0 && biggest_size < second_biggest_size * 3 / 2 {
        tracing::debug!(
            "Biggest file ({} bytes) not significantly larger than second biggest ({} bytes)",
            biggest_size,
            second_biggest_size
        );
        return Ok(DeobfuscateResult {
            files_renamed,
            extensions_fixed,
        });
    }

    // Step 3: Rename the biggest file
    let sanitized_name = sanitize_name(useful_name);
    let new_name = format!("{}{}", sanitized_name, ext);
    let new_path = biggest_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&new_name);

    // If the destination resolves to the same file we already have, there's
    // nothing to rename — and forcing _1 suffix would be worse than doing nothing.
    if new_path == biggest_file {
        return Ok(DeobfuscateResult {
            files_renamed,
            extensions_fixed,
        });
    }
    let new_path = get_unique_filename(&new_path);

    tracing::debug!(
        "Deobfuscating: {} -> {}",
        biggest_file.display(),
        new_path.display()
    );

    match rename_file(&biggest_file, &new_path) {
        Ok(_) => {
            files_renamed += 1;
        }
        Err(e) => {
            tracing::debug!("Failed to rename {}: {}", biggest_file.display(), e);
            return Ok(DeobfuscateResult {
                files_renamed,
                extensions_fixed,
            });
        }
    }

    // Step 4: Find and rename related files (same basename)
    let basename = get_basename(&biggest_file);
    let basename_str = basename.to_string_lossy();

    for file in &file_list {
        if *file == biggest_file {
            continue;
        }

        let file_basename = get_basename(file);
        let file_basename_str = file_basename.to_string_lossy();

        // Check if this file shares the same basename
        if file_basename_str == basename_str {
            let remaining = file
                .to_string_lossy()
                .replace(&basename_str.to_string(), "");

            let new_name = format!("{}{}", sanitized_name, remaining);
            let new_path = file
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(&new_name);
            let new_path = get_unique_filename(&new_path);

            tracing::debug!(
                "Deobfuscating related: {} -> {}",
                file.display(),
                new_path.display()
            );

            match rename_file(file, &new_path) {
                Ok(_) => files_renamed += 1,
                Err(e) => tracing::debug!("Failed to rename {}: {}", file.display(), e),
            }
        }
    }

    Ok(DeobfuscateResult {
        files_renamed,
        extensions_fixed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_obfuscated() {
        assert!(is_probably_obfuscated("f7f8f9abc123.mkv"));
        assert!(is_probably_obfuscated("a1b2c3d4e5f6.iso"));
        assert!(is_probably_obfuscated("xkcd.tmp"));
        assert!(!is_probably_obfuscated("Great_Movie_2023.mkv"));
        assert!(!is_probably_obfuscated("My.Document.pdf"));
    }

    #[test]
    fn test_sanitize_name() {
        assert_eq!(sanitize_name("File/Name:Test"), "File_Name_Test");
        assert_eq!(sanitize_name("Normal_File-123"), "Normal_File-123");
    }
}
