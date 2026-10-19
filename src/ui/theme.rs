//! Colour and glyph decisions, made once and in one place.
//!
//! Two things are detected at startup and then treated as fixed: whether colour
//! is welcome, and whether the terminal can render the glyphs we would like to
//! use. Both degrade to something plain rather than to something broken.

use std::io::IsTerminal;
use std::sync::OnceLock;

/// Whether ANSI colour should be emitted.
///
/// Honours the [`NO_COLOR`](https://no-color.org) convention, the more explicit
/// `CLICOLOR_FORCE`, and otherwise follows whether stderr is a terminal.
pub fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        if std::env::var_os("CLICOLOR_FORCE").is_some_and(|value| value != "0") {
            return true;
        }
        std::io::stderr().is_terminal()
    })
}

/// Whether the terminal is likely to render non-ASCII glyphs correctly.
///
/// A terminal that cannot will otherwise show replacement boxes in every log
/// line, which is worse than plain ASCII markers.
pub fn unicode_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        // A UTF-8 locale is the signal every other CLI uses for this.
        ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(std::env::var_os)
            .is_some_and(|value| {
                let value = value.to_string_lossy().to_ascii_lowercase();
                value.contains("utf-8") || value.contains("utf8")
            })
    })
}

/// The severity of a message, which selects both glyph and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Success,
    Info,
    Warn,
    Error,
    /// Low-importance detail, dimmed.
    Debug,
}

impl Level {
    /// The leading glyph, in the widest form the terminal supports.
    pub fn glyph(self) -> &'static str {
        if unicode_enabled() {
            match self {
                Self::Success => "✔",
                Self::Info => "ℹ",
                Self::Warn => "⚠",
                Self::Error => "✖",
                Self::Debug => "·",
            }
        } else {
            match self {
                Self::Success => "+",
                Self::Info => "i",
                Self::Warn => "!",
                Self::Error => "x",
                Self::Debug => ".",
            }
        }
    }

    /// The ANSI SGR parameters for this level, without the escape wrapper.
    fn ansi(self) -> &'static str {
        match self {
            Self::Success => "32",
            Self::Info => "36",
            Self::Warn => "33",
            Self::Error => "31",
            Self::Debug => "90",
        }
    }

    /// Wrap `text` in this level's colour, if colour is enabled.
    pub fn paint(self, text: &str) -> String {
        paint(self.ansi(), text)
    }
}

/// Apply raw SGR parameters to `text`, respecting [`color_enabled`].
fn paint(sgr: &str, text: &str) -> String {
    if color_enabled() {
        format!("\x1b[{sgr}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

/// Dimmed text, for timestamps and secondary detail.
pub fn dim(text: &str) -> String {
    paint("2", text)
}

/// Bold text, for names the eye should land on.
pub fn bold(text: &str) -> String {
    paint("1", text)
}

/// Cyan text, used consistently for chat and route names.
pub fn accent(text: &str) -> String {
    paint("36", text)
}

/// The arrow used to show direction, e.g. `source -> target`.
pub fn arrow() -> &'static str {
    if unicode_enabled() { "→" } else { "->" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_have_an_ascii_fallback() {
        // Whichever mode the test environment is in, every level yields a
        // non-empty marker.
        for level in [
            Level::Success,
            Level::Info,
            Level::Warn,
            Level::Error,
            Level::Debug,
        ] {
            assert_ne!(level.glyph(), "");
        }
    }

    #[test]
    fn painting_without_color_is_the_identity() {
        // `paint` is only conditional on `color_enabled`, so assert the branch
        // that does not depend on the environment.
        if !color_enabled() {
            assert_eq!(dim("hello"), "hello");
            assert_eq!(Level::Error.paint("bad"), "bad");
        }
    }
}
