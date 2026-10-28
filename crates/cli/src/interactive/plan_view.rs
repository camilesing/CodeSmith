//! Plan-body rendering for the REPL — a small markdown-to-terminal
//! renderer, used on the plan file at the `exit_plan_mode` approval.
//!
//! Why this exists: the plan-mode handoff once collapsed a fully written
//! plan file into the `update_plan` step list, so the user approved a plan
//! they never saw. This module gives the plan file its own visible surface.
//! It is deliberately *not* a general markdown renderer — streaming
//! assistant text stays verbatim (see the module note in `render.rs`) — it
//! handles exactly what plans contain: ATX headings, paragraphs, bullet and
//! ordered lists, fenced code blocks, horizontal rules, inline
//! `code`/`**bold**`/`*italic*`, and tables (degraded to width-wrapped
//! ` · ` rows, since box-drawing tables cannot survive arbitrary widths).
//!
//! Width: every line is reflowed to the terminal width (or
//! [`DEFAULT_COLUMNS`] when the size is unknown), counted in display columns
//! with `unicode-width`, so CJK text wraps where it should and a wide glyph
//! is never cut in half. Styling is applied *after* wrapping, so ANSI
//! sequences never enter the width math.
//!
//! Known limitations, recorded beside the behaviour they own: blockquotes
//! lose their prefix, nested lists flatten to one level, inline HTML and
//! images have no rendering (they stay literal text), there is no syntax
//! highlighting, and a code-block line wider than the available columns is
//! printed uncut (the terminal wraps it) — code is never reflowed, because
//! its line breaks are content.

use colored::Colorize;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Columns assumed when the terminal size cannot be read.
pub const DEFAULT_COLUMNS: usize = 100;

/// Hard cap on a rendered line's width.
const MAX_COLUMNS: usize = 160;

/// Left indent of the rendered block.
const INDENT: &str = "  ";

/// Marker bullet, and the heading marker.
const BULLET: &str = "•";
const HEADING_MARKER: &str = "▸";

/// Bullets prefer a whitespace break at least this many columns before the
/// right edge; below it, breaking exactly at the edge beats a ragged short
/// line (`- item` rows are usually file paths).
const BULLET_EDGE_TOLERANCE: usize = 4;

/// Render a plan file body for the terminal at the current width.
pub fn render_plan(markdown: &str) -> Vec<String> {
    render_plan_at_width(markdown, columns())
}

/// [`render_plan`] with an explicit width — the testable core.
pub fn render_plan_at_width(markdown: &str, columns: usize) -> Vec<String> {
    render_markdown(markdown, columns.clamp(20, MAX_COLUMNS))
}

/// Test-only: force or release `colored`'s ANSI emission. In a non-TTY test
/// process color is auto-disabled, which would make style assertions vacuous.
#[cfg(test)]
struct ColoredOverride;

#[cfg(test)]
impl ColoredOverride {
    fn forced() -> Self {
        colored::control::set_override(true);
        Self
    }
}

#[cfg(test)]
impl Drop for ColoredOverride {
    fn drop(&mut self) {
        colored::control::unset_override();
    }
}

/// Terminal width in columns, or [`DEFAULT_COLUMNS`] when unknown.
///
/// The renderer runs at an approval prompt (never mid-stream), so one
/// `ioctl`-class query per plan is the whole cost and the width is never
/// stale.
pub fn columns() -> usize {
    crossterm::terminal::size()
        .map(|(cols, _rows)| usize::from(cols).clamp(20, MAX_COLUMNS))
        .unwrap_or(DEFAULT_COLUMNS)
}

// === Inline spans ===

/// Style of one inline run of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InlineKind {
    Plain,
    Bold,
    Code,
    Italic,
}

/// One styled run of text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    text: String,
    kind: InlineKind,
}

impl Span {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: InlineKind::Plain,
        }
    }

    /// Display width in terminal columns (wide glyphs are 2).
    fn width(&self) -> usize {
        self.text.width()
    }
}

