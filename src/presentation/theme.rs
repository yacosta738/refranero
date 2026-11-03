//! Semantic styles for the command-line identity.
//!
//! Only named ANSI colours are used. Truecolor values would ignore the reader's palette, which is
//! the one thing a CLI must not override.
use owo_colors::{OwoColorize, Style};

/// Semantic styles, disabled when the output does not support colour.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Theme {
    enabled: bool,
}

impl Theme {
    /// Build a theme that paints or skips ANSI sequences.
    pub const fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    fn paint(self, style: Style, text: &str) -> String {
        if self.enabled {
            text.style(style).to_string()
        } else {
            text.to_owned()
        }
    }

    /// Category and language eyebrow.
    pub fn eyebrow(self, text: &str) -> String {
        self.paint(Style::new().bold().yellow(), text)
    }

    /// Thin separator rule.
    pub fn rule(self, text: &str) -> String {
        self.paint(Style::new().dimmed(), text)
    }

    /// Quote gutter glyph.
    pub fn gutter(self, text: &str) -> String {
        self.paint(Style::new().yellow(), text)
    }

    /// Quote text, keeping the terminal foreground colour.
    pub fn quote(self, text: &str) -> String {
        self.paint(Style::new().bold(), text)
    }

    /// Source and identifier metadata.
    pub fn meta(self, text: &str) -> String {
        self.paint(Style::new().dimmed(), text)
    }

    /// Hyperlink label.
    pub fn link(self, text: &str) -> String {
        self.paint(Style::new().cyan(), text)
    }

    /// Collection index.
    pub fn index(self, text: &str) -> String {
        self.paint(Style::new().yellow(), text)
    }

    /// Language chip that is not the current one.
    pub fn chip(self, text: &str) -> String {
        self.paint(Style::new().dimmed(), text)
    }

    /// Current language chip: reversed so it reads as a selected control.
    pub fn chip_current(self, text: &str) -> String {
        self.paint(Style::new().bold().reversed(), text)
    }

    /// Filled part of the refresh progress bar.
    pub fn progress(self, text: &str) -> String {
        self.paint(Style::new().bold().yellow(), text)
    }
}

#[cfg(test)]
mod tests {
    use super::Theme;

    #[test]
    fn disabled_theme_emits_no_ansi() {
        let theme = Theme::new(false);
        for painted in [
            theme.eyebrow("REFRÁN"),
            theme.rule("───"),
            theme.quote("texto"),
            theme.meta("fuente"),
            theme.link("abrir"),
            theme.index("01"),
            theme.chip("CA"),
            theme.chip_current("[ES]"),
            theme.progress("██"),
        ] {
            assert!(!painted.contains('\x1b'), "{painted:?}");
        }
    }

    #[test]
    fn enabled_theme_wraps_text_in_ansi_and_resets() {
        let painted = Theme::new(true).eyebrow("REFRÁN");
        assert!(painted.starts_with("\x1b["), "{painted:?}");
        assert!(painted.ends_with("\x1b[0m"), "{painted:?}");
        assert!(painted.contains("REFRÁN"), "{painted:?}");
    }
}
