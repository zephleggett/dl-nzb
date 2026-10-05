//! File name deobfuscation
//!
//! This module provides functionality to detect and rename obfuscated files
//! to more meaningful names based on the NZB name.

use super::file_extension;
use crate::error::DlNzbError;
use crate::patterns::par2 as par2_patterns;
use par2_rs::Par2Info;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::{fs, io::Read};

type Result<T> = std::result::Result<T, DlNzbError>;

/// Rename obfuscated files to their real names using the authoritative file
/// table embedded in the PAR2 set, returning how many were renamed.
///
/// Each protected file is identified by the MD5 of its first 16 KiB (the PAR2
/// `hash_16k`), so this works even when every filename on disk is scrambled —
/// the most reliable deobfuscation method (this is what SABnzbd does). It must
/// run BEFORE PAR2 repair so the repairer's name-match fast path sees real names
/// and before any `delete_par2_after_repair` purge removes the par2 files.
///
/// `on_rename(from, to)` is called after each file renamed.
pub fn recover_par2_names(
    directory: &Path,
    par2_files: &[PathBuf],
    on_rename: &mut dyn FnMut(&Path, &Path),
) -> usize {
    // Prefer the index par2 (no `.vol`), else any par2 file; `Par2Info::load`
    // discovers sibling volumes by recovery-set id regardless.
    let index = par2_files
        .iter()
        .find(|p| par2_patterns::is_main_par2(p))
        .or_else(|| par2_files.first());
    let Some(index) = index else {
        return 0;
    };

    let info = match Par2Info::load(index) {
        Ok(i) => i,
        Err(e) => {
            tracing::debug!(
                "PAR2 name recovery: could not parse {}: {}",
                index.display(),
                e
            );
            return 0;
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
        return 0;
    }

    let entries: Vec<PathBuf> = match fs::read_dir(directory) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect(),
        Err(_) => return 0,
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
        // The name comes from the PAR2 set: only accept a plain file name, so
        // a crafted `../x` or absolute path can't move a file out of the job
        // folder (sub-folder names are skipped too; their parent may not exist).
        let mut components = Path::new(real_name).components();
        if !matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        ) {
            tracing::debug!("PAR2 name recovery: unsafe name {:?} skipped", real_name);
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
                on_rename(&path, &target);
            }
            Err(e) => tracing::debug!("PAR2 name recovery rename failed: {}", e),
        }
    }

    files_renamed
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

/// A job's title made safe as a file name stem: no path separators or
/// characters file systems refuse, no control or invisible characters, no
/// leading or trailing spaces or dots, at most 200 bytes. `None` when nothing
/// is left.
pub(crate) fn name_from_title(title: &str) -> Option<String> {
    let edge = |c: char| c.is_whitespace() || c == '.';
    let cleaned = sanitize_name(title);
    let trimmed = cleaned.trim_matches(edge);
    let mut end = trimmed.len().min(200);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let name = trimmed[..end].trim_end_matches(edge);
    (!name.is_empty()).then(|| name.to_string())
}

/// Sanitize a name to be filesystem-safe: path separators and characters
/// file systems refuse become `_`, control characters and line separators
/// too, and characters that show as nothing ([`is_invisible`]) are dropped:
/// they can disguise how a name reads ("evil\u{202E}vkm.exe" shows as
/// "evilexe.mkv"), or make a name of nothing at all ("\u{200B}.mkv").
fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            '\u{2028}' | '\u{2029}' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

/// Whether `c` shows as nothing: a format character (Unicode category Cf:
/// the soft hyphen, zero-width space and joiners, direction marks, the
/// bidirectional embeddings, overrides and isolates, the word joiner and
/// invisible operators, the byte order mark, tags...), or another default-
/// ignorable one (the combining grapheme joiner, Hangul fillers, Khmer
/// inherent vowels, Mongolian and other variation selectors).
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        // Category Cf.
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
            // Other default-ignorable characters, the fillers among them.
            | '\u{034F}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FFA0}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// `stem` without a trailing `ext` (".mkv", any case), so a title that
/// already ends in the file's extension doesn't get it twice. Unchanged
/// when nothing would be left.
fn without_extension<'a>(stem: &'a str, ext: &str) -> &'a str {
    if ext.is_empty() || stem.len() <= ext.len() {
        return stem;
    }
    let cut = stem.len() - ext.len();
    if !stem.is_char_boundary(cut) || !stem[cut..].eq_ignore_ascii_case(ext) {
        return stem;
    }
    let rest = stem[..cut].trim_end_matches(|c: char| c.is_whitespace() || c == '.');
    if rest.is_empty() {
        stem
    } else {
        rest
    }
}