/// One wrapped line, as the spans it is made of.
type WrappedLine = Vec<Span>;

// === Blocks ===

/// How a rendered line is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineStyle {
    Heading,
    Paragraph,
    /// A paragraph row that is a bullet's continuation (kept distinct so the
    /// paragraph merge never swallows it).
    ListContinuation,
    Bullet,
    Code,
    Rule,
    Table,
}

/// Flush one wrapped run with `style` on its first row.
fn push_wrapped(
    out: &mut Vec<(LineStyle, WrappedLine)>,
    style: LineStyle,
    lines: Vec<WrappedLine>,
) {
    for (index, line) in lines.into_iter().enumerate() {
        let line_style = if index == 0 {
            style
        } else if style == LineStyle::Bullet {
            LineStyle::ListContinuation
        } else {
            style
        };
        out.push((line_style, line));
    }
}

/// Render markdown into styled, width-bounded terminal lines.
fn render_markdown(markdown: &str, columns: usize) -> Vec<String> {
    render_blocks(markdown, columns)
        .into_iter()
        .map(|(style, line)| paint_line(style, line))
        .collect()
}

/// Walk the markdown, emitting `(style, wrapped line)` rows.
fn render_blocks(markdown: &str, columns: usize) -> Vec<(LineStyle, WrappedLine)> {
    let mut out: Vec<(LineStyle, WrappedLine)> = Vec::new();
    let mut block: Vec<Span> = Vec::new();
    let mut fence: Option<char> = None;
    let mut table: Vec<Vec<String>> = Vec::new();

    // Close the current paragraph run (its soft breaks reflow at the
    // terminal width, not the author's).
    let flush_paragraph = |block: &mut Vec<Span>, out: &mut Vec<(LineStyle, WrappedLine)>| {
        if block.is_empty() {
            return;
        }
        let spans = std::mem::take(block);
        let lines = wrap_spans(
            &spans,
            columns.saturating_sub(INDENT.width()),
            0,
            usize::MAX,
        );
        push_wrapped(out, LineStyle::Paragraph, lines);
    };

    for raw in markdown.lines() {
        // Inside a fence: literal rows until the closing marker.
        if let Some(marker) = fence {
            if is_fence_marker(raw.trim_start(), marker) {
                fence = None;
                continue;
            }
            flush_paragraph(&mut block, &mut out);
            out.push((LineStyle::Code, vec![Span::plain(raw.trim_end())]));
            continue;
        }

        let line = raw.trim_end();
        let trimmed = line.trim_start();

        // A fence opens on ``` or ~~~ (three or more).
        if let Some(marker) = fence_marker(trimmed) {
            flush_paragraph(&mut block, &mut out);
            flush_table(&mut table, &mut out, columns);
            fence = Some(marker);
            continue;
        }

        // Table rows accumulate; any other line ends the table.
        if let Some(cells) = table_cells(line) {
            flush_paragraph(&mut block, &mut out);
            if !is_table_separator_row(&cells) {
                table.push(cells);
            }
            continue;
        }
        flush_table(&mut table, &mut out, columns);

        if trimmed.is_empty() {
            flush_paragraph(&mut block, &mut out);
            continue;
        }

        if let Some(heading) = heading_text(trimmed) {
            flush_paragraph(&mut block, &mut out);
            let spans = parse_inline(&heading);
            // The rendered prefix is the marker plus its space: measure it rather
            // than predicting it, so a marker change cannot silently push
            // headings over the width.
            let avail = columns.saturating_sub(INDENT.width() + HEADING_MARKER.width() + 1);
            push_wrapped(
                &mut out,
                LineStyle::Heading,
                wrap_spans(&spans, avail, 0, 0),
            );
            continue;
        }

        if is_rule(trimmed) {
            flush_paragraph(&mut block, &mut out);
            out.push((LineStyle::Rule, Vec::new()));
            continue;
        }

        if let Some(item) = bullet_content(trimmed) {
            flush_paragraph(&mut block, &mut out);
            let marker_width = BULLET.width() + 1;
            let avail = columns.saturating_sub(INDENT.width() + marker_width);
            let lines = wrap_spans(
                &parse_inline(&item),
                avail,
                0,
                BULLET_EDGE_TOLERANCE * marker_width,
            );
            push_wrapped(&mut out, LineStyle::Bullet, lines);
            continue;
        }

        // Blockquote: keep the text, drop the prefix. Everything else
        // accumulates into the paragraph run.
        let text = trimmed.trim_start_matches('>').trim_start();
        if !block.is_empty() {
            block.push(Span::plain(" "));
        }
        block.extend(parse_inline(text));
    }
    flush_paragraph(&mut block, &mut out);
    flush_table(&mut table, &mut out, columns);
    out
}

