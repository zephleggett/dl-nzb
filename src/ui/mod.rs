//! Terminal UI layer: one visual language for the whole run.
//!
//! All human/progress output goes to **stderr** (stdout is reserved for `--json`
//! and `list` data). Every printer here early-returns in quiet/JSON mode, so the
//! callers don't each repeat the guard.
//!
//! Left-gutter grammar:
//! ```text
//! Release.Name · 18.1 GiB · 186 files     header (flush-left, no glyph)
//!   ├─ ✓ Downloaded …                      child lines, one level in
//!   └─ ℹ 18.1 GiB in 9m 15s                last child closes with └─
//! ```

pub mod glyph;
pub mod layout;
pub mod style;

pub use layout::{
    format_duration, format_eta, rule, sanitize_display, truncate_middle, MAX_LISTED_FILES,
};

use indicatif::ProgressBar;

/// Print a flush-left header/banner line (e.g. the release name). No-op in quiet.
pub fn header(line: impl std::fmt::Display) {
    if crate::output_mode::is_quiet() {
        return;
    }
    eprintln!("{line}");
}

/// Print a blank separator line. No-op in quiet.
pub fn blank() {
    if crate::output_mode::is_quiet() {
        return;
    }
    eprintln!();
}

/// Print one child status line under the current header:
/// `  ├─ <body>` (or `  └─ <body>` when `last`). The body already carries its
/// own glyph + colour. No-op in quiet.
pub fn child(last: bool, body: impl std::fmt::Display) {
    if crate::output_mode::is_quiet() {
        return;
    }
    eprintln!("  {} {}", style::dim(glyph::branch(last)), body);
}

/// Finish a stage's progress bar cleanly: clear the live bar (no leftover
/// `100%` skeleton), then optionally print one persistent child line for it.
/// Multi-line results should pass `None` and emit their own [`child`] lines.
pub fn finish_clean(bar: &ProgressBar, child_body: Option<String>) {
    bar.finish_and_clear();
    if let Some(body) = child_body {
        child(false, body);
    }
}

/// A grouped child emitter that assigns `├─` to every row and `└─` to the last
/// one *after* all rows are known — so the "stacked `└─`" bug is impossible.
#[derive(Default)]
pub struct Tree {
    rows: Vec<String>,
}

impl Tree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, body: impl Into<String>) -> &mut Self {
        self.rows.push(body.into());
        self
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Emit all rows with correct `├─` / `└─` markers. No-op in quiet/empty.
    pub fn emit(&self) {
        if crate::output_mode::is_quiet() {
            return;
        }
        let last = self.rows.len().saturating_sub(1);
        for (i, body) in self.rows.iter().enumerate() {
            child(i == last, body);
        }
    }
}
