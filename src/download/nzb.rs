pub use nzb_rs::Nzb as NzbRs;
use once_cell::sync::Lazy;
use regex::Regex;
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;

use crate::error::{DlNzbError, NzbError};

static SUBJECT_FILENAME_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?:&quot;|")([^"]+)(?:&quot;|")"#).expect("valid regex"));

type Result<T> = std::result::Result<T, DlNzbError>;

/// One article of a file.
#[derive(Debug, Clone)]
pub struct NzbSegment {
    pub bytes: u64,
    pub number: u32,
    pub message_id: String,
}

impl NzbSegment {
    /// Whether the message id can be asked for at all. One that can't (a
    /// line break, a space, a control character...) is never sent: the
    /// article counts as missing.
    pub fn has_valid_id(&self) -> bool {
        crate::nntp::is_valid_message_id(&self.message_id)
    }
}

/// The message id as it goes into a command: surrounding whitespace (from a
/// pretty-printed NZB) and one pair of enclosing angle brackets (some NZBs
/// carry them) removed. Anything else stays, for [`NzbSegment::has_valid_id`]
/// to judge.
fn normalize_message_id(raw: &str) -> String {
    let id = raw.trim();
    id.strip_prefix('<')
        .and_then(|id| id.strip_suffix('>'))
        .unwrap_or(id)
        .to_string()
}

#[derive(Debug, Clone)]
pub struct NzbFile {
    /// The on-disk name for this file: extracted from the subject, sanitized,
    /// and de-duplicated across the whole NZB (case-insensitively, since the
    /// default macOS/iOS file systems are). Assigned once at parse time in NZB
    /// order, so it is stable across runs (resume relies on that) and the
    /// same in every download phase.
    pub filename: String,
    /// The newsgroups it was posted to (only names fit for a `GROUP` command).
    pub groups: Vec<String>,
    /// Its articles, in the NZB's order (by number).
    pub segments: Vec<NzbSegment>,
}

impl NzbFile {
    /// Sum of the NZB's per-segment byte counts (the posted, encoded size).
    pub fn bytes(&self) -> u64 {
        self.segments.iter().map(|s| s.bytes).sum()
    }

    pub fn is_par2(&self) -> bool {
        crate::patterns::par2::is_par2_file(Path::new(&self.filename))
    }
}

/// The NZB's `<head>` metadata.
#[derive(Debug, Clone, Default)]
pub struct NzbMeta {
    /// `<meta type="title">`, without any `{{password}}` (that is in
    /// `passwords`).
    pub title: Option<String>,
    /// Passwords for encrypted archives, in the order they are tried (after
    /// the user's): `<meta type="password">` values, then a `{{password}}`
    /// from the title, then one from the file name (`Name{{password}}.nzb`).
    pub passwords: Vec<String>,
    pub category: Option<String>,
}

impl NzbMeta {
    fn add_password(&mut self, password: String) {
        if !password.is_empty() && !self.passwords.contains(&password) {
            self.passwords.push(password);
        }
    }
}

/// Split the `Name{{password}}` convention indexers and SABnzbd use in NZB
/// file names (and sometimes titles) into the name and the password:
/// `"Show.S01E01{{s3cret}}"` gives `("Show.S01E01", Some("s3cret"))`. The
/// password runs from the first `{{` (not at the very start) to the last
/// `}}`; a name without one comes back unchanged.
pub fn split_password(name: &str) -> (String, Option<String>) {
    let unchanged = || (name.to_string(), None);
    let first_char = name.chars().next().map_or(0, char::len_utf8);
    let Some(open) = name[first_char..].find("{{").map(|i| i + first_char) else {
        return unchanged();
    };
    let Some(close) = name.rfind("}}").filter(|&close| close >= open + 2) else {
        return unchanged();
    };
    let password = name[open + 2..close].trim();
    if password.is_empty() {
        return unchanged();
    }
    let rest = format!(
        "{}{}",
        name[..open].trim_end(),
        name[close + 2..].trim_start()
    );
    (rest.trim().to_string(), Some(password.to_string()))
}

/// A parsed NZB: its files, each with its on-disk name, and its metadata.
#[derive(Debug, Clone)]
pub struct Nzb {
    files: Vec<NzbFile>,
    meta: NzbMeta,
}

impl Nzb {
    /// Parse an NZB file. A `{{password}}` in its file name
    /// (`Name{{password}}.nzb`) joins the NZB's passwords.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)?;
        let mut nzb: Nzb = content.parse()?;
        let stem = path.file_stem().map(|s| s.to_string_lossy());
        if let Some(password) = stem.and_then(|s| split_password(&s).1) {
            nzb.meta.add_password(password);
        }
        Ok(nzb)
    }

    pub fn files(&self) -> &[NzbFile] {
        &self.files
    }

    /// The `<head>` metadata (title, passwords, category).
    pub fn meta(&self) -> &NzbMeta {
        &self.meta
    }

    /// Whether any file in the NZB is a PAR2 file.
    pub fn has_par2(&self) -> bool {
        self.files.iter().any(NzbFile::is_par2)
    }

    /// A copy of this NZB keeping only the files for which `keep` returns true.
    /// Used to split a download into a data-first phase and a deferred PAR2
    /// recovery phase.
    pub fn subset<F: Fn(&NzbFile) -> bool>(&self, keep: F) -> Nzb {
        Nzb {
            files: self.files.iter().filter(|f| keep(f)).cloned().collect(),
            meta: self.meta.clone(),
        }
    }

    pub fn total_segments(&self) -> usize {
        self.files.iter().map(|file| file.segments.len()).sum()
    }

    pub fn get_filename_from_subject(subject: &str) -> Option<String> {
        SUBJECT_FILENAME_REGEX
            .captures(subject)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str().to_string())
    }
}