/// Flush the pending table as degraded ` · ` rows (the header row bold).
fn flush_table(
    table: &mut Vec<Vec<String>>,
    out: &mut Vec<(LineStyle, WrappedLine)>,
    columns: usize,
) {
    if table.is_empty() {
        return;
    }
    let separator = " · ";
    for (index, cells) in table.drain(..).enumerate() {
        let mut spans: Vec<Span> = Vec::new();
        for (cell_index, cell) in cells.iter().enumerate() {
            if cell_index > 0 {
                spans.push(Span::plain(separator));
            }
            let mut cell_spans = parse_inline(cell);
            if index == 0 {
                for span in &mut cell_spans {
                    span.kind = InlineKind::Bold;
                }
            }
            spans.extend(cell_spans);
        }
        let avail = columns.saturating_sub(INDENT.width());
        push_wrapped(&mut *out, LineStyle::Table, wrap_spans(&spans, avail, 0, 0));
    }
}

/// The opening-fence marker character (``` or ~~~, three or more).
fn fence_marker(line: &str) -> Option<char> {
    for marker in ['`', '~'] {
        let run = line.chars().take_while(|c| *c == marker).count();
        if run >= 3 {
            return Some(marker);
        }
    }
    None
}

/// `true` when the row closes a fence opened with `marker`.
fn is_fence_marker(line: &str, marker: char) -> bool {
    line.chars().take_while(|c| *c == marker).count() >= 3
}

/// The text after the `#` run of an ATX heading.
fn heading_text(line: &str) -> Option<String> {
    let trimmed = line.trim_start_matches('#');
    if trimmed.len() == line.len() {
        return None;
    }
    Some(trimmed.trim().trim_end_matches('#').trim().to_string())
}

/// `true` for `---`, `***`, `___` thematic breaks.
fn is_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3
        && ['-', '*', '_']
            .iter()
            .any(|marker| trimmed.chars().all(|c| c == *marker))
}

/// A table row split into cells, or `None` when the row isn't one.
fn table_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    // A pipe in prose, a heading, or a bullet is not a table. This mirrors
    // the retired TUI renderer, which required the row's leading `|`.
    if !trimmed.starts_with('|') {
        return None;
    }
    let core = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let core = core.strip_suffix('|').unwrap_or(core);
    Some(
        core.split('|')
            .map(|cell| cell.trim().to_string())
            .collect(),
    )
}

/// `true` when the row is a table's `| --- | :-: |` separator.
fn is_table_separator_row(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let core = cell.trim_matches(|c: char| c == ':' || c == '-').trim();
            core.is_empty() && cell.contains('-')
        })
}

/// The content of a `-`/`*`/`+` bullet or an `N.` ordered item.
fn bullet_content(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    for marker in ['-', '*', '+'] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            let text = rest.trim_start();
            return (!text.is_empty()).then(|| text.to_string());
        }
    }
    let digits = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .filter(|idx| *idx > 0)?;
    let (number, rest) = trimmed.split_at(digits);
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    let text = rest.trim_start();
    (!text.is_empty()).then(|| format!("{number}. {text}"))
}

