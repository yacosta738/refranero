//! Full-screen composition for the interactive reader.
//!
//! Pure like the renderer: it receives the resolved size and state and returns exactly `height`
//! lines, so the session loop only has to write them.

use textwrap::core::display_width;

use crate::domain::Saying;

use super::{renderer, renderer::LanguageStrip, theme::Theme, RenderOptions};

/// Everything needed to compose one frame.
pub struct Screen<'a> {
    /// Entry being read.
    pub saying: &'a Saying,
    /// Language code of the next variant, when the entry belongs to a group.
    pub next_language: Option<String>,
    /// Transient note shown above the footer.
    pub message: Option<String>,
    /// Show the key map instead of the entry.
    pub help: bool,
    /// Terminal columns.
    pub width: usize,
    /// Terminal rows.
    pub height: usize,
    /// Language codes offered by the reader, highlighted by the entry language.
    pub languages: Vec<String>,
    /// Caption for the language strip, when there is something to show.
    pub languages_label: Option<String>,
    /// Refresh progress published by the background worker.
    pub progress: Option<ProgressView>,
    /// ANSI styling enabled.
    pub color: bool,
    /// OSC 8 hyperlinks enabled.
    pub links: bool,
}

/// Background refresh status for the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressView {
    /// What is being refreshed, for example `Actualizando Wayback A`.
    pub label: String,
    /// Units completed.
    pub done: usize,
    /// Units in this phase.
    pub total: usize,
}

/// Compose one frame with exactly `height` lines.
pub fn compose(screen: &Screen<'_>) -> String {
    let width = screen.width.max(24);
    let height = screen.height.max(8);
    let theme = Theme::new(screen.color);

    let mut content = if screen.help {
        help_lines(&theme, width)
    } else {
        entry_lines(screen)
    };

    let reserved =
        1 + usize::from(screen.message.is_some()) + usize::from(screen.progress.is_some());
    let available = height.saturating_sub(reserved).max(1);
    if content.len() > available {
        content.truncate(available);
        if let Some(last) = content.last_mut() {
            *last = theme.meta("  …");
        }
    }

    let top = (available - content.len()) / 2;
    let mut frame = vec![String::new(); top];
    frame.extend(content);
    while frame.len() < available {
        frame.push(String::new());
    }
    if let Some(message) = screen.message.as_deref() {
        frame.push(theme.meta(&format!("  {message}")));
    }
    if let Some(progress) = screen.progress.as_ref() {
        frame.push(progress_line(progress, width, &theme));
    }
    frame.push(footer(width, screen.next_language.as_deref(), &theme));
    frame.join("\n")
}

fn entry_lines(screen: &Screen<'_>) -> Vec<String> {
    let options = RenderOptions {
        width: screen.width,
        color: screen.color,
        links: screen.links,
        interactive: true,
    };
    let strip = screen
        .languages_label
        .as_deref()
        .map(|label| LanguageStrip {
            label,
            codes: &screen.languages,
            current: &screen.saying.entry.language,
        });
    let block = renderer::render_with_languages(
        std::slice::from_ref(screen.saying),
        &options,
        strip.as_ref(),
    );
    block
        .trim_start_matches('\n')
        .lines()
        .map(str::to_owned)
        // An oversized URL would overlap the next absolute-positioned row; the `o` shortcut
        // already exposes the source in the interactive reader.
        .filter(|line| !line.contains("://") || display_width(line) <= screen.width)
        .collect()
}

fn help_lines(theme: &Theme, width: usize) -> Vec<String> {
    let rows: &[(&str, &str)] = if width < 44 {
        &[
            ("n", "otro"),
            ("l", "idioma"),
            ("h", "anterior"),
            ("c", "categoría"),
            ("o", "abrir"),
            ("?", "cerrar"),
            ("q", "salir"),
        ]
    } else {
        &[
            ("n / Enter / Espacio", "Otro refrán"),
            ("l / →", "Siguiente idioma o variante"),
            ("h / ←", "Idioma o variante anterior"),
            ("c", "Siguiente categoría"),
            ("o", "Abrir la fuente"),
            ("?", "Cerrar esta ayuda"),
            ("q / Esc", "Salir"),
        ]
    };

    let mut lines = vec![
        String::new(),
        format!("  {}", theme.eyebrow("ATAJOS")),
        format!(
            "  {}",
            theme.rule(&"─".repeat(width.saturating_sub(4).min(40)))
        ),
        String::new(),
    ];
    for (keys, action) in rows {
        let keys = if width < 44 {
            format!("{keys}  ")
        } else {
            format!("{keys:<22}")
        };
        lines.push(format!("  {}{action}", theme.link(&keys)));
    }
    lines
}

