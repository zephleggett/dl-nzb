//! File extension detection and validation
//!
//! This module provides utilities to detect file types by inspecting file contents
//! and determine whether files have meaningful extensions.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Common/popular file extensions that are considered meaningful
const POPULAR_EXTENSIONS: &[&str] = &[
    // Archives
    "zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "dmg", // Video
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "m2ts", "ts",
    // Audio
    "mp3", "flac", "wav", "aac", "ogg", "wma", "m4a", "opus", // Images
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "svg", "tiff", "ico", // Documents
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "odt", "ods", "odp",
    // Ebooks
    "epub", "mobi", "azw", "azw3", "fb2", "cbr", "cbz", // Subtitles
    "srt", "sub", "idx", "ass", "ssa", "vtt", // Executables
    "exe", "dll", "dmg", "app", "apk", "deb", "rpm", // Data
    "nfo", "sfv", "nzb", "torrent",
];

/// DVD/Bluray directories that should prevent deobfuscation
pub const IGNORED_MOVIE_FOLDERS: &[&str] = &["VIDEO_TS", "AUDIO_TS", "BDMV", "CERTIFICATE"];

/// File extensions to exclude from deobfuscation
pub const EXCLUDED_FILE_EXTS: &[&str] = &[".par2", ".sfv", ".nfo", ".txt", ".srr"];

/// Magic bytes for common file types, and the extension (without its dot)
/// they mean. The first entry that matches wins.
struct MagicBytes {
    bytes: &'static [u8],
    extension: &'static str,
    offset: usize,
}

const MAGIC_BYTES: &[MagicBytes] = &[
    // Images
    MagicBytes {
        bytes: b"\xFF\xD8\xFF",
        extension: "jpg",
        offset: 0,
    },
    MagicBytes {
        bytes: b"\x89PNG\r\n\x1a\n",
        extension: "png",
        offset: 0,
    },
    MagicBytes {
        bytes: b"GIF87a",
        extension: "gif",
        offset: 0,
    },
    MagicBytes {
        bytes: b"GIF89a",
        extension: "gif",
        offset: 0,
    },
    MagicBytes {
        bytes: b"BM",
        extension: "bmp",
        offset: 0,
    },
    MagicBytes {
        bytes: b"RIFF",
        extension: "webp",
        offset: 0,
    }, // WAV, AVI or WebP by its subtype
    // Archives
    MagicBytes {
        bytes: b"PK\x03\x04",
        extension: "zip",
        offset: 0,
    }, // or an Office document or EPUB, by its first part's name
    MagicBytes {
        bytes: b"PK\x05\x06",
        extension: "zip",
        offset: 0,
    },
    MagicBytes {
        bytes: b"Rar!\x1a\x07\x00",
        extension: "rar",
        offset: 0,
    }, // RAR 4.x
    MagicBytes {
        bytes: b"Rar!\x1a\x07\x01\x00",
        extension: "rar",
        offset: 0,
    }, // RAR 5.x
    MagicBytes {
        bytes: b"7z\xBC\xAF\x27\x1C",
        extension: "7z",
        offset: 0,
    },
    MagicBytes {
        bytes: b"\x1f\x8b\x08",
        extension: "gz",
        offset: 0,
    },
    MagicBytes {
        bytes: b"BZh",
        extension: "bz2",
        offset: 0,
    },
    // Video
    MagicBytes {
        bytes: b"ftyp",
        extension: "mp4",
        offset: 4,
    }, // MP4/M4V/MOV
    MagicBytes {
        bytes: b"\x1aE\xdf\xa3",
        extension: "mkv",
        offset: 0,
    }, // Matroska/WebM EBML header
    MagicBytes {
        bytes: b"\x00\x00\x01\xBA",
        extension: "mpg",
        offset: 0,
    }, // MPEG PS
    MagicBytes {
        bytes: b"\x00\x00\x01\xB3",
        extension: "mpg",
        offset: 0,
    }, // MPEG PS
    // Audio
    MagicBytes {
        bytes: b"ID3",
        extension: "mp3",
        offset: 0,
    },
    MagicBytes {
        bytes: b"\xFF\xFB",
        extension: "mp3",
        offset: 0,
    },
    MagicBytes {
        bytes: b"fLaC",
        extension: "flac",
        offset: 0,
    },
    MagicBytes {
        bytes: b"OggS",
        extension: "ogg",
        offset: 0,
    },
    // Documents
    MagicBytes {
        bytes: b"%PDF",
        extension: "pdf",
        offset: 0,
    },
    MagicBytes {
        bytes: b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1",
        extension: "doc",
        offset: 0,
    }, // OLE/DOC/XLS
    // ISO
    MagicBytes {
        bytes: b"CD001",
        extension: "iso",
        offset: 0x8001,
    },
    MagicBytes {
        bytes: b"CD001",
        extension: "iso",
        offset: 0x8801,
    },
    MagicBytes {
        bytes: b"CD001",
        extension: "iso",
        offset: 0x9001,
    },
];

/// Check if a file has a popular/meaningful extension
pub fn has_popular_extension<P: AsRef<Path>>(path: P) -> bool {
    if let Some(ext) = path.as_ref().extension() {
        if let Some(ext_str) = ext.to_str() {
            let ext_lower = ext_str.to_lowercase();
            return POPULAR_EXTENSIONS.contains(&ext_lower.as_str());
        }
    }
    false
}

