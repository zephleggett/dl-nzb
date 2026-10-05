//! `Engine::inspect`: describe an NZB from parsing alone.

use std::collections::HashMap;
use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;

use super::types::{ContentKind, FileKind, NzbFile, NzbInfo};
use crate::download::{split_password, Nzb};
use crate::error::Result;
use crate::processing::deobfuscate::name_from_title;

pub(crate) fn inspect(nzb_path: &Path) -> Result<NzbInfo> {
    let nzb = Nzb::from_file(nzb_path)?;
    let title = nzb_title(&nzb, nzb_path);

    let files: Vec<NzbFile> = nzb
        .files()
        .iter()
        .map(|f| NzbFile {
            name: f.filename.clone(),
            bytes: f.bytes(),
            segments: f.segments.segment.len() as u32,
            kind: file_kind(&f.filename),
        })
        .collect();
    let total_bytes: u64 = files.iter().map(|f| f.bytes).sum();
    let par2_bytes: u64 = files
        .iter()
        .filter(|f| f.kind == FileKind::Par2)
        .map(|f| f.bytes)
        .sum();
    let content_kind = content_kind(&files, &title);

    Ok(NzbInfo {
        title,
        passwords: nzb.meta().passwords.clone(),
        category: nzb.meta().category.clone(),
        total_bytes,
        data_bytes: total_bytes - par2_bytes,
        par2_bytes,
        files,
        content_kind,
    })
}

/// Archive volumes: `.rar`, `.partNN.rar`, `.rNN`/`.sNN`, `.7z`, `.zip`, and
/// numbered splits (`.001`, `.7z.001`).
static ARCHIVE_EXT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\.(rar|[rs]\d{2}|7z|zip|tar|gz|tgz|bz2|xz|\d{3})$").expect("valid regex")
});

/// Archive and split suffixes, stripped to find the extension of what's inside
/// (`Movie.mkv.001` -> `Movie.mkv`, `Show.part01.rar` -> `Show`).
static ARCHIVE_SUFFIX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(\.part\d+)?\.(rar|[rs]\d{2}|7z|zip|\d{3})$").expect("valid regex")
});

const METADATA_EXT: &[&str] = &[
    "nfo", "sfv", "srr", "srs", "nzb", "url", "md5", "sha1", "sha256", "diz", "log",
];

pub(crate) fn file_kind(name: &str) -> FileKind {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".par2") {
        FileKind::Par2
    } else if ARCHIVE_EXT.is_match(&lower) {
        FileKind::Archive
    } else if extension(&lower).is_some_and(|e| METADATA_EXT.contains(&e)) {
        FileKind::Other
    } else {
        FileKind::Data
    }
}

fn extension(lower_name: &str) -> Option<&str> {
    Path::new(lower_name).extension().and_then(|e| e.to_str())
}

/// The kind a file extension suggests, if it's a recognised media/document type.
fn kind_of_extension(ext: &str) -> Option<ContentKind> {
    Some(match ext {
        "mkv" | "mp4" | "m4v" | "avi" | "mov" | "wmv" | "ts" | "m2ts" | "mts" | "webm" | "mpg"
        | "mpeg" | "vob" | "flv" | "divx" | "ogm" => ContentKind::Video,
        "mp3" | "flac" | "m4a" | "m4b" | "aac" | "ogg" | "opus" | "wav" | "aiff" | "aif"
        | "alac" | "ape" | "wv" | "dsf" | "dff" | "wma" => ContentKind::Audio,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "tif" | "tiff" | "bmp"
        | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" => ContentKind::Image,
        "pdf" | "epub" | "mobi" | "azw" | "azw3" | "cbr" | "cbz" | "djvu" | "doc" | "docx"
        | "txt" | "rtf" => ContentKind::Document,
        "exe" | "msi" | "dmg" | "pkg" | "iso" | "img" | "apk" | "ipa" | "deb" | "rpm"
        | "appimage" => ContentKind::Software,
        _ => return None,
    })
}