fn progress_line(progress: &ProgressView, width: usize, theme: &Theme) -> String {
    let label = format!("  {}", progress.label);
    if progress.total == 0 {
        return theme.meta(&format!("{label}…"));
    }
    let percent = (progress.done * 100 / progress.total).min(100);
    let suffix = format!(" {percent:>3}%  {}/{}", progress.done, progress.total);
    let fixed = display_width(&label) + display_width(&suffix) + 2;
    let bar_width = width.saturating_sub(fixed).clamp(8, 28);
    let filled = (bar_width * progress.done / progress.total).min(bar_width);
    let bar = format!(
        "{}{}",
        theme.progress(&"█".repeat(filled)),
        theme.meta(&"░".repeat(bar_width - filled))
    );
    format!("{label} {bar}{suffix}")
}

fn footer(width: usize, next_language: Option<&str>, theme: &Theme) -> String {
    let language = next_language.map_or_else(
        || "l/h idioma".to_owned(),
        |code| format!("l/h idioma→{code}"),
    );
    let candidates = [
        format!("n otro · o abrir · {language} · c categoría · ? ayuda · q salir"),
        format!("n otro · o abrir · {language} · ? ayuda · q salir"),
        "n otro · o abrir · l/h idioma · q salir".to_owned(),
        "n otro · o abrir · l/h · ? · q".to_owned(),
        "n·o·l/h·?·q".to_owned(),
    ];
    let budget = width.saturating_sub(2);
    let text = candidates
        .into_iter()
        .find(|candidate| display_width(candidate) <= budget)
        .unwrap_or_else(|| "n·o·l/h·?·q".to_owned());
    theme.meta(&format!("  {text}"))
}

#[cfg(test)]
mod tests {
    use crate::domain::NewSaying;
    use textwrap::core::display_width;

    use super::{compose, Saying, Screen};

    const URL: &str =
        "https://www.cervantesvirtual.com/obra-visor/refranes-alusivos-al-tiempo/html/";

    fn saying(text: &str) -> Saying {
        Saying {
            id: 164,
            entry: NewSaying {
                text: text.to_owned(),
                category: "refran".to_owned(),
                language: "es".to_owned(),
                source: "cervantes_virtual".to_owned(),
                source_url: URL.to_owned(),
                original_url: None,
                captured_at: None,
                group_id: None,
            },
        }
    }

