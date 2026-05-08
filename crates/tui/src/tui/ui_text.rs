//! Shared text helpers for TUI selection and clipboard workflows.

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::tui::history::HistoryCell;
use crate::tui::osc8;

/// Tool-card left-rail decoration glyphs emitted by
/// `crate::tui::transcript::line_with_group_rail` as the leading span of a
/// rendered tool-card line. They are visual-only and must not leak into
/// copied text (#1163).
const TOOL_CARD_RAIL_PREFIXES: &[&str] = &["\u{256D} ", "\u{2502} ", "\u{2570} "];
/// Display width of any rail prefix (one box-drawing glyph + one space).
const TOOL_CARD_RAIL_PREFIX_WIDTH: usize = 2;

pub(super) fn history_cell_to_text(cell: &HistoryCell, width: u16) -> String {
    cell.transcript_lines(width)
        .into_iter()
        .map(line_to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_to_string(line: Line<'static>) -> String {
    let mut out = String::new();
    append_spans_plain(line.spans.iter(), &mut out);
    out
}

/// Strip a leading tool-card rail glyph span (`╭ `, `│ `, `╰ `) and OSC-8
/// link wrappers from a rendered transcript line so the decoration does
/// not leak into copied text. Returns `(plain_text, rail_prefix_width)`
/// where `rail_prefix_width` is `0` for non-tool-card lines and `2` when
/// the rail prefix was stripped. Callers subtract `rail_prefix_width`
/// from the recorded selection columns to keep the visible selection
/// rect aligned with the returned plain text.
pub(super) fn line_to_plain_for_copy(line: &Line<'static>) -> (String, usize) {
    let mut spans = line.spans.iter();
    if let Some(first) = line.spans.first()
        && TOOL_CARD_RAIL_PREFIXES.contains(&first.content.as_ref())
    {
        spans.next();
        let mut out = String::new();
        append_spans_plain(spans, &mut out);
        return (out, TOOL_CARD_RAIL_PREFIX_WIDTH);
    }
    let mut out = String::new();
    append_spans_plain(spans, &mut out);
    (out, 0)
}

fn append_spans_plain<'a, I>(spans: I, out: &mut String)
where
    I: Iterator<Item = &'a Span<'a>>,
{
    for span in spans {
        if span.content.contains('\x1b') {
            osc8::strip_into(&span.content, out);
        } else {
            out.push_str(span.content.as_ref());
        }
    }
}

pub(super) fn text_display_width(text: &str) -> usize {
    text.chars().map(char_display_width).sum()
}

pub(super) fn slice_text(text: &str, start: usize, end: usize) -> String {
    if end <= start {
        return String::new();
    }

    let mut out = String::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let ch_width = char_display_width(ch);
        let ch_start = col;
        let ch_end = col.saturating_add(ch_width);
        if ch_end > start && ch_start < end {
            out.push(ch);
        }
        col = ch_end;
        if col >= end {
            break;
        }
    }
    out
}

fn char_display_width(ch: char) -> usize {
    if ch == '\t' {
        4
    } else {
        UnicodeWidthChar::width(ch).unwrap_or(0).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    #[test]
    fn line_to_plain_for_copy_strips_osc_8_wrapper() {
        // A span carrying an OSC 8-wrapped URL must not leak the escape into
        // selection / clipboard output. The visible label survives.
        let wrapped = format!(
            "\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\",
            "https://example.com", "https://example.com"
        );
        let line = Line::from(vec![
            Span::raw("see "),
            Span::raw(wrapped),
            Span::raw(" for details"),
        ]);
        let (text, rail_width) = line_to_plain_for_copy(&line);
        assert_eq!(text, "see https://example.com for details");
        assert_eq!(rail_width, 0);
    }

    #[test]
    fn line_to_plain_for_copy_passes_through_plain_spans() {
        let line = Line::from(vec![Span::raw("plain "), Span::raw("text")]);
        let (text, rail_width) = line_to_plain_for_copy(&line);
        assert_eq!(text, "plain text");
        assert_eq!(rail_width, 0);
    }

    #[test]
    fn line_to_plain_for_copy_strips_tool_card_rail_prefix() {
        let line = Line::from(vec![Span::raw("\u{2502} "), Span::raw("body content")]);
        let (text, rail_width) = line_to_plain_for_copy(&line);
        assert_eq!(text, "body content");
        assert_eq!(rail_width, 2);
    }

    #[test]
    fn line_to_plain_for_copy_strips_top_and_bottom_rails() {
        for glyph in ["\u{256D} ", "\u{2570} "] {
            let line = Line::from(vec![Span::raw(glyph), Span::raw("x")]);
            let (text, rail_width) = line_to_plain_for_copy(&line);
            assert_eq!(text, "x");
            assert_eq!(rail_width, 2);
        }
    }

    #[test]
    fn line_to_plain_for_copy_keeps_user_typed_pipe_when_no_rail() {
        // A normal line whose plain text starts with `│ literal` (single
        // span, not a rail prefix span) must round-trip verbatim.
        let line = Line::from(vec![Span::raw("\u{2502} literal pipe at start")]);
        let (text, rail_width) = line_to_plain_for_copy(&line);
        assert_eq!(text, "\u{2502} literal pipe at start");
        assert_eq!(rail_width, 0);
    }
}