/// Strip DOCTYPE declaration from NZB content (nzb-rs doesn't handle DTDs).
/// Content without one is passed through as it is.
fn strip_doctype(content: &str) -> Cow<'_, str> {
    // Find and remove <!DOCTYPE ... > which can span multiple lines
    if let Some(start) = content.find("<!DOCTYPE") {
        if let Some(end) = content[start..].find('>') {
            let mut result = String::with_capacity(content.len());
            result.push_str(&content[..start]);
            result.push_str(&content[start + end + 1..]);
            return Cow::Owned(result);
        }
    }
    Cow::Borrowed(content)
}

/// Choose an on-disk name for every file, in NZB order: the quoted subject name
/// (our regex also accepts a literal `&quot;`), else nzb-rs's broader subject
/// heuristics, else `file_NNNN`. Names are sanitized and then de-duplicated
/// (`name_2.ext`) so two files can never write to the same path. The old
/// `unknown_file_{date}` fallback collided whenever several unnamed files
/// shared a post date, which is the normal case.
fn assign_filenames(files: &[nzb_rs::File]) -> Vec<String> {
    let mut used: HashSet<String> = HashSet::with_capacity(files.len());
    files
        .iter()
        .enumerate()
        .map(|(i, file)| {
            let raw = Nzb::get_filename_from_subject(&file.subject)
                .or_else(|| file.name().map(str::to_string))
                .unwrap_or_default();
            let fallback = format!("file_{:04}", i + 1);
            let name = sanitize_filename(&raw, &fallback);
            unique_name(name, &mut used)
        })
        .collect()
}

/// `name` if unused (case-insensitively), else `stem_2.ext`, `stem_3.ext`, ...
fn unique_name(name: String, used: &mut HashSet<String>) -> String {
    if used.insert(name.to_lowercase()) {
        return name;
    }
    let path = Path::new(&name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&name)
        .to_string();
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    (2..)
        .map(|k| format!("{stem}_{k}{ext}"))
        .find(|candidate| used.insert(candidate.to_lowercase()))
        .expect("an unused name exists")
}

/// Make a subject-derived name safe to use as a single path component: strip
/// any directories, control characters and characters illegal on common file
/// systems, trim surrounding dots/spaces (so it can't be hidden or `..`), and
/// cap the length. Empty results use `fallback`.
pub(crate) fn sanitize_filename(raw: &str, fallback: &str) -> String {
    // Normalise Windows separators first so `file_name()` strips them too.
    let normalised = raw.replace('\\', "/");
    let name = std::path::Path::new(&normalised)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let mut sanitized = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_control() {
            continue;
        }
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => sanitized.push('_'),
            _ => sanitized.push(ch),
        }
    }
    let trimmed = sanitized.trim().trim_matches('.').trim();
    let mut result = if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        fallback.to_string()
    } else {
        trimmed.to_string()
    };

    let max_len = 240usize;
    if result.len() > max_len {
        let mut end = 0usize;
        for (idx, _) in result.char_indices() {
            if idx > max_len {
                break;
            }
            end = idx;
        }
        if end > 0 {
            result.truncate(end);
        }
    }
    result
}