    fn screen(saying: &Saying, width: usize, height: usize) -> Screen<'_> {
        Screen {
            saying,
            next_language: None,
            message: None,
            help: false,
            width,
            height,
            languages: Vec::new(),
            languages_label: None,
            progress: None,
            color: false,
            links: false,
        }
    }

    fn assert_exact_height(frame: &str, height: usize) {
        assert_eq!(frame.lines().count(), height, "{frame}");
    }

    fn assert_fits(frame: &str, width: usize) {
        for line in frame.lines() {
            assert!(
                display_width(line) <= width,
                "line {line:?} is {} columns, exceeds {width}",
                display_width(line)
            );
        }
    }

    #[test]
    fn frame_has_exactly_the_requested_height() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        for height in [8, 12, 24, 40] {
            let frame = compose(&screen(&entry, 80, height));
            assert_exact_height(&frame, height);
            assert_fits(&frame, 80);
        }
    }

    #[test]
    fn footer_adapts_to_narrow_terminals() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        for width in [80, 40, 24] {
            let frame = compose(&screen(&entry, width, 24));
            let last = frame.lines().last().unwrap_or_default();
            assert!(last.contains('q'), "missing footer at {width}: {frame}");
            assert_fits(&frame, width);
        }
    }

    #[test]
    fn footer_hints_the_next_variant_language() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen(&entry, 80, 24);
        options.next_language = Some("ca".to_owned());
        let frame = compose(&options);
        assert!(frame.contains("idioma→ca"), "{frame}");
    }

    #[test]
    fn message_sits_above_the_footer() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen(&entry, 80, 24);
        options.message = Some("Abriendo fuente…".to_owned());
        let frame = compose(&options);
        assert_exact_height(&frame, 24);
        let lines: Vec<&str> = frame.lines().collect();
        assert!(lines[22].contains("Abriendo"), "{frame}");
        assert!(lines[23].contains('q'), "{frame}");
    }

    #[test]
    fn help_replaces_the_entry() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen(&entry, 80, 24);
        options.help = true;
        let frame = compose(&options);
        assert!(frame.contains("ATAJOS"), "{frame}");
        assert!(!frame.contains('«'), "{frame}");
    }

    #[test]
    fn progress_bar_fills_with_completion() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen(&entry, 80, 24);
        options.progress = Some(super::ProgressView {
            label: "Actualizando Wayback A".to_owned(),
            done: 5,
            total: 10,
        });
        let frame = compose(&options);
        assert!(frame.contains('█'), "{frame}");
        assert!(frame.contains('░'), "{frame}");
        assert!(frame.contains("50%"), "{frame}");
        assert!(frame.contains("5/10"), "{frame}");
        assert_fits(&frame, 80);
        assert_exact_height(&frame, 24);

        options.progress = Some(super::ProgressView {
            label: "Actualizando Wayback A".to_owned(),
            done: 10,
            total: 10,
        });
        let done = compose(&options);
        assert!(!done.contains('░'), "{done}");
        assert!(done.contains("100%"), "{done}");
    }

    #[test]
    fn progress_bar_sits_above_the_footer() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen(&entry, 80, 24);
        options.progress = Some(super::ProgressView {
            label: "Actualizando Cervantes Virtual".to_owned(),
            done: 2,
            total: 5,
        });
        let frame = compose(&options);
        let lines: Vec<&str> = frame.lines().collect();
        assert!(lines[22].contains("Cervantes"), "{frame}");
        assert!(lines[23].contains('q'), "{frame}");
    }

    #[test]
    fn progress_snapshot() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen(&entry, 80, 24);
        options.progress = Some(super::ProgressView {
            label: "Actualizando Wayback A".to_owned(),
            done: 4,
            total: 10,
        });
        insta::assert_snapshot!(compose(&options));
    }

    #[test]
    fn language_strip_is_visible_in_the_frame() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen(&entry, 80, 24);
        options.languages = vec!["es".to_owned(), "ca".to_owned(), "en".to_owned()];
        options.languages_label = Some("Idiomas del catálogo".to_owned());
        let frame = compose(&options);
        assert!(frame.contains("Idiomas del catálogo:"), "{frame}");
        assert!(frame.contains("[ES]"), "{frame}");
        assert_fits(&frame, 80);
        assert_exact_height(&frame, 24);
    }

    #[test]
    fn oversized_url_is_dropped_instead_of_overlapping() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen(&entry, 40, 24);
        options.links = false;
        let frame = compose(&options);
        assert_fits(&frame, 40);
        assert!(!frame.contains("://"), "{frame}");
        assert!(frame.contains("o abrir"), "{frame}");
    }

    #[test]
    fn long_text_is_truncated_with_a_marker() {
        let entry = saying(&"palabra ".repeat(400));
        let frame = compose(&screen(&entry, 80, 12));
        assert_exact_height(&frame, 12);
        assert!(frame.contains('…'), "{frame}");
    }

    #[test]
    fn language_strip_snapshot() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen(&entry, 80, 24);
        options.languages = vec!["es".to_owned(), "ca".to_owned(), "en".to_owned()];
        options.languages_label = Some("Idiomas del catálogo".to_owned());
        insta::assert_snapshot!(compose(&options));
    }

    #[test]
    fn interactive_screen_snapshot() {
        let entry = saying(
            "Por muchas vueltas que dieron los sastres no sacaron el antruejo (carnaval) del martes.",
        );
        let mut options = screen(&entry, 80, 24);
        options.next_language = Some("ca".to_owned());
        insta::assert_snapshot!(compose(&options));
    }

    #[test]
    fn narrow_screen_snapshot() {
        let entry = saying("Más vale tarde que nunca.");
        insta::assert_snapshot!(compose(&screen(&entry, 40, 16)));
    }

    #[test]
    fn help_snapshot() {
        let entry = saying("Más vale tarde que nunca.");
        let mut options = screen(&entry, 80, 20);
        options.help = true;
        insta::assert_snapshot!(compose(&options));
    }
}
