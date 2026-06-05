//! Layout primitives shared across the whole run: duration formatting, name
//! truncation, control-char sanitisation, and horizontal rules.

use std::time::Duration;

use super::glyph;

/// Cap on how many files a summary / listing shows before collapsing the rest
/// into a "… and N more" row.
pub const MAX_LISTED_FILES: usize = 12;

/// Human duration: `45s` / `3m 20s` / `2h 5m`. Shared by the live ETA widget and
/// the completion summary so both speak one format.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let (m, s) = (secs / 60, secs % 60);
        if s == 0 {
            format!("{m}m")
        } else {
            format!("{m}m {s}s")
        }
    } else {
        let (h, m) = (secs / 3600, (secs % 3600) / 60);
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h {m}m")
        }
    }
}

/// ETA flavour of [`format_duration`]: same rollover, but an absurd/unknown ETA
/// (≥ 24h, e.g. a stalled transfer) renders as `—` instead of a huge number.
pub fn format_eta(d: Duration) -> String {
    if d.as_secs() >= 24 * 3600 {
        "—".to_string()
    } else {
        format_duration(d)
    }
}

/// Strip ASCII control characters (obfuscated NZB subjects sometimes embed them)
/// so they can't corrupt the terminal.
pub fn sanitize_display(input: &str) -> String {
    input.chars().filter(|c| !c.is_ascii_control()).collect()
}

/// Middle-ellipsis truncation to `max` columns, preserving the filename tail
/// (extension): `A.Very.Long.Release.Name.x265.mkv` → `A.Very.Long…x265.mkv`.
/// Width is approximated by char count (filenames are rarely wide-char).
pub fn truncate_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max || max < 5 {
        return s.to_string();
    }
    let keep = max - 1; // room for the ellipsis
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let head_s: String = chars.iter().take(head).collect();
    let tail_s: String = chars.iter().skip(chars.len() - tail).collect();
    format!("{head_s}…{tail_s}")
}

/// A horizontal rule `n` columns wide using the active rule character.
pub fn rule(n: usize) -> String {
    std::iter::repeat_n(glyph::rule_char(), n).collect()
}

/// The plural suffix for a count: `""` for 1, `"s"` otherwise.
pub fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
