//! Colour resolution and a small semantic palette.
//!
//! Call sites name *intent* (`success`, `warn`, `path`, …) and never embed raw
//! SGR escapes. Colour is resolved exactly once at startup via [`init`] and read
//! everywhere through [`colors_enabled`], mirroring the global-state pattern used
//! by `crate::output_mode` / `crate::shutdown`.

use std::sync::OnceLock;

/// The user's `--color` choice, mapped from the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

static COLOR: OnceLock<bool> = OnceLock::new();

/// Resolve colour exactly once. Precedence (highest first):
///   1. `--color=never`                         -> off
///   2. `--color=always` / `CLICOLOR_FORCE!=0`  -> on (even when piped)
///   3. `NO_COLOR` present (any value)          -> off
///   4. `--color=auto` (default): on iff `auto_tty` and `TERM` != "dumb".
///
/// `auto_tty` is whether the stream this command actually writes its output to
/// is a terminal — stderr for the download/test narrative, stdout for the
/// `list` / `config` data dumps — so piping either stream yields clean output.
/// `NO_COLOR` (https://no-color.org) is honoured for `auto`, but an explicit
/// `--color=always` is a deliberate override and wins over it — matching
/// ripgrep / fd / clap's own behaviour.
pub fn init(choice: ColorChoice, auto_tty: bool) {
    let enabled = match choice {
        ColorChoice::Never => false,
        ColorChoice::Always => true,
        ColorChoice::Auto => {
            if std::env::var_os("NO_COLOR").is_some() {
                false
            } else if force_via_env() {
                true
            } else {
                auto_tty && !is_dumb_term()
            }
        }
    };
    let _ = COLOR.set(enabled);
}

fn force_via_env() -> bool {
    matches!(std::env::var("CLICOLOR_FORCE"), Ok(v) if v != "0" && !v.is_empty())
}

fn is_dumb_term() -> bool {
    matches!(std::env::var("TERM"), Ok(t) if t == "dumb")
}

/// Whether SGR colour codes should be emitted. Defaults to `false` when [`init`]
/// was never called (safe for tests / library embedders).
#[inline]
pub fn colors_enabled() -> bool {
    *COLOR.get_or_init(|| false)
}

/// Semantic colour roles. Call sites pick *intent*, never a raw colour.
#[derive(Debug, Clone, Copy)]
pub enum Style {
    Success, // green
    Warn,    // yellow
    Error,   // red
    Info,    // cyan
    Accent,  // magenta (durations, emphasis)
    Heading, // bold
    Dim,     // grey (gutters, secondary detail)
    Path,    // blue (filesystem paths)
}

impl Style {
    /// SGR parameters (the bytes between `ESC[` and `m`).
    const fn sgr(self) -> &'static str {
        match self {
            Style::Success => "32",
            Style::Warn => "33",
            Style::Error => "31",
            Style::Info => "36",
            Style::Accent => "35",
            Style::Heading => "1",
            Style::Dim => "90",
            Style::Path => "34",
        }
    }
}

/// A `Display` wrapper that emits SGR around `text` only when colour is enabled,
/// and plain text otherwise. Zero allocation; cheap to construct and composes
/// inside `format!` / `println!` with no intermediate `String`.
pub struct Painted<'a> {
    style: Style,
    text: &'a str,
}

impl std::fmt::Display for Painted<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if colors_enabled() {
            write!(f, "\x1b[{}m{}\x1b[0m", self.style.sgr(), self.text)
        } else {
            f.write_str(self.text)
        }
    }
}

/// Paint `text` with a semantic role.
#[inline]
pub fn paint(style: Style, text: &str) -> Painted<'_> {
    Painted { style, text }
}

#[inline]
pub fn success(t: &str) -> Painted<'_> {
    paint(Style::Success, t)
}
#[inline]
pub fn warn(t: &str) -> Painted<'_> {
    paint(Style::Warn, t)
}
#[inline]
pub fn error(t: &str) -> Painted<'_> {
    paint(Style::Error, t)
}
#[inline]
pub fn info(t: &str) -> Painted<'_> {
    paint(Style::Info, t)
}
#[inline]
pub fn accent(t: &str) -> Painted<'_> {
    paint(Style::Accent, t)
}
#[inline]
pub fn heading(t: &str) -> Painted<'_> {
    paint(Style::Heading, t)
}
#[inline]
pub fn dim(t: &str) -> Painted<'_> {
    paint(Style::Dim, t)
}
#[inline]
pub fn path(t: &str) -> Painted<'_> {
    paint(Style::Path, t)
}
