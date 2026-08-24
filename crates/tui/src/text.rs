//! Text-hygiene helpers for the code paths that render foreign error text.

/// Whether `c` can reorder or hide neighbouring text without occupying a cell of its own.
///
/// `char::is_control` covers only the Cc block; the bidi overrides and isolates are Cf and pass
/// straight through it. A remote error body containing U+202E controls how the row *renders*
/// independently of what it says, which is the Trojan-Source spoofing shape.
fn is_invisible_formatting(c: char) -> bool {
    matches!(c,
        '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{2069}'
        | '\u{FEFF}')
}

/// The first line of `s`, with control and invisible-formatting characters removed and the ends
/// trimmed.
///
/// Error text from a provider or from the OS credential store reaches a terminal cell verbatim.
/// A raw `ESC` in a cell is an escape-sequence injection. The newline hazard is the engine log,
/// whose `ListItem::new(String)` builds a `Text` that splits on `.lines()`, so one field becomes
/// an unbounded block; the modal renders a `Line`, which does not split — its hazard is length,
/// which [`truncate_chars`] handles.
pub(crate) fn one_line(s: &str) -> String {
    s.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control() && !is_invisible_formatting(*c))
        .collect::<String>()
        .trim()
        .to_string()
}

/// `s` capped at `max` characters, with a trailing ellipsis when it was cut.
///
/// Characters, not bytes: slicing a multi-byte message at a byte offset panics.
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_keeps_only_the_first_line() {
        assert_eq!(one_line("first\nsecond\nthird"), "first");
        assert_eq!(one_line("only"), "only");
        assert_eq!(one_line(""), "");
    }

    /// Error text is written into a terminal cell verbatim; a raw `ESC` in a cell is an
    /// escape-sequence injection, and a tab or a carriage return corrupts the row.
    #[test]
    fn one_line_strips_control_characters() {
        assert_eq!(one_line("a\u{1b}[31mb\tc"), "a[31mbc");
        assert_eq!(one_line("a\rb"), "ab");
    }

    /// `char::is_control` covers only the Cc block. U+202E and its neighbours are Cf: they survive
    /// the filter and let a hostile error body control how a row *renders* independently of what it
    /// says — the Trojan-Source shape, on a modal this repo already treats as a phishing surface.
    #[test]
    fn one_line_strips_bidi_and_invisible_formatting() {
        for c in [
            '\u{202E}', '\u{2066}', '\u{200F}', '\u{FEFF}', '\u{2060}', '\u{200B}',
        ] {
            let rendered = one_line(&format!("a{c}b"));
            assert_eq!(rendered, "ab", "U+{:04X} survived", c as u32);
        }
    }

    /// `str::lines` treats `\r\n` as one terminator, so a message that opens with a blank line
    /// yields an empty first line. That is the pre-existing `summarize_provider_error` behaviour
    /// and this refactor must preserve it — changing it would be a behaviour change wearing a
    /// refactor's clothes.
    #[test]
    fn one_line_does_not_skip_a_leading_blank_line() {
        assert_eq!(one_line("\r\nafter"), "");
        assert_eq!(one_line("\nafter"), "");
    }

    #[test]
    fn one_line_trims_the_ends() {
        assert_eq!(one_line("   padded   \nnext"), "padded");
    }

    #[test]
    fn truncate_chars_leaves_a_short_string_alone() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("exactly10!", 10), "exactly10!");
    }

    #[test]
    fn truncate_chars_appends_an_ellipsis_when_it_cuts() {
        assert_eq!(truncate_chars("abcdef", 3), "abc\u{2026}");
    }

    /// Counting characters rather than bytes is what keeps a multi-byte message from panicking
    /// on a split boundary — `&s[..max]` would.
    #[test]
    fn truncate_chars_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("ñññññ", 2), "ññ\u{2026}");
        assert_eq!(truncate_chars("ñññ", 3), "ñññ");
    }
}
