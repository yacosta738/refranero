//! Full-screen composition for the interactive reader.
//!
//! Pure like the renderer: it receives the resolved size and state and returns exactly `height`
//! lines, so the session loop only has to write them.

use textwrap::core::display_width;

use crate::domain::Saying;

use super::{
    renderer, renderer::LanguageStrip, theme::Theme, Catalog, Message, RenderOptions, UiLanguage,
};

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
    /// Language used for interface text.
    pub language: UiLanguage,
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
    let catalog = Catalog::new(screen.language);

    let mut content = if screen.help {
        help_lines(&theme, width, catalog)
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
    frame.push(footer(
        width,
        screen.next_language.as_deref(),
        &theme,
        catalog,
    ));
    frame.join("\n")
}

fn entry_lines(screen: &Screen<'_>) -> Vec<String> {
    let options = RenderOptions {
        width: screen.width,
        color: screen.color,
        links: screen.links,
        interactive: true,
        language: screen.language,
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

/// Key column of the long help layout.
const HELP_KEY_COLUMN: usize = 22;

fn help_lines(theme: &Theme, width: usize, catalog: Catalog) -> Vec<String> {
    let short_rows: Vec<(&str, &str)> = vec![
        ("n", catalog.text(Message::HelpNextShort)),
        ("l", catalog.text(Message::HelpLanguageShort)),
        ("h", catalog.text(Message::HelpPreviousShort)),
        ("c", catalog.text(Message::HelpCategoryShort)),
        ("o", catalog.text(Message::HelpOpenShort)),
        ("u", catalog.text(Message::HelpUpdateShort)),
        ("?", catalog.text(Message::HelpCloseShort)),
        ("q", catalog.text(Message::HelpQuitShort)),
    ];
    let long_rows: Vec<(&str, &str)> = vec![
        (
            catalog.text(Message::HelpNextKeys),
            catalog.text(Message::HelpNextLong),
        ),
        (
            catalog.text(Message::HelpNextLanguageKeys),
            catalog.text(Message::HelpNextLanguageLong),
        ),
        (
            catalog.text(Message::HelpPreviousLanguageKeys),
            catalog.text(Message::HelpPreviousLanguageLong),
        ),
        ("c", catalog.text(Message::HelpCategoryLong)),
        ("o", catalog.text(Message::HelpOpenLong)),
        ("u", catalog.text(Message::HelpUpdateLong)),
        ("?", catalog.text(Message::HelpCloseLong)),
        (
            catalog.text(Message::HelpQuitKeys),
            catalog.text(Message::HelpQuitLong),
        ),
    ];

    // The long layout indents the keys and pads them to a column. It needs room for that column
    // plus the longest action; below that the compact layout keeps rows inside the terminal.
    let key_width = long_rows
        .iter()
        .map(|(keys, _)| display_width(keys))
        .max()
        .unwrap_or(0)
        .max(HELP_KEY_COLUMN);
    let long_width = long_rows
        .iter()
        .map(|(_, action)| 2 + key_width + display_width(action))
        .max()
        .unwrap_or(0);
    let long = width >= long_width;
    let rows = if long { &long_rows } else { &short_rows };

    let mut lines = vec![
        String::new(),
        format!("  {}", theme.eyebrow(catalog.text(Message::HelpTitle))),
        format!(
            "  {}",
            theme.rule(&"─".repeat(width.saturating_sub(4).min(40)))
        ),
        String::new(),
    ];
    for (keys, action) in rows {
        let keys = if long {
            format!("{keys:<key_width$}")
        } else {
            format!("{keys}  ")
        };
        lines.push(format!("  {}{action}", theme.link(&keys)));
    }
    lines
}

fn progress_line(progress: &ProgressView, width: usize, theme: &Theme) -> String {
    let label = format!("  {}", progress.label);
    let counters = format!("{}/{}", progress.done, progress.total);
    if progress.total == 0 {
        return theme.meta(&clip(&format!("{label}…"), width));
    }
    let percent = (progress.done * 100 / progress.total).min(100);
    let suffix = format!(" {percent:>3}%  {counters}");
    let fixed = display_width(&label) + display_width(&suffix) + 1;

    // No room for a bar: keep the phase, then drop the counters, then clip.
    if width < fixed + 4 {
        let compact = format!("{label} {percent:>3}%");
        if display_width(&compact) <= width {
            return theme.meta(&compact);
        }
        return theme.meta(&clip(&label, width));
    }

    let bar_width = (width - fixed).min(28);
    let filled = (bar_width * progress.done / progress.total).min(bar_width);
    let bar = format!(
        "{}{}",
        theme.progress(&"█".repeat(filled)),
        theme.meta(&"░".repeat(bar_width - filled))
    );
    format!("{label} {bar}{suffix}")
}

/// Truncate plain text to `width` visual columns, marking the cut with an ellipsis.
fn clip(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_owned();
    }
    let mut clipped = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let char_width = display_width(&ch.to_string());
        if used + char_width + 1 > width {
            break;
        }
        clipped.push(ch);
        used += char_width;
    }
    clipped.push('…');
    clipped
}

