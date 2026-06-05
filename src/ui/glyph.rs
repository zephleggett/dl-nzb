//! One unified glyph vocabulary, with an automatic ASCII fallback for non-UTF8
//! locales / dumb terminals. Resolved once at startup via [`init`]; before that
//! it defaults to Unicode.

use std::sync::OnceLock;

static UNICODE: OnceLock<bool> = OnceLock::new();

/// Decide Unicode vs ASCII once. Unicode unless the locale looks non-UTF8 or
/// `TERM=dumb`.
pub fn init() {
    let unicode = locale_is_utf8() && !term_is_dumb();
    let _ = UNICODE.set(unicode);
}

fn locale_is_utf8() -> bool {
    // Non-UTF8 locales render the marks as mojibake; fall back to ASCII.
    let lc = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_CTYPE"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default()
        .to_ascii_uppercase();
    // Empty LANG is common on macOS Terminal.app — assume UTF-8-capable.
    lc.is_empty() || lc.contains("UTF-8") || lc.contains("UTF8")
}

fn term_is_dumb() -> bool {
    matches!(std::env::var("TERM"), Ok(t) if t == "dumb")
}

#[inline]
fn unicode() -> bool {
    *UNICODE.get_or_init(|| true)
}

/// Whether Unicode glyphs (vs the ASCII fallbacks) are in use — the
/// authoritative answer other modules (e.g. progress-bar fill chars) should ask
/// instead of inferring it from a rendered glyph.
#[inline]
pub fn is_unicode() -> bool {
    unicode()
}

/// A semantic glyph. Each maps to one Unicode mark and one ASCII fallback.
#[derive(Debug, Clone, Copy)]
pub enum Mark {
    Success,     // ✓  / [OK]
    Warn,        // ⚠  / [!]
    Error,       // ✗  / [x]   (replaces both ✗ and the emoji ❌)
    Info,        // ℹ  / [i]
    Retry,       // ↻  / [~]
    Interrupted, // ■  / [#]
}

impl Mark {
    pub fn as_str(self) -> &'static str {
        match (self, unicode()) {
            (Mark::Success, true) => "✓",
            (Mark::Success, false) => "[OK]",
            (Mark::Warn, true) => "⚠",
            (Mark::Warn, false) => "[!]",
            (Mark::Error, true) => "✗",
            (Mark::Error, false) => "[x]",
            (Mark::Info, true) => "ℹ",
            (Mark::Info, false) => "[i]",
            (Mark::Retry, true) => "↻",
            (Mark::Retry, false) => "[~]",
            (Mark::Interrupted, true) => "■",
            (Mark::Interrupted, false) => "[#]",
        }
    }
}

impl std::fmt::Display for Mark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// Convenience constants so call sites read `glyph::OK`.
pub const OK: Mark = Mark::Success;
pub const WARN: Mark = Mark::Warn;
pub const ERR: Mark = Mark::Error;
pub const INFO: Mark = Mark::Info;
pub const RETRY: Mark = Mark::Retry;
pub const INTERRUPTED: Mark = Mark::Interrupted;

/// Tree connector for a child line: `├─` for non-last, `└─` for last (ASCII
/// `|-` / `` `- ``).
pub fn branch(last: bool) -> &'static str {
    match (last, unicode()) {
        (false, true) => "├─",
        (true, true) => "└─",
        (false, false) => "|-",
        (true, false) => "`-",
    }
}

/// The horizontal-rule character (`─` / `-`).
pub fn rule_char() -> char {
    if unicode() {
        '─'
    } else {
        '-'
    }
}
