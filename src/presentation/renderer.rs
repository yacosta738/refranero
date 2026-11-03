//! Composition of sayings into terminal output.
//!
//! This module is pure: it receives resolved [`RenderOptions`] and returns a block of text. It
//! never reads the environment, the clock, the network or the catalog, which keeps snapshots
//! deterministic.

use textwrap::{core::display_width, fill, Options, WordSeparator};

use crate::domain::Saying;

use super::{label, sanitize, theme::Theme, RenderOptions};

const INDENT: usize = 2;
/// Longest line the reader has to track. Wider terminals get margins, not longer lines.
const MAX_MEASURE: usize = 66;
const MIN_WIDTH: usize = 20;
const MIN_FULL_WIDTH: usize = 70;
const MIN_COMPACT_WIDTH: usize = 40;
const RULE: char = '─';
const GUTTER: char = '│';
const LINK_LABEL: &str = "Consultar fuente";

/// Language chips shown under the eyebrow in the interactive reader.
pub struct LanguageStrip<'a> {
    /// Caption that explains what the codes mean.
    pub label: &'a str,
    /// Language codes available for the current entry or filter.
    pub codes: &'a [String],
    /// Code of the entry on screen.
    pub current: &'a str,
}

/// Render one or more sayings into a printable block without a trailing newline.
pub fn render(sayings: &[Saying], options: &RenderOptions) -> String {
    render_with_languages(sayings, options, None)
}

/// Render a block, optionally showing the language strip under the eyebrow.
pub fn render_with_languages(
    sayings: &[Saying],
    options: &RenderOptions,
    strip: Option<&LanguageStrip<'_>>,
) -> String {
    if sayings.is_empty() {
        return String::new();
    }
    let theme = Theme::new(options.color);
    let width = options.width.max(MIN_WIDTH);
    if !options.interactive || width < MIN_COMPACT_WIDTH {
        return render_plain(sayings, width, &theme);
    }
    let measure = measure(width);
    if sayings.len() > 1 {
        render_collection(sayings, measure, &theme, options)
    } else if width >= MIN_FULL_WIDTH {
        render_full(&sayings[0], measure, &theme, options, strip)
    } else {
        render_compact(&sayings[0], measure, &theme, options, strip)
    }
}

fn measure(width: usize) -> usize {
    width.saturating_sub(INDENT).clamp(16, MAX_MEASURE)
}

fn render_full(
    saying: &Saying,
    measure: usize,
    theme: &Theme,
    options: &RenderOptions,
    strip: Option<&LanguageStrip<'_>>,
) -> String {
    let indent = " ".repeat(INDENT);
    let initial = format!("{indent}{GUTTER} ");
    let subsequent = format!("{indent}{GUTTER}  ");

    let mut lines = vec![String::new()];
    lines.push(format!(
        "{indent}{}",
        theme.eyebrow(&eyebrow(saying, false))
    ));
    if let Some(strip) = strip {
        lines.extend(strip_lines(strip, measure, theme));
    }
    lines.push(format!(
        "{indent}{}",
        theme.rule(&RULE.to_string().repeat(measure))
    ));
    lines.push(String::new());
    lines.extend(paint_quote(
        &quote_lines(saying, measure, &initial, &subsequent),
        &initial,
        &subsequent,
        theme,
        None,
    ));
    lines.push(String::new());
    lines.extend(paint_meta(
        &wrap(&meta(saying), measure, &indent, &indent),
        theme,
    ));
    lines.extend(link_lines(saying, &indent, theme, options));
    lines.join("\n")
}

fn render_compact(
    saying: &Saying,
    measure: usize,
    theme: &Theme,
    options: &RenderOptions,
    strip: Option<&LanguageStrip<'_>>,
) -> String {
    let indent = " ".repeat(INDENT);
    let subsequent = format!("{indent} ");

    let mut lines = vec![String::new()];
    lines.push(format!("{indent}{}", theme.eyebrow(&eyebrow(saying, true))));
    if let Some(strip) = strip {
        lines.extend(strip_lines(strip, measure, theme));
    }
    lines.push(String::new());
    lines.extend(paint_quote(
        &quote_lines(saying, measure, &indent, &subsequent),
        &indent,
        &subsequent,
        theme,
        None,
    ));
    lines.push(String::new());
    lines.extend(paint_meta(
        &wrap(&meta(saying), measure, &indent, &indent),
        theme,
    ));
    lines.extend(link_lines(saying, &indent, theme, options));
    lines.join("\n")
}