/// Parse inline `code`, `**bold**`, and `*italic*` into spans.
///
/// Deliberately small: unmatched markers stay literal text, which is what a
/// plan wants (`*` in globs, `a ** b` in prose). Code spans win over
/// emphasis.
fn parse_inline(text: &str) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    // Only the run's own leading whitespace is dropped: whitespace beside a
    // marker belongs to the sentence (`a **b** c`, `` `x` y ``).
    let mut rest = text.trim_start();

    while let Some((before, marked, kind, after)) = take_marked(rest) {
        if !before.is_empty() {
            spans.push(Span::plain(before));
        }
        spans.push(Span {
            text: marked.to_string(),
            kind,
        });
        rest = after;
    }
    if !rest.is_empty() {
        spans.push(Span::plain(rest));
    }
    spans
}

/// Find the first marked run in `text`, returning
/// `(before, content, kind, after)`.
///
/// `` `x` `` → code, `**x**` → bold, `*x*` → italic. A marker without a
/// partner yields `None`, and the text stays plain.
fn take_marked(text: &str) -> Option<(&str, &str, InlineKind, &str)> {
    let mut best: Option<(usize, char)> = None;
    for marker in ['`', '*'] {
        if let Some(at) = text.find(marker)
            && best.is_none_or(|(current, _)| at < current)
        {
            best = Some((at, marker));
        }
    }
    let (at, marker) = best?;
    let marker_char = marker;
    let run = text[at..].chars().take_while(|c| *c == marker_char).count();
    let content_start = at + marker_char.len_utf8() * run;
    if content_start >= text.len() {
        return None;
    }
    // The closing run must be at least as long as the opening one and must
    // actually close it: it may not start inside a longer run of the same
    // marker, and must not be preceded by whitespace (so `*.rs globs and a
    // ** b` and `* b` stay literal text instead of becoming emphasis).
    let mut search = content_start;
    let closing = loop {
        let at_marker = text.get(search..)?.find(marker_char)? + search;
        let mut end = at_marker;
        while text
            .get(end..)
            .is_some_and(|tail| tail.starts_with(marker_char))
        {
            end += marker_char.len_utf8();
        }
        let closer_run = end - at_marker;
        let previous = text[content_start..at_marker].graphemes(true).next_back();
        let closes = closer_run >= run && !previous.is_some_and(|g| g.trim().is_empty());
        if closes {
            break at_marker;
        }
        // A run that does not close: step past this whole run and keep
        // looking.
        search = end;
    };
    let content = &text[content_start..closing];
    if content.trim().is_empty() {
        return None;
    }
    let kind = match marker {
        '`' => InlineKind::Code,
        _ if run >= 2 => InlineKind::Bold,
        _ => InlineKind::Italic,
    };
    let after = closing + marker_char.len_utf8() * run;
    Some((&text[..at], content, kind, text.get(after..).unwrap_or("")))
}