fn footer(width: usize, next_language: Option<&str>, theme: &Theme, catalog: Catalog) -> String {
    let language = next_language.map_or_else(
        || catalog.text(Message::FooterLanguage).to_owned(),
        |code| format!("{}→{code}", catalog.text(Message::FooterLanguage)),
    );
    let next = catalog.text(Message::FooterNext);
    let open = catalog.text(Message::FooterOpen);
    let category = catalog.text(Message::FooterCategory);
    let help = catalog.text(Message::FooterHelp);
    let update = catalog.text(Message::FooterUpdate);
    let quit = catalog.text(Message::FooterQuit);
    let candidates = [
        format!("{next} · {open} · {language} · {category} · {help} · {update} · {quit}"),
        format!("{next} · {open} · {language} · {help} · {update} · {quit}"),
        format!("{next} · {language} · {help} · {update} · {quit}"),
        format!("{next} · {open} · l/h · u · ? · q"),
        "n·o·u·l/h·?·q".to_owned(),
    ];
    let budget = width.saturating_sub(2);
    let text = candidates
        .into_iter()
        .find(|candidate| display_width(candidate) <= budget)
        .unwrap_or_else(|| "n·o·u·l/h·?·q".to_owned());
    theme.meta(&format!("  {text}"))
}

#[cfg(test)]
mod tests {
    use crate::domain::NewSaying;
    use textwrap::core::display_width;

    use super::{compose, Saying, Screen};
    use crate::presentation::UiLanguage;

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
            language: UiLanguage::Spanish,
        }
    }

    /// Screen for a given interface language.
    fn screen_in(saying: &Saying, width: usize, height: usize, language: UiLanguage) -> Screen<'_> {
        Screen {
            language,
            ..screen(saying, width, height)
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
    fn help_lists_the_manual_update_action() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen(&entry, 80, 24);
        options.help = true;
        let frame = compose(&options);
        assert!(frame.contains("Actualizar el catálogo"), "{frame}");
        assert!(frame.contains('u'), "{frame}");
    }

    #[test]
    fn english_help_lists_the_manual_update_action() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let mut options = screen_in(&entry, 80, 24, UiLanguage::English);
        options.help = true;
        let frame = compose(&options);
        assert!(frame.contains("Update the catalog"), "{frame}");
    }

    #[test]
    fn footer_advertises_the_manual_update_action() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        let frame = compose(&screen(&entry, 80, 24));
        assert!(frame.contains("u actualizar"), "{frame}");

        let english = compose(&screen_in(&entry, 80, 24, UiLanguage::English));
        assert!(english.contains("u update"), "{english}");
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
    fn progress_row_fits_every_width() {
        let entry = saying("A quien madruga, Dios le ayuda.");
        for width in [24, 30, 40, 80] {
            let mut options = screen(&entry, width, 24);
            options.progress = Some(super::ProgressView {
                label: "Actualizando Cervantes Virtual".to_owned(),
                done: 3,
                total: 5,
            });
            let frame = compose(&options);
            assert_fits(&frame, width);
            assert_exact_height(&frame, 24);
            assert!(frame.contains("Actualizando"), "width {width}: {frame}");
        }
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
        assert!(frame.contains("u · ?"), "{frame}");
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

    #[test]
    fn english_help_snapshot() {
        let entry = saying("Más vale tarde que nunca.");
        let mut options = screen_in(&entry, 80, 20, UiLanguage::English);
        options.help = true;
        insta::assert_snapshot!(compose(&options));
    }

    #[test]
    fn english_interactive_screen_snapshot() {
        let entry = saying(
            "Por muchas vueltas que dieron los sastres no sacaron el antruejo (carnaval) del martes.",
        );
        let mut options = screen_in(&entry, 80, 24, UiLanguage::English);
        options.next_language = Some("ca".to_owned());
        insta::assert_snapshot!(compose(&options));
    }

    #[test]
    fn english_footer_names_the_shortcuts_in_english() {
        let entry = saying("Más vale tarde que nunca.");
        let mut options = screen_in(&entry, 80, 24, UiLanguage::English);
        options.next_language = Some("ca".to_owned());
        let frame = compose(&options);
        assert!(
            frame.contains("n next · o open · l/h language→ca"),
            "{frame}"
        );
    }

    #[test]
    fn english_footer_adapts_to_narrow_terminals() {
        let entry = saying("Más vale tarde que nunca.");
        for width in [24, 32, 44] {
            let mut options = screen_in(&entry, width, 12, UiLanguage::English);
            options.next_language = Some("ca".to_owned());
            let frame = compose(&options);
            assert_exact_height(&frame, 12);
            assert_fits(&frame, width);
            assert!(
                frame.contains("n·o·u·l/h·?·q") || frame.contains("n next"),
                "{frame}"
            );
        }
    }

    #[test]
    fn english_help_lists_the_shortcuts_in_english() {
        let entry = saying("Más vale tarde que nunca.");
        let mut options = screen_in(&entry, 80, 20, UiLanguage::English);
        options.help = true;
        let frame = compose(&options);
        assert!(frame.contains("SHORTCUTS"), "{frame}");
        assert!(frame.contains("Another saying"), "{frame}");
        assert!(frame.contains("Open the source"), "{frame}");
    }

    #[test]
    fn help_fits_every_narrow_width() {
        let entry = saying("Más vale tarde que nunca.");
        for language in [UiLanguage::Spanish, UiLanguage::English] {
            for width in [24, 32, 44, 48, 51, 52, 80] {
                let mut options = screen_in(&entry, width, 20, language);
                options.help = true;
                assert_fits(&compose(&options), width);
            }
        }
    }

    #[test]
    fn spanish_footer_stays_the_default() {
        let entry = saying("Más vale tarde que nunca.");
        let mut options = screen(&entry, 80, 24);
        options.next_language = Some("ca".to_owned());
        let frame = compose(&options);
        assert!(
            frame.contains("n otro · o abrir · l/h idioma→ca"),
            "{frame}"
        );
    }
}