/// Deobfuscate files in a directory, returning how many were renamed (adding
/// an extension doesn't count).
///
/// This function:
/// 1. Adds missing extensions to files based on magic bytes
/// 2. Renames the largest obfuscated file to a meaningful name
/// 3. Renames related files (same basename) to match
///
/// `on_rename(from, to)` is called after each file renamed.
pub fn deobfuscate_files(
    directory: &Path,
    useful_name: &str,
    on_rename: &mut dyn FnMut(&Path, &Path),
) -> Result<usize> {
    let mut files_renamed = 0;

    // Get all files in directory (not recursively)
    let mut file_list: Vec<PathBuf> = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();

    if file_list.is_empty() {
        return Ok(0);
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
                    return Ok(0);
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
            let new_path = get_unique_filename(&file.with_extension(new_ext));

            tracing::debug!(
                "Adding extension: {} -> {}",
                file.display(),
                new_path.display()
            );
            match fs::rename(file, &new_path) {
                Ok(()) => {
                    on_rename(file, &new_path);
                    new_file_list.push(new_path);
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
        return Ok(files_renamed);
    };

    // Check if biggest file should be excluded
    let ext = get_ext(&biggest_file);
    if file_extension::EXCLUDED_FILE_EXTS.contains(&ext.as_str()) {
        tracing::debug!(
            "Biggest file {} excluded due to extension",
            biggest_file.display()
        );
        return Ok(files_renamed);
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
        return Ok(files_renamed);
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
        return Ok(files_renamed);
    }

    // Step 3: Rename the biggest file
    let sanitized = sanitize_name(useful_name);
    let sanitized_name = without_extension(&sanitized, &ext);
    let new_name = format!("{}{}", sanitized_name, ext);
    let new_path = biggest_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&new_name);

    // If the destination resolves to the same file we already have, there's
    // nothing to rename — and forcing _1 suffix would be worse than doing nothing.
    if new_path == biggest_file {
        return Ok(files_renamed);
    }
    let new_path = get_unique_filename(&new_path);

    tracing::debug!(
        "Deobfuscating: {} -> {}",
        biggest_file.display(),
        new_path.display()
    );

    match fs::rename(&biggest_file, &new_path) {
        Ok(()) => {
            files_renamed += 1;
            on_rename(&biggest_file, &new_path);
        }
        Err(e) => {
            tracing::debug!("Failed to rename {}: {}", biggest_file.display(), e);
            return Ok(files_renamed);
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

            match fs::rename(file, &new_path) {
                Ok(()) => {
                    files_renamed += 1;
                    on_rename(file, &new_path);
                }
                Err(e) => tracing::debug!("Failed to rename {}: {}", file.display(), e),
            }
        }
    }

    Ok(files_renamed)
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
        assert_eq!(
            sanitize_name("evil\u{202E}vkm.exe\u{2066}\u{200E}\u{061C}"),
            "evilvkm.exe"
        );
        assert_eq!(name_from_title("\u{202E}.\u{200F}"), None);
    }

    /// Characters that show as nothing are dropped, so a title of only
    /// them makes no name, and one hiding the extension keeps one.
    #[test]
    fn invisible_characters_are_dropped() {
        for title in [
            "\u{200B}",
            "\u{FEFF}",
            "\u{2060}",
            "\u{200D}",
            "\u{00AD}",
            "\u{3164}",
            "\u{FFA0}",
            "\u{115F}\u{1160}",
            "\u{FE0F}",
            "\u{E0041}",
            "\u{180E}",
            " \u{200C} . ",
        ] {
            assert_eq!(name_from_title(title), None, "{title:?}");
        }
        assert_eq!(
            sanitize_name("Na\u{200B}me\u{FEFF}.mkv\u{2060}"),
            "Name.mkv"
        );
        assert_eq!(
            without_extension(&sanitize_name("Name.mkv\u{200B}"), ".mkv"),
            "Name"
        );
        assert_eq!(sanitize_name("a\u{2028}b"), "a_b");
        // Visible text in any script stays.
        assert_eq!(sanitize_name("日本語 Ünïcødé ㅎ"), "日本語 Ünïcødé ㅎ");
    }

    #[test]
    fn a_title_keeps_one_extension() {
        assert_eq!(without_extension("Name.mkv", ".mkv"), "Name");
        assert_eq!(without_extension("Big Movie.MKV", ".mkv"), "Big Movie");
        assert_eq!(without_extension("Name .mkv", ".mkv"), "Name");
        assert_eq!(without_extension(".mkv", ".mkv"), ".mkv");
        assert_eq!(without_extension("Name.mkv", ""), "Name.mkv");
        assert_eq!(without_extension("Name.avi", ".mkv"), "Name.avi");
        assert_eq!(without_extension("日本.mkv", ".mkv"), "日本");
    }

    #[test]
    fn titles_become_safe_file_names() {
        assert_eq!(
            name_from_title(" Big Buck Bunny (2008) ").as_deref(),
            Some("Big Buck Bunny (2008)")
        );
        assert_eq!(name_from_title("../a/b").as_deref(), Some("_a_b"));
        assert_eq!(name_from_title("..").as_deref(), None);
        assert_eq!(name_from_title("  \n").as_deref(), Some("_"));
        assert_eq!(name_from_title("   ").as_deref(), None);
        let long = "é".repeat(150);
        let name = name_from_title(&long).unwrap();
        assert!(name.len() <= 200 && long.starts_with(&name));
    }
}
