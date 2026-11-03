//! Neutralisation of terminal control sequences and hyperlink targets.
//!
//! Imported records are stored verbatim; safety is enforced when composing terminal output.
use std::iter::Peekable;
use std::str::Chars;

/// Bidi overrides and isolates that let a string reverse or hide visible text.
const BIDI_CONTROLS: [char; 9] = [
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}',
];

/// Return `input` without control sequences, collapsed whitespace or bidi overrides.
pub fn clean(input: &str) -> String {
    let mut cleaned = String::with_capacity(input.len());
    let mut pending_space = false;
    let mut chars = input.chars().peekable();
    while let Some(raw) = chars.next() {
        // Escape sequences are removed whole, so no visible residue such as `[31m` remains.
        match raw {
            '\u{1b}' => {
                skip_escape(&mut chars);
                continue;
            }
            '\u{9B}' => {
                skip_control_sequence(&mut chars);
                continue;
            }
            '\u{9D}' | '\u{90}' | '\u{98}' | '\u{9E}' | '\u{9F}' => {
                skip_string(&mut chars);
                continue;
            }
            _ => {}
        }
        let ch = match raw {
            '\n' | '\r' | '\t' | '\u{0B}' | '\u{0C}' => ' ',
            other => other,
        };
        if ch == ' ' {
            pending_space = !cleaned.is_empty();
            continue;
        }
        if is_forbidden(ch) {
            continue;
        }
        if pending_space {
            cleaned.push(' ');
            pending_space = false;
        }
        cleaned.push(ch);
    }
    cleaned
}

fn is_forbidden(ch: char) -> bool {
    ch.is_control() || BIDI_CONTROLS.contains(&ch)
}

/// Skip the sequence introduced by `ESC`, dispatching on its introducer byte.
fn skip_escape(chars: &mut Peekable<Chars<'_>>) {
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            skip_control_sequence(chars);
        }
        Some(']') => {
            chars.next();
            skip_string(chars);
        }
        Some('P' | 'X' | '^' | '_') => {
            chars.next();
            skip_string(chars);
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

/// Skip a CSI sequence: parameters until a final byte in `0x40..=0x7E`.
fn skip_control_sequence(chars: &mut Peekable<Chars<'_>>) {
    for ch in chars.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&ch) {
            break;
        }
    }
}

/// Skip an OSC or DCS payload, which ends with BEL, ST or the C1 ST control.
///
/// An ESC that does not start a string terminator is left for [`skip_escape`], so the escape
/// sequence that follows it is still neutralised instead of being printed literally.
fn skip_string(chars: &mut Peekable<Chars<'_>>) {
    while let Some(&ch) = chars.peek() {
        match ch {
            '\u{7}' | '\u{9C}' => {
                chars.next();
                break;
            }
            '\u{1b}' => {
                let mut lookahead = chars.clone();
                lookahead.next();
                if lookahead.peek() == Some(&'\\') {
                    chars.next();
                    chars.next();
                }
                break;
            }
            _ => {
                chars.next();
            }
        }
    }
}

/// Return a normalised target when `candidate` is an absolute HTTPS URL.
///
/// The result is safe to embed in an OSC 8 sequence: it contains no control characters.
pub fn safe_https_url(candidate: &str) -> Option<String> {
    parse_web_url(candidate)
        .filter(|parsed| parsed.scheme() == "https")
        .map(|parsed| parsed.to_string())
}

/// Return a normalised target when `candidate` is an absolute HTTP(S) URL.
pub fn displayable_url(candidate: &str) -> Option<String> {
    parse_web_url(candidate).map(|parsed| parsed.to_string())
}

fn parse_web_url(candidate: &str) -> Option<url::Url> {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.chars().any(char::is_control) {
        return None;
    }
    let parsed = url::Url::parse(candidate).ok()?;
    if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
        return None;
    }
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::{clean, displayable_url, safe_https_url};

    #[test]
    fn strips_ansi_csi_sequences_without_residue() {
        assert_eq!(clean("rojo\x1b[31mtexto\x1b[0m"), "rojotexto");
    }

    #[test]
    fn strips_osc_and_control_bytes() {
        let hostile = "a\x1b]8;;https://evil.example\x07b\x1b\\c\x7Fd";
        let output = clean(hostile);
        assert_eq!(output, "abcd");
        assert!(!output.chars().any(char::is_control));
    }

    #[test]
    fn strips_eight_bit_c1_sequences() {
        assert_eq!(clean("a\u{9B}2Jb"), "ab");
        assert_eq!(clean("a\u{9D}0;t\u{7}b"), "ab");
        for introducer in ['\u{90}', '\u{98}', '\u{9E}', '\u{9F}'] {
            let hostile = format!("a{introducer}payload\u{9C}b");
            assert_eq!(clean(&hostile), "ab", "introducer {introducer:?}");
        }
    }

    #[test]
    fn keeps_a_nested_escape_visible_to_the_cleaner() {
        // An ESC inside an OSC payload that does not start an ST must not be swallowed.
        assert_eq!(clean("a\x1b]0;title\x1b[31mb"), "ab");
    }

    #[test]
    fn strips_bidi_overrides_and_keeps_marks() {
        assert_eq!(clean("a\u{202E}b\u{2069}c"), "abc");
        assert_eq!(clean("a\u{200F}b"), "a\u{200F}b");
    }

    #[test]
    fn turns_line_breaks_and_tabs_into_single_spaces() {
        assert_eq!(clean("  a\n\n\tb  \r\n"), "a b");
    }

    #[test]
    fn keeps_legitimate_unicode() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(clean(family), family);
        assert_eq!(clean("Añoranza, ¿qué más da?"), "Añoranza, ¿qué más da?");
        assert_eq!(clean("日本語のテキスト"), "日本語のテキスト");
    }

    #[test]
    fn is_idempotent_and_handles_empty_input() {
        let once = clean("a\x1bb\n c");
        assert_eq!(clean(&once), once);
        assert_eq!(clean(""), "");
        assert_eq!(clean("\x1b[2J\n"), "");
    }

    #[test]
    fn accepts_only_absolute_https_urls() {
        assert_eq!(
            safe_https_url("https://www.cervantesvirtual.com/obra").as_deref(),
            Some("https://www.cervantesvirtual.com/obra")
        );
        assert_eq!(safe_https_url("http://www.cervantesvirtual.com"), None);
        assert_eq!(safe_https_url("javascript:alert(1)"), None);
        assert_eq!(safe_https_url("file:///etc/passwd"), None);
        assert_eq!(safe_https_url("/relative"), None);
        assert_eq!(safe_https_url("https://example.com/\u{1b}"), None);
    }

    #[test]
    fn accepts_only_absolute_web_urls_for_display() {
        assert_eq!(
            displayable_url("http://example.com/x").as_deref(),
            Some("http://example.com/x")
        );
        assert_eq!(displayable_url("javascript:alert(1)"), None);
        assert_eq!(displayable_url("file:///etc/passwd"), None);
        assert_eq!(displayable_url(""), None);
    }
}