impl FromStr for Nzb {
    type Err = DlNzbError;

    fn from_str(content: &str) -> Result<Self> {
        // Strip DOCTYPE declaration if present (nzb-rs doesn't handle it)
        let content = strip_doctype(content);

        let inner = NzbRs::parse(&content)
            .map_err(|e| NzbError::ParseError(format!("Failed to parse NZB: {}", e)))?;

        let filenames = assign_filenames(&inner.files);

        let files = inner
            .files
            .iter()
            .zip(filenames)
            .map(|(file, filename)| {
                let segments = file
                    .segments
                    .iter()
                    .map(|segment| NzbSegment {
                        bytes: segment.size as u64,
                        number: segment.number,
                        message_id: normalize_message_id(&segment.message_id),
                    })
                    .collect();

                // A group name that can't go into a `GROUP` command is
                // dropped; a file left with none is skipped like one that
                // lists no newsgroup.
                let groups = file
                    .groups
                    .iter()
                    .map(|group| group.trim())
                    .filter(|group| crate::nntp::is_valid_group_name(group))
                    .map(str::to_string)
                    .collect();

                NzbFile {
                    filename,
                    groups,
                    segments,
                }
            })
            .collect::<Vec<NzbFile>>();

        let any_valid = files
            .iter()
            .flat_map(|f| &f.segments)
            .any(NzbSegment::has_valid_id);
        if !any_valid {
            return Err(NzbError::NoValidArticles.into());
        }

        let non_empty = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        };
        let (title, title_password) = match non_empty(&inner.meta.title) {
            Some(title) => {
                let (title, password) = split_password(&title);
                (Some(title).filter(|t| !t.is_empty()), password)
            }
            None => (None, None),
        };
        let mut meta = NzbMeta {
            title,
            passwords: Vec::new(),
            category: non_empty(&inner.meta.category),
        };
        for password in inner.meta.passwords.iter().map(|p| p.trim().to_string()) {
            meta.add_password(password);
        }
        if let Some(password) = title_password {
            meta.add_password(password);
        }

        Ok(Nzb { files, meta })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_xml(subject: &str, id: &str) -> String {
        format!(
            r#"<file poster="p" date="1700000000" subject="{subject}"><groups><group>a.b.c</group></groups><segments><segment bytes="10" number="1">{id}</segment></segments></file>"#
        )
    }