fn render_collection(
    sayings: &[Saying],
    measure: usize,
    theme: &Theme,
    options: &RenderOptions,
) -> String {
    let indent = " ".repeat(INDENT);
    let meta_indent = " ".repeat(6);

    let mut lines = vec![String::new()];
    lines.push(format!(
        "{indent}{}",
        theme.eyebrow(&collection_eyebrow(sayings))
    ));
    lines.push(format!(
        "{indent}{}",
        theme.rule(&RULE.to_string().repeat(measure))
    ));

    for (index, saying) in sayings.iter().enumerate() {
        let number = index + 1;
        let index_label = format!("{number:02}");
        let initial = format!("{indent}{index_label}  ");
        let subsequent = " ".repeat(INDENT + index_label.len() + 3);

        lines.push(String::new());
        lines.extend(paint_quote(
            &quote_lines(saying, measure, &initial, &subsequent),
            &initial,
            &subsequent,
            theme,
            Some(&index_label),
        ));
        lines.extend(paint_meta(
            &wrap(&meta(saying), measure, &meta_indent, &meta_indent),
            theme,
        ));
        lines.extend(link_lines(saying, &meta_indent, theme, options));
    }
    lines.join("\n")
}

fn render_plain(sayings: &[Saying], width: usize, theme: &Theme) -> String {
    let mut blocks = Vec::with_capacity(sayings.len());
    for (index, saying) in sayings.iter().enumerate() {
        let indent = if sayings.len() > 1 {
            format!("{:02} ", index + 1)
        } else {
            String::new()
        };
        let subsequent = " ".repeat(display_width(&indent));
        let mut lines = paint_quote(
            &wrap(&quoted(saying), width, &indent, &subsequent),
            &indent,
            &subsequent,
            theme,
            None,
        );
        let source = label::source(&sanitize::clean(&saying.entry.source));
        let code = sanitize::clean(&saying.entry.language);
        let candidates = [
            format!("{source} · {code} · #{}", saying.id),
            format!("{source} · #{}", saying.id),
            format!("#{}", saying.id),
        ];
        // Metadata is short and never wrapped: breaking before a separator reads as an error.
        let meta_text = candidates
            .into_iter()
            .find(|candidate| display_width(candidate) <= width)
            .unwrap_or_else(|| format!("#{}", saying.id));
        lines.push(theme.meta(&meta_text));
        lines.extend(link_lines(saying, "", theme, &RenderOptions::plain(width)));
        blocks.push(lines.join("\n"));
    }
    blocks.join("\n\n")
}

fn strip_lines(strip: &LanguageStrip<'_>, width: usize, theme: &Theme) -> Vec<String> {
    if strip.codes.is_empty() {
        return Vec::new();
    }
    let indent = " ".repeat(INDENT);
    let label = format!("{}: ", short_label(strip.label, width));
    let mut line = format!("{indent}{}", theme.meta(&label));
    let mut used = display_width(&line);
    let mut first = true;
    let mut lines = Vec::new();

    for code in strip.codes {
        let selected = code == strip.current;
        let text = if selected {
            format!("[{}]", code.to_uppercase())
        } else {
            code.to_uppercase()
        };
        let chip_width = display_width(&text);
        let gap = if first { 0 } else { 2 };
        if !first && used + gap + chip_width > width {
            lines.push(line);
            line = indent.clone();
            used = display_width(&indent);
            first = true;
        }
        let gap = if first { 0 } else { 2 };
        if gap > 0 {
            line.push_str("  ");
        }
        line.push_str(&if selected {
            theme.chip_current(&text)
        } else {
            theme.chip(&text)
        });
        used += gap + chip_width;
        first = false;
    }
    lines.push(line);
    lines
}

/// Keep the caption short when the measure cannot afford the long one.
const fn short_label(label: &str, width: usize) -> &str {
    if width < 50 {
        "Idiomas"
    } else {
        label
    }
}

fn eyebrow(saying: &Saying, code_only: bool) -> String {
    let category = label::category(&sanitize::clean(&saying.entry.category)).to_uppercase();
    let code = sanitize::clean(&saying.entry.language);
    let language = if code_only {
        code.to_uppercase()
    } else {
        label::language(&code).to_uppercase()
    };
    format!("{category} · {language}")
}

fn collection_eyebrow(sayings: &[Saying]) -> String {
    let category = sayings[0].entry.category.as_str();
    let uniform = sayings
        .iter()
        .all(|saying| saying.entry.category == category);
    let noun = if uniform {
        label::plural(category)
    } else {
        "ENTRADAS"
    };
    format!("REFRANERO · {} {noun}", sayings.len())
}