/// Detect the most likely file extension (without its dot) by reading magic
/// bytes.
pub fn what_is_most_likely_extension<P: AsRef<Path>>(path: P) -> Option<&'static str> {
    // Open file and read first chunk
    let mut file = File::open(path.as_ref()).ok()?;
    let mut buffer = vec![0u8; 0x10000]; // 64KB should be enough for magic bytes

    let bytes_read = file.read(&mut buffer).ok()?;
    if bytes_read == 0 {
        return None;
    }
    let buffer = &buffer[..bytes_read];

    for magic in MAGIC_BYTES {
        if buffer.get(magic.offset..magic.offset + magic.bytes.len()) != Some(magic.bytes) {
            continue;
        }
        // Special handling for formats that share magic bytes
        match magic.bytes {
            // RIFF: the subtype says which (an unknown one is no match)
            b"RIFF" if buffer.len() >= 12 => match &buffer[8..12] {
                b"WAVE" => return Some("wav"),
                b"AVI " => return Some("avi"),
                b"WEBP" => return Some("webp"),
                _ => continue,
            },
            // ZIP-based formats: Office documents and EPUB name their parts
            // early on
            b"PK\x03\x04" if buffer.len() >= 30 => {
                let content = String::from_utf8_lossy(&buffer[..buffer.len().min(512)]);
                return Some(if content.contains("word/") {
                    "docx"
                } else if content.contains("xl/") {
                    "xlsx"
                } else if content.contains("ppt/") {
                    "pptx"
                } else if content.contains("epub") {
                    "epub"
                } else {
                    "zip"
                });
            }
            // MP4 container: could be MP4, M4V, M4A, MOV
            b"ftyp" if buffer.len() >= 12 => {
                return Some(match &buffer[8..12] {
                    b"M4A " => "m4a",
                    b"M4V " => "m4v",
                    b"qt  " => "mov",
                    _ => "mp4",
                });
            }
            _ => return Some(magic.extension),
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_popular_extensions() {
        assert!(has_popular_extension("video.mkv"));
        assert!(has_popular_extension("archive.zip"));
        assert!(has_popular_extension("archive.rar"));
        assert!(has_popular_extension("document.pdf"));
        assert!(!has_popular_extension("random.xyz"));
        assert!(!has_popular_extension("noext"));
    }

    #[test]
    fn test_mkv_detection() {
        // Create a temporary file with MKV magic bytes
        let mut temp = NamedTempFile::new().unwrap();
        // Matroska/MKV EBML header: 0x1A 0x45 0xDF 0xA3
        temp.write_all(&[0x1A, 0x45, 0xDF, 0xA3]).unwrap();
        temp.write_all(&[0x00; 100]).unwrap(); // Pad with zeros
        temp.flush().unwrap();

        let detected = what_is_most_likely_extension(temp.path());
        assert_eq!(detected, Some("mkv"));
    }

    #[test]
    fn test_rar4_detection() {
        // Create a temporary file with RAR 4.x magic bytes
        let mut temp = NamedTempFile::new().unwrap();
        // RAR 4.x: Rar!\x1A\x07\x00
        temp.write_all(b"Rar!\x1A\x07\x00").unwrap();
        temp.write_all(&[0x00; 100]).unwrap();
        temp.flush().unwrap();

        let detected = what_is_most_likely_extension(temp.path());
        assert_eq!(detected, Some("rar"));
    }

    #[test]
    fn test_rar5_detection() {
        // Create a temporary file with RAR 5.x magic bytes
        let mut temp = NamedTempFile::new().unwrap();
        // RAR 5.x: Rar!\x1A\x07\x01\x00
        temp.write_all(b"Rar!\x1A\x07\x01\x00").unwrap();
        temp.write_all(&[0x00; 100]).unwrap();
        temp.flush().unwrap();

        let detected = what_is_most_likely_extension(temp.path());
        assert_eq!(detected, Some("rar"));
    }

    /// Formats that share magic bytes are told apart by what follows.
    #[test]
    fn shared_magic_bytes_are_told_apart() {
        let detect = |bytes: &[u8]| {
            let mut temp = NamedTempFile::new().unwrap();
            temp.write_all(bytes).unwrap();
            temp.flush().unwrap();
            what_is_most_likely_extension(temp.path())
        };
        assert_eq!(detect(b"RIFF\0\0\0\0WAVEfmt "), Some("wav"));
        assert_eq!(detect(b"RIFF\0\0\0\0AVI LIST"), Some("avi"));
        assert_eq!(detect(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(detect(b"RIFF\0\0\0\0ABCDEFGH"), None);
        assert_eq!(detect(b"RIFF\0\0"), Some("webp"));

        let zip = |name: &str| {
            let mut bytes = b"PK\x03\x04".to_vec();
            bytes.extend_from_slice(&[0; 26]);
            bytes.extend_from_slice(name.as_bytes());
            bytes
        };
        assert_eq!(detect(&zip("word/document.xml")), Some("docx"));
        assert_eq!(detect(&zip("xl/workbook.xml")), Some("xlsx"));
        assert_eq!(detect(&zip("ppt/slides/1.xml")), Some("pptx"));
        assert_eq!(detect(&zip("mimetypeapplication/epub+zip")), Some("epub"));
        assert_eq!(detect(&zip("readme.txt")), Some("zip"));
        assert_eq!(detect(b"PK\x03\x04short"), Some("zip"));

        assert_eq!(detect(b"\0\0\0\x18ftypqt  \0\0"), Some("mov"));
        assert_eq!(detect(b"\0\0\0\x18ftypM4A \0\0"), Some("m4a"));
        assert_eq!(detect(b"\0\0\0\x18ftypisom\0\0"), Some("mp4"));
    }
}
