//! Common regex patterns for file detection and parsing

use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

/// RAR archive patterns
pub mod rar {
    use super::*;

    /// Matches first part of multi-part RAR archives (.part01.rar, .part001.rar, .part0001.rar)
    static FIRST_PART_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)\.part0*1\.rar$").expect("valid regex"));

    /// Matches any part of multi-part RAR archives (.partNN.rar, .partNNN.rar)
    static MULTI_PART_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)\.part\d+\.rar$").expect("valid regex"));

    /// Matches old-style RAR split files (.r00, .r01, etc.)
    static OLD_STYLE_SPLIT_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)\.r\d{2}$").expect("valid regex"));

    /// Matches the base name of a RAR archive (before .partNN or .rNN)
    static BASE_NAME_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?i)(.*?)(?:\.part\d+|\.r\d{2})?\.rar$").expect("valid regex"));

    /// Check if path is a RAR archive that should be extracted
    /// Returns true for:
    /// - Single RAR files (archive.rar)
    /// - First part of multi-part archives (archive.part01.rar, archive.part001.rar)
    ///   Does NOT return true for:
    /// - Later parts (.part02.rar, .part003.rar, etc.)
    /// - Old-style splits (.r00, .r01, etc.) - these are handled via the main .rar
    pub fn is_extractable_archive(path: &Path) -> bool {
        let filename = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name,
            None => return false,
        };

        // Must have .rar extension
        if !filename.to_lowercase().ends_with(".rar") {
            return false;
        }

        // If it's a multi-part archive, must be the first part
        if MULTI_PART_REGEX.is_match(filename) {
            return FIRST_PART_REGEX.is_match(filename);
        }

        // Single .rar file without .partNN
        true
    }

    /// Check if a file is part of a RAR archive set (for deletion).
    fn is_rar_related(filename: &str) -> bool {
        let lower = filename.to_lowercase();
        lower.ends_with(".rar") || OLD_STYLE_SPLIT_REGEX.is_match(filename)
    }

    /// Extract base name from RAR archive for finding related parts
    pub fn extract_base_name(filename: &str) -> Option<&str> {
        BASE_NAME_REGEX
            .captures(filename)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str())
    }

    /// Check if two filenames belong to the same RAR archive set
    pub fn is_same_archive(base_name: &str, other_filename: &str) -> bool {
        let lower_base = base_name.to_lowercase();
        let lower_other = other_filename.to_lowercase();

        // Same base name and is a RAR-related file
        lower_other.starts_with(&lower_base) && is_rar_related(other_filename)
    }
}

/// PAR2 file patterns
pub mod par2 {
    use std::path::Path;

    /// Check if path is a PAR2 file
    pub fn is_par2_file(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("par2"))
            .unwrap_or(false)
    }

    /// Check if this is the main (index) PAR2 file (without .vol)
    pub fn is_main_par2(path: &Path) -> bool {
        is_par2_file(path)
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|name| !name.to_lowercase().contains(".vol"))
                .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_is_extractable_archive() {
        // Single archives should be extractable
        assert!(rar::is_extractable_archive(&PathBuf::from("archive.rar")));
        assert!(rar::is_extractable_archive(&PathBuf::from("Archive.RAR")));

        // First parts should be extractable
        assert!(rar::is_extractable_archive(&PathBuf::from(
            "archive.part01.rar"
        )));
        assert!(rar::is_extractable_archive(&PathBuf::from(
            "archive.part001.rar"
        )));
        assert!(rar::is_extractable_archive(&PathBuf::from(
            "archive.Part01.Rar"
        )));

        // Later parts should NOT be extractable
        assert!(!rar::is_extractable_archive(&PathBuf::from(
            "archive.part02.rar"
        )));
        assert!(!rar::is_extractable_archive(&PathBuf::from(
            "archive.part003.rar"
        )));
        assert!(!rar::is_extractable_archive(&PathBuf::from(
            "archive.part10.rar"
        )));

        // Non-RAR files should NOT be extractable
        assert!(!rar::is_extractable_archive(&PathBuf::from("archive.zip")));
        assert!(!rar::is_extractable_archive(&PathBuf::from("archive.r00")));
    }

    #[test]
    fn test_extract_base_name() {
        assert_eq!(rar::extract_base_name("archive.rar"), Some("archive"));
        assert_eq!(
            rar::extract_base_name("archive.part01.rar"),
            Some("archive")
        );
        assert_eq!(
            rar::extract_base_name("archive.part001.rar"),
            Some("archive")
        );
        assert_eq!(
            rar::extract_base_name("my.file.name.part05.rar"),
            Some("my.file.name")
        );
    }

    #[test]
    fn test_is_same_archive() {
        assert!(rar::is_same_archive("archive", "archive.part02.rar"));
        assert!(rar::is_same_archive("archive", "archive.r00"));
        assert!(rar::is_same_archive("archive", "archive.r15"));
        assert!(!rar::is_same_archive("archive", "other.rar"));
    }
}