/// Release-name tokens that identify content when the files are archives or
/// obfuscated names.
static VIDEO_HINT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(2160p|1080[pi]|720p|576p|480p|x26[45]|h\.?26[45]|hevc|avc|blu-?ray|bdrip|brrip|web-?dl|webrip|hdtv|dvdrip|remux|xvid|uhd|s\d{1,2}e\d{1,3})\b",
    )
    .expect("valid regex")
});
static AUDIO_HINT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(flac|mp3|aac|320kbps|v0|24bit|16bit|lossless|discography|vinyl)\b")
        .expect("valid regex")
});
static DOCUMENT_HINT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(e-?book|epub|pdf|mobi|azw3|comics?|magazine)\b").expect("valid regex")
});
static SOFTWARE_HINT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(macos|mac\s?os\s?x|win(32|64|dows)?|x64|x86|setup|installer|keygen|portable|multilingual)\b")
        .expect("valid regex")
});

/// Pick the content kind by dominant bytes. Archive volumes count toward what
/// they contain when the name shows it (`Movie.mkv.001`); when archives or
/// unrecognised (obfuscated) files still dominate, release-name tokens decide
/// (`1080p` means video), else it stays `Archive`/`Other`.
fn content_kind(files: &[NzbFile], title: &str) -> ContentKind {
    let mut bytes_by_kind: HashMap<ContentKind, u64> = HashMap::new();
    for file in files {
        let lower = file.name.to_ascii_lowercase();
        let kind = match file.kind {
            FileKind::Par2 | FileKind::Other => continue,
            FileKind::Archive => {
                let inner = ARCHIVE_SUFFIX.replace(&lower, "");
                extension(&inner)
                    .and_then(kind_of_extension)
                    .unwrap_or(ContentKind::Archive)
            }
            FileKind::Data => extension(&lower)
                .and_then(kind_of_extension)
                .unwrap_or(ContentKind::Other),
        };
        *bytes_by_kind.entry(kind).or_default() += file.bytes;
    }
    // Ties resolve deterministically by preferring the more specific kind.
    let order = [
        ContentKind::Video,
        ContentKind::Audio,
        ContentKind::Image,
        ContentKind::Document,
        ContentKind::Software,
        ContentKind::Archive,
        ContentKind::Other,
    ];
    let dominant = order
        .iter()
        .copied()
        .filter(|k| bytes_by_kind.get(k).copied().unwrap_or(0) > 0)
        .max_by_key(|k| {
            (
                bytes_by_kind[k],
                std::cmp::Reverse(order.iter().position(|o| o == k)),
            )
        })
        .unwrap_or(ContentKind::Other);

    if !matches!(dominant, ContentKind::Archive | ContentKind::Other) {
        return dominant;
    }
    // Hints from the release title, then from the file names themselves.
    let names = std::iter::once(title).chain(files.iter().map(|f| f.name.as_str()));
    for name in names {
        let spaced = name.replace(['.', '_'], " ");
        if VIDEO_HINT.is_match(&spaced) {
            return ContentKind::Video;
        }
        if AUDIO_HINT.is_match(&spaced) {
            return ContentKind::Audio;
        }
        if DOCUMENT_HINT.is_match(&spaced) {
            return ContentKind::Document;
        }
        if SOFTWARE_HINT.is_match(&spaced) {
            return ContentKind::Software;
        }
    }
    dominant
}

/// An NZB's title ([`NzbInfo::title`]): its `<meta type="title">`, or the
/// NZB's file name without its extension when that title is missing or looks
/// obfuscated (one seen in the wild: `4172R01e3H14n37E65f01G58y82y7191.mkv`).
/// Passwords in the title or the file name (`Name{{password}}.nzb`) are split
/// off while parsing and land in `meta().passwords`; the title never carries
/// one, since the app names the job folder after it.
pub(crate) fn nzb_title(nzb: &Nzb, nzb_path: &Path) -> String {
    // A title that makes no name at all ("..", only invisible characters)
    // counts as missing too.
    let usable = |t: &String| name_from_title(t).is_some();
    nzb.meta()
        .title
        .clone()
        .filter(|t| !title_looks_obfuscated(t) && usable(t))
        .or_else(|| {
            nzb_path
                .file_stem()
                .map(|s| split_password(&s.to_string_lossy()).0)
                .filter(usable)
        })
        .unwrap_or_else(|| "Download".to_string())
}