/// Wrap one logical line's spans to `avail` display columns.
///
/// Whitespace is the preferred break point; when the **last** whitespace run
/// inside the budget sits within `edge_tolerance` columns of the right edge,
/// the break falls there instead — the next token would have been over-wide
/// anyway, and an exact-edge break beats a ragged short line.
///
/// Graphemes are never split, so combining marks and ZWJ emoji survive.
fn wrap_spans(
    spans: &[Span],
    avail: usize,
    cont_indent: usize,
    edge_tolerance: usize,
) -> Vec<WrappedLine> {
    let avail = avail.max(1);
    let cont_width = avail.saturating_sub(cont_indent).max(1);
    let units: Vec<(InlineKind, &str)> = spans
        .iter()
        .flat_map(|span| {
            span.text
                .graphemes(true)
                .map(move |grapheme| (span.kind, grapheme))
        })
        .collect();

    let mut lines: Vec<WrappedLine> = Vec::new();
    let mut current: WrappedLine = Vec::new();
    let mut index = 0usize;

    while index < units.len() {
        // Leading whitespace never starts a row.
        if current.is_empty() {
            while index < units.len() && units[index].1.trim().is_empty() {
                index += 1;
            }
            if index >= units.len() {
                break;
            }
        }

        let continuation = !lines.is_empty();
        let budget = if continuation { cont_width } else { avail };
        let width = units[index].1.width();
        let used: usize = current.iter().map(|span| span.width()).sum();

        if used + width <= budget {
            let (kind, grapheme) = units[index];
            push_unit(&mut current, (kind, grapheme));
            index += 1;
            continue;
        }
        // The next unit does not fit. On a fresh continuation row nothing
        // can be gained by breaking, so take the unit (progress is
        // guaranteed: `cont_width >= 1` and no grapheme is narrower than 1).
        if current.is_empty() {
            let (kind, grapheme) = units[index];
            push_unit(&mut current, (kind, grapheme));
            index += 1;
            lines.push(std::mem::take(&mut current));
            continue;
        }

        match find_break(
            units[index..].iter().map(|(_, g)| *g),
            budget - used,
            edge_tolerance,
        )
        .filter(|at| *at > 0)
        {
            // Fold before the overflowing unit, at the chosen break.
            Some(at) => {
                let break_at = index + at;
                push_units(&mut current, &units[index..break_at]);
                index = break_at;
                lines.push(std::mem::take(&mut current));
            }
            // The row is full and nothing more fits before the fold: close it
            // and let the overflowing unit open the next one. The unit is not
            // consumed here — the next iteration starts with an empty row,
            // where it fits (`cont_width >= 1`), so progress is preserved and
            // no row ever exceeds its budget.
            None => {
                lines.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// The index (into `units`) at which to fold: the last whitespace run that
/// fits within `left` columns, or the last position that fits when no run
/// does. `None` when not even one unit fits.
fn find_break<'a>(
    units: impl Iterator<Item = &'a str>,
    left: usize,
    edge_tolerance: usize,
) -> Option<usize> {
    let mut last_fit = 0usize;
    let mut whitespace: Option<usize> = None;
    let mut filled = 0usize;
    for (offset, grapheme) in units.enumerate() {
        let width = grapheme.width();
        if filled + width > left {
            break;
        }
        filled += width;
        last_fit = offset + 1;
        if grapheme.chars().all(char::is_whitespace) {
            // Remember the run's first unit: folding drops it entirely.
            whitespace.get_or_insert(offset + 1);
        } else {
            whitespace = None;
        }
    }
    if left > filled && left - filled <= edge_tolerance {
        // The row ended before the edge with a whitespace break close to it
        // (the run reaching the edge itself is not a whitespace break — a
        // word ends there): prefer the whitespace over the exact fit.
        whitespace.or(Some(last_fit))
    } else {
        Some(last_fit)
    }
}

fn push_units(target: &mut WrappedLine, units: &[(InlineKind, &str)]) {
    for (kind, grapheme) in units {
        push_unit(target, (*kind, grapheme));
    }
}

fn push_unit(target: &mut WrappedLine, (kind, grapheme): (InlineKind, &str)) {
    match target.last_mut() {
        Some(last) if last.kind == kind => last.text.push_str(grapheme),
        _ => target.push(Span {
            text: grapheme.to_string(),
            kind,
        }),
    }
}

/// Paint one wrapped line: indent, marker, style.
fn paint_line(style: LineStyle, line: WrappedLine) -> String {
    let body = line
        .iter()
        .map(|span| match span.kind {
            InlineKind::Plain => span.text.clone(),
            InlineKind::Bold => span.text.bold().to_string(),
            InlineKind::Code => span.text.cyan().to_string(),
            InlineKind::Italic => span.text.italic().to_string(),
        })
        .collect::<String>();
    match style {
        LineStyle::Rule => format!("{INDENT}{}", "-".repeat(24).dimmed()),
        LineStyle::Code => format!("{INDENT}{}", body.dimmed()),
        LineStyle::Heading => format!("{INDENT}{} {}", HEADING_MARKER.dimmed(), body.bold()),
        LineStyle::Bullet => format!("{INDENT}{BULLET} {body}"),
        LineStyle::Paragraph | LineStyle::ListContinuation | LineStyle::Table => {
            format!("{INDENT}{body}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line's display width and ANSI-stripped text.
    fn display(line: &str) -> (usize, String) {
        let plain = strip_ansi(line);
        (plain.width(), plain)
    }

    fn strip_ansi(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn no_rendered_line_exceeds_the_width() {
        let plan = "# 接口列表页合并「开发中 / 已上线」为单页 + 状态筛选\n\n\
                    ## Context\n\n\
                    `https://best-data-api-ui.bestfulfill-inc.com/index.html#/interface/list`（宿主 UI \
                    datapoly-manager-ui/src/views/interface/list.vue）目前用 el-radio-group 把接口按 online \
                    布尔切成两个互斥视图：「开发中」(false) 与「已上线」(true)。\n\n\
                    - datapoly-manager-ui/src/views/interface/list.vue — 状态筛选替换单选组，删除 el-radio-group（line 6-11），换成 el-select\n\
                    - i18n 词条：`zh-CN.js` 与 `en-US.js` 各加两键\n";
        for line in render_plan_at_width(plan, 60) {
            let (width, plain) = display(&line);
            assert!(width <= 60, "line exceeds 60 columns ({width}): {plain:?}");
        }
    }

    #[test]
    fn cjk_wraps_between_wide_glyphs_never_through_them() {
        let text = format!("# {}", "宽字符排版验证".repeat(12));
        let rendered = render_plan_at_width(&text, 24);
        assert!(rendered.len() > 1, "CJK run must wrap: {rendered:?}");
        for line in &rendered {
            let (width, plain) = display(line);
            assert!(width <= 24, "over budget ({width}): {plain:?}");
            assert!(
                plain.chars().all(|c| c != '\u{fffd}'),
                "wide glyph split: {plain:?}"
            );
        }
    }

    #[test]
    fn table_rows_degrade_to_dot_rows_with_bold_header() {
        let _colors = ColoredOverride::forced();
        let table = "| 现状 | 目标 |\n| --- | --- |\n| 两个 tab | 一页 + 筛选 |\n";
        let rendered = render_plan_at_width(table, 60);
        let plain: Vec<String> = rendered.iter().map(|l| display(l).1).collect();
        assert_eq!(plain.len(), 2, "separator row is dropped: {plain:?}");
        assert!(plain[0].contains("现状"), "{plain:?}");
        assert!(
            plain[0].contains(" · "),
            "cells join with a separator: {plain:?}"
        );
        assert!(
            rendered[0].contains("\u{1b}["),
            "header is styled: {rendered:?}"
        );
    }

    #[test]
    fn code_fence_is_literal_and_not_reflowed() {
        let fence =
            "```sh\ncargo test -p codesmith-cli -- --nocapture a_somewhat_long_filter_name\n```\n";
        let rendered = render_plan_at_width(fence, 40);
        let plain: Vec<String> = rendered.iter().map(|l| display(l).1).collect();
        assert_eq!(plain.len(), 1, "fence body is one row: {plain:?}");
        assert!(
            plain[0].contains("a_somewhat_long_filter_name"),
            "code line stays whole: {plain:?}"
        );
    }

    #[test]
    fn bullet_continuation_hangs_under_the_marker() {
        let bullet = "- datapoly-manager-ui/src/views/interface/list.vue 状态筛选替换单选组并删除 el-radio-group 的 v-if 守卫\n";
        let rendered = render_plan_at_width(bullet, 40);
        assert!(rendered.len() > 1, "long bullet wraps: {rendered:?}");
        let (_, first) = display(&rendered[0]);
        let (_, second) = display(&rendered[1]);
        assert!(
            first.contains('•'),
            "first line carries the bullet: {first:?}"
        );
        assert!(
            !second.contains('•'),
            "continuation has no bullet: {second:?}"
        );
        assert!(
            second.starts_with("  "),
            "continuation keeps the block indent: {second:?}"
        );
    }

    #[test]
    fn inline_code_and_bold_are_styled_without_markers() {
        let _colors = ColoredOverride::forced();
        let row = "把 `online` 改成 **三态筛选**（`null` 全量）";
        let rendered = render_plan_at_width(row, 80);
        let plain = display(&rendered[0]).1;
        assert!(!plain.contains('`'), "backticks are consumed: {plain:?}");
        assert!(plain.contains("三态筛选"), "bold text survives: {plain:?}");
        assert!(
            !plain.contains("**"),
            "bold markers are consumed: {plain:?}"
        );
        assert!(rendered[0].contains("\u{1b}["), "styling applied");
    }

    #[test]
    fn unmatched_markers_stay_literal() {
        let row = "use *.rs globs and a ** b as-is";
        let rendered = render_plan_at_width(row, 80);
        let plain = display(&rendered[0]).1;
        assert!(plain.contains("*.rs"), "glob survives: {plain:?}");
        assert!(
            plain.contains("a ** b"),
            "unmatched bold survives: {plain:?}"
        );
    }

    #[test]
    fn headings_render_as_marked_bold_lines_without_hashes() {
        let rendered = render_plan_at_width("## Context\n", 80);
        let plain = display(&rendered[0]).1;
        assert!(!plain.contains('#'), "hash marker is replaced: {plain:?}");
        assert!(plain.contains("Context"), "{plain:?}");
    }

    #[test]
    fn paragraph_soft_breaks_reflow_within_the_width() {
        let paragraph = "第一行文字\n第二行文字\n第三行文字\n";
        let rendered = render_plan_at_width(paragraph, 80);
        assert_eq!(rendered.len(), 1, "soft breaks join into one paragraph");
        assert!(display(&rendered[0]).1.contains("第一行文字 第二行文字"));
    }

    #[test]
    fn spaces_around_inline_markers_survive() {
        for (row, expected) in [
            ("a **b** c", "a b c"),
            ("run `cargo test` now", "run cargo test now"),
            (
                "把 `online` 改成 **三态筛选**（`null` 全量）",
                "把 online 改成 三态筛选（null 全量）",
            ),
            ("- 用 `foo` 和 `bar` 两个键", "• 用 foo 和 bar 两个键"),
        ] {
            let rendered = render_plan_at_width(row, 200);
            let plain: Vec<String> = rendered.iter().map(|l| display(l).1).collect();
            assert!(
                plain.iter().any(|line| line.contains(expected)),
                "expected {expected:?} in {plain:?}"
            );
        }
    }

    #[test]
    fn pipes_outside_a_table_are_not_a_table() {
        let prose = render_plan_at_width("正文里用 a | b 表示或", 200);
        let line = display(&prose[0]).1;
        assert!(line.contains("a | b"), "prose keeps its pipe: {line:?}");

        let bullet = render_plan_at_width("- cat foo | grep bar", 200);
        let line = display(&bullet[0]).1;
        assert!(line.contains('•'), "bullet keeps its marker: {line:?}");
        assert!(line.contains("cat foo | grep bar"), "{line:?}");

        let heading = render_plan_at_width("## a | b", 200);
        let line = display(&heading[0]).1;
        assert!(!line.contains('#'), "heading is rendered: {line:?}");
        assert!(line.contains("a | b"), "{line:?}");
    }

    #[test]
    fn ordered_items_keep_their_number() {
        let rendered = render_plan_at_width("1. 先改 vue\n2. 再补 i18n\n", 80);
        assert_eq!(rendered.len(), 2);
        let first = display(&rendered[0]).1;
        assert!(first.contains("1."), "{first:?}");
    }
}