fn meta(saying: &Saying) -> String {
    format!(
        "{} · #{}",
        label::source(&sanitize::clean(&saying.entry.source)),
        saying.id
    )
}

fn quoted(saying: &Saying) -> String {
    format!("«{}»", text_of(saying))
}

fn text_of(saying: &Saying) -> String {
    let text = sanitize::clean(&saying.entry.text);
    if text.is_empty() {
        "…".to_owned()
    } else {
        text
    }
}

fn quote_lines(saying: &Saying, width: usize, initial: &str, subsequent: &str) -> Vec<String> {
    // Two columns are reserved so the closing guillemet never lands alone on its own line.
    wrap(
        &quoted(saying),
        width.saturating_sub(2),
        initial,
        subsequent,
    )
}

fn wrap(text: &str, width: usize, initial: &str, subsequent: &str) -> Vec<String> {
    let options = Options::new(width.max(8))
        .initial_indent(initial)
        .subsequent_indent(subsequent)
        .word_separator(WordSeparator::UnicodeBreakProperties)
        .break_words(true);
    fill(text, options).lines().map(str::to_owned).collect()
}

fn paint_quote(
    lines: &[String],
    initial: &str,
    subsequent: &str,
    theme: &Theme,
    index_label: Option<&str>,
) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            let (prefix, body) = split_indent(line, initial, subsequent);
            let painted_body = theme.quote(body);
            match index_label {
                Some(label) if prefix == initial => {
                    format!("{}{painted_body}", paint_index(prefix, label, theme))
                }
                Some(_) => format!("{prefix}{painted_body}"),
                None => format!("{}{painted_body}", paint_gutter(prefix, theme)),
            }
        })
        .collect()
}

/// Colour the index inside an already-built prefix, leaving its padding untouched.
fn paint_index(prefix: &str, label: &str, theme: &Theme) -> String {
    prefix.find(label).map_or_else(
        || prefix.to_owned(),
        |position| {
            format!(
                "{}{}{}",
                &prefix[..position],
                theme.index(label),
                &prefix[position + label.len()..]
            )
        },
    )
}

/// Colour the gutter glyph only; the surrounding padding stays plain.
fn paint_gutter(prefix: &str, theme: &Theme) -> String {
    prefix.replace(GUTTER, &theme.gutter(&GUTTER.to_string()))
}

/// Split a wrapped line into its indentation and its content.
///
/// The longer indent is tried first: `initial` can be a prefix of `subsequent`.
fn split_indent<'a>(line: &'a str, initial: &'a str, subsequent: &'a str) -> (&'a str, &'a str) {
    for indent in [subsequent, initial] {
        if !indent.is_empty() {
            if let Some(body) = line.strip_prefix(indent) {
                return (indent, body);
            }
        }
    }
    ("", line)
}

fn paint_meta(lines: &[String], theme: &Theme) -> Vec<String> {
    lines.iter().map(|line| theme.meta(line)).collect()
}

fn link_lines(
    saying: &Saying,
    initial: &str,
    theme: &Theme,
    options: &RenderOptions,
) -> Vec<String> {
    let raw = &saying.entry.source_url;
    if options.links {
        if let Some(target) = sanitize::safe_https_url(raw) {
            let label = theme.link(&format!("↗ {LINK_LABEL}"));
            let open = format!("\u{1b}]8;;{target}\u{1b}\\");
            let close = "\u{1b}]8;;\u{1b}\\";
            return vec![format!("{initial}{open}{label}{close}")];
        }
    }
    // URLs are never wrapped: a soft-wrapped link cannot be copied in one piece.
    sanitize::displayable_url(raw).map_or_else(Vec::new, |url| vec![format!("{initial}↗ {url}")])
}

#[cfg(test)]
mod tests {
    use crate::domain::NewSaying;
    use textwrap::core::display_width;

    use super::{render, render_with_languages, LanguageStrip, Saying};
    use crate::presentation::RenderOptions;

    const URL: &str =
        "https://www.cervantesvirtual.com/obra-visor/refranes-alusivos-al-tiempo/html/";
    const LONG_TEXT: &str = "Por muchas vueltas que dieron los sastres no sacaron el antruejo (carnaval) del martes, ni la ceniza del miércoles.";