    #[test]
    fn filenames_are_deduplicated_and_never_empty() {
        let xml = format!(
            r#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><head><meta type="title">My Release</meta><meta type="password">s3cret</meta><meta type="category">TV</meta></head>{}{}{}{}</nzb>"#,
            file_xml("a &quot;Movie.mkv&quot; yEnc (1/1)", "1@x"),
            file_xml("b &quot;movie.MKV&quot; yEnc (1/1)", "2@x"),
            file_xml("c ????", "3@x"),
            file_xml("d ????", "4@x"),
        );
        let nzb: Nzb = xml.parse().unwrap();
        let names: Vec<&str> = nzb.files().iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names[0], "Movie.mkv");
        assert_eq!(names[1], "movie_2.MKV");
        assert_ne!(names[2], names[3], "unnamed files must not collide");
        assert_eq!(nzb.meta().title.as_deref(), Some("My Release"));
        assert_eq!(nzb.meta().passwords, vec!["s3cret".to_string()]);
        assert_eq!(nzb.meta().category.as_deref(), Some("TV"));
    }

    #[test]
    fn malformed_ids_are_kept_but_flagged_and_bad_groups_dropped() {
        let xml = r#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><file poster="p" date="1700000000" subject="&quot;a.bin&quot;"><groups><group>alt.binaries.test</group><group>alt.x&#13;&#10;QUIT</group></groups><segments><segment bytes="10" number="1">
            &lt;one@x&gt;
        </segment><segment bytes="10" number="2">two@x&#13;&#10;QUIT</segment><segment bytes="10" number="3">thr ee@x</segment></segments></file></nzb>"#;
        let nzb: Nzb = xml.parse().unwrap();
        let file = &nzb.files()[0];
        assert_eq!(file.groups, ["alt.binaries.test"]);
        // Every segment stays (positions are what resume records), but only
        // the first can be asked for.
        let segments = &file.segments;
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].message_id, "one@x");
        let valid: Vec<bool> = segments.iter().map(NzbSegment::has_valid_id).collect();
        assert_eq!(valid, [true, false, false]);
    }

    #[test]
    fn passwords_in_names_are_split_off() {
        assert_eq!(
            split_password("Show.S01E01{{s3cret}}"),
            ("Show.S01E01".to_string(), Some("s3cret".to_string()))
        );
        assert_eq!(
            split_password("My Release {{pass word}} [x]"),
            ("My Release[x]".to_string(), Some("pass word".to_string()))
        );
        assert_eq!(
            split_password("A{{p{{q}}}}"),
            ("A".to_string(), Some("p{{q}}".to_string()))
        );
        for unchanged in [
            "Plain",
            "{{only}}",
            "Empty{{  }}",
            "Open{{only",
            "Close}}{{x",
        ] {
            assert_eq!(split_password(unchanged), (unchanged.to_string(), None));
        }
    }

    #[test]
    fn nzb_passwords_come_from_meta_title_and_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let xml = format!(
            r#"<?xml version="1.0"?><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><head><meta type="title">Release {{{{from-title}}}}</meta><meta type="password">from-meta</meta><meta type="password">from-file</meta></head>{}</nzb>"#,
            file_xml("a &quot;a.rar&quot; yEnc (1/1)", "1@x"),
        );
        let path = dir.path().join("Release{{from-file}}.nzb");
        std::fs::write(&path, xml).unwrap();
        let nzb = Nzb::from_file(&path).unwrap();
        assert_eq!(nzb.meta().title.as_deref(), Some("Release"));
        assert_eq!(
            nzb.meta().passwords,
            vec!["from-meta", "from-file", "from-title"]
        );
    }

    #[test]
    fn sanitize_filename_strips_separators() {
        // file_name() strips path prefix; remaining illegal chars are replaced.
        assert_eq!(sanitize_filename("a/b<c>.mkv", "fallback"), "b_c_.mkv");
        assert_eq!(
            sanitize_filename("name:with*illegal?chars.mkv", "fb"),
            "name_with_illegal_chars.mkv"
        );
        assert_eq!(sanitize_filename("..", "fallback"), "fallback");
        assert_eq!(sanitize_filename("", "fallback"), "fallback");
        assert_eq!(sanitize_filename("..\\..\\evil.exe", "fb"), "evil.exe");
        assert_eq!(sanitize_filename(".hidden.json", "fb"), "hidden.json");
        assert_eq!(sanitize_filename("clean.mkv", "fb"), "clean.mkv");
    }

    #[test]
    fn test_nzb_api_structure() {
        let xml = r#"
        <?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
        <nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
            <head>
                <meta type="title">Test File</meta>
            </head>
            <file poster="test@example.com" date="1234567890" subject="test.zip">
                <groups>
                    <group>alt.binaries.test</group>
                </groups>
                <segments>
                    <segment bytes="1024" number="1">test@example.com</segment>
                </segments>
            </file>
        </nzb>
        "#;

        let nzb_rs = NzbRs::parse(xml).unwrap();

        // Print the structure to understand the API
        println!("Files count: {}", nzb_rs.files.len());
        if let Some(file) = nzb_rs.files.first() {
            println!("File poster: {}", file.poster);
            println!("File posted_at: {:?}", file.posted_at);
            println!("File subject: {}", file.subject);
            if let Some(segment) = file.segments.first() {
                println!("Segment size: {}", segment.size);
                println!("Segment number: {}", segment.number);
                println!("Segment message_id: {}", segment.message_id);
            }
        }

        println!("Meta title: {:?}", nzb_rs.meta.title);
        println!("Meta category: {:?}", nzb_rs.meta.category);
    }
}