/// Whether a `<meta type="title">` is a scrambled token rather than a name:
/// after dropping a file extension, one unbroken run (no spaces, dots,
/// underscores or dashes) of at least 12 characters that is hex-like, at least
/// a quarter digits scattered among the letters, or high-entropy (>= 3.8 bits/char, which random
/// alphanumerics reach and words don't). Long hex-and-dot strings (a SABnzbd
/// "certainly obfuscated" pattern) count too. Real titles have word breaks or
/// are short, so a false positive only falls back to the NZB's file name.
pub(crate) fn title_looks_obfuscated(title: &str) -> bool {
    let title = title.trim();
    let path = Path::new(title);
    let stem = match (path.file_stem(), path.extension()) {
        (Some(stem), Some(ext))
            if (2..=4).contains(&ext.len())
                && ext
                    .to_string_lossy()
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric()) =>
        {
            stem.to_string_lossy().into_owned()
        }
        _ => title.to_string(),
    };
    if stem.is_empty() {
        return true;
    }
    let lower = stem.to_ascii_lowercase();
    if lower.len() >= 20 && lower.chars().all(|c| c.is_ascii_hexdigit() || c == '.') {
        return true;
    }
    let chars: Vec<char> = stem.chars().collect();
    if chars.len() < 12 || chars.iter().any(|c| matches!(c, ' ' | '.' | '_' | '-')) {
        return false;
    }
    let digits = chars.iter().filter(|c| c.is_ascii_digit()).count();
    // Digits scattered through letters (`R01e3H14n`), not a trailing year.
    let transitions = chars
        .windows(2)
        .filter(|w| w[0].is_ascii_digit() != w[1].is_ascii_digit())
        .count();
    if (digits * 4 >= chars.len() && transitions >= 4)
        || lower.chars().all(|c| c.is_ascii_hexdigit())
    {
        return true;
    }
    let mut counts: HashMap<char, usize> = HashMap::new();
    for c in &chars {
        *counts.entry(*c).or_default() += 1;
    }
    let n = chars.len() as f64;
    let entropy: f64 = counts
        .values()
        .map(|&k| {
            let p = k as f64 / n;
            -p * p.log2()
        })
        .sum();
    entropy >= 3.8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obfuscated_titles_are_recognised() {
        for t in [
            "4172R01e3H14n37E65f01G58y82y7191.mkv",
            "b082fa0beaa644d3aa01045d5b8d0b36",
            "0675e29e9abfd2.f7d069dab0b853283cc1b069a25f82.6547",
            "xKq7PzR2mW9vL4nB",
            "QwErTyUiOpAsDfGhJkLzXcVb",
        ] {
            assert!(title_looks_obfuscated(t), "{t}");
        }
        for t in [
            "The Fifth Element (1997)",
            "The.Fifth.Element.1997.2160p.UHD.BluRay.x265-LAMA",
            "Inception",
            "Supercalifragilistic",
            "Interstellar2014",
            "My_Release_Name",
        ] {
            assert!(!title_looks_obfuscated(t), "{t}");
        }
    }

    fn file(name: &str, bytes: u64) -> NzbFile {
        NzbFile {
            name: name.to_string(),
            bytes,
            segments: 1,
            kind: file_kind(name),
        }
    }

    #[test]
    fn classifies_files() {
        assert_eq!(file_kind("a.vol00+01.PAR2"), FileKind::Par2);
        assert_eq!(file_kind("a.part01.rar"), FileKind::Archive);
        assert_eq!(file_kind("a.r12"), FileKind::Archive);
        assert_eq!(file_kind("a.7z.001"), FileKind::Archive);
        assert_eq!(file_kind("a.nfo"), FileKind::Other);
        assert_eq!(file_kind("a.mkv"), FileKind::Data);
        assert_eq!(file_kind("f7e2a9c0b1d3"), FileKind::Data);
    }

    #[test]
    fn content_kind_by_dominant_bytes() {
        let files = [
            file("movie.mkv", 1000),
            file("sample.mp3", 10),
            file("x.par2", 5000),
        ];
        assert_eq!(content_kind(&files, "Some Movie"), ContentKind::Video);

        let files = [file("Album.part01.rar", 500), file("Album.part02.rar", 500)];
        assert_eq!(
            content_kind(&files, "Artist - Album (2020) FLAC"),
            ContentKind::Audio
        );
        assert_eq!(content_kind(&files, "Something"), ContentKind::Archive);

        let files = [file("Movie.mkv.001", 500), file("Movie.mkv.002", 500)];
        assert_eq!(content_kind(&files, "x"), ContentKind::Video);

        let files = [file("a8f9c0d1e2b3", 9000)];
        assert_eq!(
            content_kind(&files, "Show.S01E02.1080p.WEB-DL"),
            ContentKind::Video
        );
        assert_eq!(content_kind(&files, "abc"), ContentKind::Other);
    }
}
