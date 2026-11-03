//! Terminal capability detection. The only module here that reads the environment.
use std::env;
use std::io::IsTerminal;

use super::RenderOptions;

/// User intent for ANSI styling, in decreasing priority: flag, `NO_COLOR`, detection.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum ColorMode {
    /// Style only when stdout is an interactive terminal and `NO_COLOR` is unset.
    #[default]
    Auto,
    /// Always style, even when stdout is redirected.
    Always,
    /// Never style.
    Never,
}

/// Detect the rendering context of stdout once per invocation.
pub fn detect(mode: ColorMode) -> RenderOptions {
    let interactive = std::io::stdout().is_terminal();
    let color = resolve_color(
        mode,
        interactive,
        env::var("NO_COLOR").ok().as_deref(),
        env::var("TERM").ok().as_deref(),
    );
    RenderOptions {
        width: terminal_width(interactive),
        color,
        links: interactive && color,
        interactive,
    }
}

/// Decide whether ANSI styling is allowed. Pure, so tests never mutate the environment.
pub fn resolve_color(
    mode: ColorMode,
    interactive: bool,
    no_color: Option<&str>,
    term: Option<&str>,
) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => {
            interactive
                && no_color.is_none_or(str::is_empty)
                && !term.is_some_and(|value| value.eq_ignore_ascii_case("dumb"))
        }
    }
}

/// Resolve the usable width: terminal report, then `COLUMNS`, then a safe default.
pub fn terminal_width(interactive: bool) -> usize {
    if interactive {
        if let Some((width, _)) = terminal_size::terminal_size() {
            let width = width.0 as usize;
            if width > 0 {
                return width;
            }
        }
    }
    env::var("COLUMNS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|width| *width > 0)
        .unwrap_or(80)
}

#[cfg(test)]
mod tests {
    use super::{resolve_color, ColorMode};

    #[test]
    fn flag_wins_over_detection() {
        assert!(resolve_color(
            ColorMode::Always,
            false,
            Some("1"),
            Some("dumb")
        ));
        assert!(!resolve_color(ColorMode::Never, true, None, None));
    }

    #[test]
    fn auto_requires_an_interactive_terminal() {
        assert!(resolve_color(
            ColorMode::Auto,
            true,
            None,
            Some("xterm-256color")
        ));
        assert!(!resolve_color(
            ColorMode::Auto,
            false,
            None,
            Some("xterm-256color")
        ));
    }

    #[test]
    fn auto_respects_no_color_convention() {
        assert!(!resolve_color(
            ColorMode::Auto,
            true,
            Some("1"),
            Some("xterm")
        ));
        assert!(resolve_color(
            ColorMode::Auto,
            true,
            Some(""),
            Some("xterm")
        ));
        assert!(resolve_color(ColorMode::Auto, true, None, Some("xterm")));
    }

    #[test]
    fn auto_disables_color_for_dumb_terminals() {
        assert!(!resolve_color(ColorMode::Auto, true, None, Some("DUMB")));
    }
}