    fn saying(id: i64, text: &str) -> Saying {
        Saying {
            id,
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

    fn interactive(width: usize) -> RenderOptions {
        RenderOptions {
            width,
            color: false,
            links: false,
            interactive: true,
        }
    }

    fn with_links(width: usize) -> RenderOptions {
        RenderOptions {
            width,
            color: false,
            links: true,
            interactive: true,
        }
    }

    /// URLs are never wrapped so they stay copyable; every other line respects the width.
    fn assert_fits_except_urls(output: &str, width: usize) {
        for line in output.lines().filter(|line| !line.contains("://")) {
            assert!(
                display_width(line) <= width,
                "line {line:?} is {} columns, exceeds {width}",
                display_width(line)
            );
        }
    }

    #[test]
    fn empty_input_renders_nothing() {
        assert_eq!(render(&[], &interactive(80)), "");
    }

    #[test]
    fn editorial_layout_keeps_a_reading_measure() {
        let output = render(&[saying(164, LONG_TEXT)], &interactive(120));
        assert_fits_except_urls(&output, 120);
        assert!(output.contains("REFRÁN · ESPAÑOL"));
        assert!(output.contains('│'));
        assert!(output.contains('«'));
        assert!(output.contains("Cervantes Virtual · #164"));
        let widest = output
            .lines()
            .filter(|line| !line.contains("://"))
            .map(display_width)
            .max()
            .unwrap_or_default();
        assert!(widest <= 68, "widest line is {widest}");
    }

    #[test]
    fn compact_layout_drops_the_rule() {
        let output = render(&[saying(164, LONG_TEXT)], &interactive(40));
        assert_fits_except_urls(&output, 40);
        assert!(output.contains("REFRÁN · ES"));
        assert!(!output.contains('─'), "{output}");
        assert!(!output.contains('│'), "{output}");
    }

    #[test]
    fn narrow_layout_falls_back_to_plain_lines() {
        let output = render(&[saying(164, LONG_TEXT)], &interactive(30));
        assert_fits_except_urls(&output, 30);
        assert!(output.contains("Cervantes Virtual"));
        assert!(output.contains(URL), "{output}");
        assert!(!output.contains('│'));
        assert!(!output.contains('─'));
    }

    #[test]
    fn collection_numbers_every_entry() {
        let sayings = [
            saying(24, "A quien madruga, Dios le ayuda."),
            saying(85, "No por mucho madrugar amanece más temprano."),
            saying(127, "Más vale tarde que nunca."),
        ];
        let output = render(&sayings, &interactive(80));
        assert_fits_except_urls(&output, 80);
        assert!(output.contains("3 REFRANES"), "{output}");
        for index in ["01", "02", "03"] {
            assert!(output.contains(index), "missing {index}: {output}");
        }
    }

    #[test]
    fn collection_header_uses_the_plural_of_a_uniform_category() {
        let mut first = saying(1, "Una.");
        let mut second = saying(2, "Otra.");
        first.entry.category = "cita".to_owned();
        second.entry.category = "cita".to_owned();
        let output = render(&[first, second], &interactive(80));
        assert!(output.contains("2 CITAS LITERARIAS"), "{output}");
    }

    #[test]
    fn redirected_output_is_plain_and_keeps_the_url() {
        let output = render(&[saying(164, LONG_TEXT)], &RenderOptions::plain(80));
        assert!(!output.contains('\x1b'));
        assert!(output.contains(URL));
        assert!(output.contains("#164"));
        assert!(!output.contains('─'));
    }

    #[test]
    fn hyperlink_is_osc8_when_supported() {
        let output = render(&[saying(164, "Corto.")], &with_links(80));
        assert!(
            output.contains("\x1b]8;;https://www.cervantesvirtual.com/"),
            "{output:?}"
        );
        assert!(output.contains("Consultar fuente"));
        assert!(output.contains("\x1b]8;;\x1b\\"), "{output:?}");
    }

    #[test]
    fn hyperlink_falls_back_to_the_full_url() {
        let output = render(&[saying(164, "Corto.")], &interactive(80));
        assert!(output.contains(URL), "{output}");
        assert!(!output.contains("\x1b]8;;"));
    }

    #[test]
    fn hyperlink_requires_a_safe_target() {
        let mut entry = saying(1, "Corto.");
        entry.entry.source_url = "javascript:alert(1)".to_owned();
        let output = render(std::slice::from_ref(&entry), &with_links(80));
        assert!(!output.contains("\x1b]8;;"), "{output:?}");
        assert!(!output.contains("javascript:"), "{output:?}");
    }

    #[test]
    fn hostile_text_is_neutralised_in_every_layout() {
        let hostile = "rojo\x1b[31mtexto\x1b[0m\x1b]8;;https://evil.example\x07\u{202E}";
        let mut entry = saying(1, hostile);
        entry.entry.source = "mi\x1b[2Jfuente".to_owned();
        for width in [120, 80, 40, 30] {
            let output = render(
                std::slice::from_ref(&entry),
                &RenderOptions {
                    width,
                    color: false,
                    links: false,
                    interactive: true,
                },
            );
            assert!(!output.contains('\x1b'), "width {width}: {output:?}");
            assert!(!output.contains("[31m"), "width {width}: {output:?}");
            assert!(
                !output.contains("evil.example"),
                "width {width}: {output:?}"
            );
        }
        let painted = render(
            std::slice::from_ref(&entry),
            &RenderOptions {
                width: 80,
                color: true,
                links: false,
                interactive: true,
            },
        );
        assert!(!painted.contains("[31m"), "{painted:?}");
        assert!(!painted.contains("evil.example"), "{painted:?}");
    }

    #[test]
    fn every_seed_saying_renders_without_ansi() {
        let seeds: Vec<Saying> =
            serde_json::from_str::<Vec<NewSaying>>(include_str!("../../data/seed.json"))
                .unwrap()
                .into_iter()
                .enumerate()
                .map(|(index, entry)| Saying {
                    id: index as i64 + 1,
                    entry,
                })
                .collect();
        let output = render(&seeds, &RenderOptions::plain(80));
        assert!(!output.contains('\x1b'));
        assert!(output.contains("Cervantes Virtual"));
    }

    #[test]
    fn long_text_wraps_without_losing_content() {
        let long = "palabra ".repeat(420);
        let output = render(&[saying(1, long.trim())], &interactive(80));
        assert_fits_except_urls(&output, 80);
        assert!(output.lines().count() > 20, "{}", output.lines().count());
        assert!(output.contains("palabra palabra"));
    }

    #[test]
    fn language_strip_marks_the_current_code() {
        let codes = vec!["es".to_owned(), "ca".to_owned(), "en".to_owned()];
        let strip = LanguageStrip {
            label: "Idiomas del catálogo",
            codes: &codes,
            current: "ca",
        };
        let output =
            render_with_languages(&[saying(164, "Corto.")], &interactive(80), Some(&strip));
        assert!(output.contains("Idiomas del catálogo:"), "{output}");
        assert!(output.contains("[CA]"), "{output}");
        assert!(!output.contains("[ES]"), "{output}");
        assert!(!output.contains("[EN]"), "{output}");
        assert_fits_except_urls(&output, 80);
    }

    #[test]
    fn language_strip_wraps_on_narrow_terminals() {
        let codes: Vec<String> = ["es", "de", "pt", "ca", "fr", "gl", "el", "en"]
            .iter()
            .map(|code| (*code).to_owned())
            .collect();
        let strip = LanguageStrip {
            label: "Otros idiomas",
            codes: &codes,
            current: "es",
        };
        let output =
            render_with_languages(&[saying(164, "Corto.")], &interactive(40), Some(&strip));
        assert_fits_except_urls(&output, 40);
        assert!(output.contains("[ES]"), "{output}");
        assert!(output.contains("Idiomas:"), "{output}");
        assert!(output.lines().count() > 7, "{output}");
    }

    #[test]
    fn plain_meta_shortens_on_narrow_terminals() {
        let output = render(&[saying(164, "Corto.")], &RenderOptions::plain(24));
        for line in output.lines().filter(|line| !line.contains("://")) {
            assert!(display_width(line) <= 24, "{line:?}");
        }
        assert!(output.contains("#164"));
    }

    #[test]
    fn wide_characters_are_measured_in_columns() {
        let text = "日本語のテキストを端末の幅に合わせて折り返します。".repeat(4);
        let output = render(&[saying(1, &text)], &interactive(40));
        assert_fits_except_urls(&output, 40);
    }

    #[test]
    fn editorial_snapshot() {
        let output = render(&[saying(164, LONG_TEXT)], &interactive(80));
        insta::assert_snapshot!(output);
    }

    #[test]
    fn compact_snapshot() {
        let output = render(&[saying(164, LONG_TEXT)], &interactive(40));
        insta::assert_snapshot!(output);
    }

    #[test]
    fn plain_snapshot() {
        let output = render(&[saying(164, LONG_TEXT)], &interactive(30));
        insta::assert_snapshot!(output);
    }

    #[test]
    fn collection_snapshot() {
        let sayings = [
            saying(24, "A quien madruga, Dios le ayuda."),
            saying(85, "No por mucho madrugar amanece más temprano."),
            saying(127, "Más vale tarde que nunca."),
        ];
        let output = render(&sayings, &interactive(80));
        insta::assert_snapshot!(output);
    }
}
