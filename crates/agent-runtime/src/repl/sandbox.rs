//! REPL fence-extraction utilities.
//!
//! The agent's main loop scans assistant text for ` ```repl ` fenced blocks
//! and feeds them to a [`crate::repl::runtime::PythonRuntime`]. Capturing
//! `FINAL(...)` and routing sub-LLM RPCs are handled inside the runtime via
//! a stdin/stdout protocol — no scraping required here.

/// Check if a string contains a *complete* `` ```repl `` fenced code block.
pub fn has_repl_block(text: &str) -> bool {
    !extract_repl_blocks(text).is_empty()
}

/// Extract every `` ```repl `` block from `text` with byte offsets.
pub fn extract_repl_blocks(text: &str) -> Vec<ReplBlock> {
    let mut blocks = Vec::new();
    let mut rest = text;

    while let Some(start_idx) = rest.find("```repl") {
        let after_marker = &rest[start_idx + "```repl".len()..];
        if !after_marker.is_empty() && !after_marker.starts_with(['\n', '\r', ' ', '\t']) {
            // Not a repl fence (e.g. ```replXYZ) — skip past this occurrence.
            rest = after_marker;
            continue;
        }
        // CommonMark fence matching: the closing fence must be at least as
        // long as the opening one, so a longer opening fence (````repl)
        // keeps shorter fenced runs inside the code intact.
        let mut fence_start = start_idx;
        while fence_start > 0 && rest.as_bytes()[fence_start - 1] == b'`' {
            fence_start -= 1;
        }
        let fence_len = start_idx + 3 - fence_start;

        let after_fence = &rest[start_idx..];
        let code_start = after_fence.find('\n').unwrap_or(after_fence.len());
        let code_region = &after_fence[code_start..];
        let Some((closing_start, closing_len)) = find_closing_fence(code_region, fence_len) else {
            break;
        };
        // The `"\n"` before the closing fence line terminates the code,
        // matching the offsets the `"\n```"` scan used to produce.
        let end_offset = closing_start - 1;
        let code = code_region[..end_offset].to_string();
        let global_start = text.len() - rest.len() + fence_start;
        // One PAST the closing fence — matches how `rest` advances below.
        // The old +3 landed ON the final backtick, leaving a stray '`' in
        // any downstream slice/splice built on these offsets.
        let global_end =
            global_start + (start_idx - fence_start) + code_start + end_offset + 1 + closing_len;
        blocks.push(ReplBlock {
            code,
            start_offset: global_start,
            end_offset: global_end,
        });
        rest = &after_fence[code_start + closing_start + closing_len..];
    }

    blocks
}

/// Find the closing fence for a block opened with `fence_len` backticks:
/// the first line starting with at least `fence_len` backticks. Returns
/// the byte offset of the line's first backtick (within `region`) and the
/// length of the backtick run.
fn find_closing_fence(region: &str, fence_len: usize) -> Option<(usize, usize)> {
    let mut offset = 0;
    for line in region.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let backticks = line.len() - line.trim_start_matches('`').len();
        if backticks >= fence_len {
            return Some((line_start, backticks));
        }
    }
    None
}

/// A `` ```repl `` code block with byte-offset position info.
#[derive(Debug, Clone)]
pub struct ReplBlock {
    pub code: String,
    pub start_offset: usize,
    pub end_offset: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_repl_block_detects_fence() {
        assert!(has_repl_block("some text ```repl\ncode\n``` more"));
        assert!(!has_repl_block("no repl here ```python\ncode\n```"));
        assert!(!has_repl_block("just text"));
    }

    #[test]
    fn has_repl_block_requires_complete_block() {
        // A still-streaming opening fence is NOT a block yet — has_repl_block
        // must agree with extract_repl_blocks instead of answering early.
        assert!(!has_repl_block("```repl\npartial"));
        assert!(!has_repl_block("```repl"));
    }

    #[test]
    fn loose_repl_fence_prefix_is_rejected() {
        // Only ```repl at end of line / followed by whitespace is a repl
        // fence; ```replXYZ is a different info string.
        assert!(!has_repl_block("```replXYZ\ncode\n```"));
        assert!(!has_repl_block("```reply now"));
        assert!(has_repl_block("```repl\ncode\n```"));
        assert!(extract_repl_blocks("```replXYZ\ncode\n```").is_empty());
    }

    #[test]
    fn extract_repl_blocks_single() {
        let text = "before\n```repl\nprint('hello')\n```\nafter";
        let blocks = extract_repl_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code.trim(), "print('hello')");
    }

    #[test]
    fn extract_repl_blocks_multiple() {
        let text = "```repl\ncode1\n```\nmid\n```repl\ncode2\n```\nend";
        let blocks = extract_repl_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].code.trim(), "code1");
        assert_eq!(blocks[1].code.trim(), "code2");
    }

    #[test]
    fn extract_repl_blocks_empty_when_none() {
        let blocks = extract_repl_blocks("no blocks here");
        assert!(blocks.is_empty());
    }

    #[test]
    fn longer_opening_fence_shields_inner_short_fences() {
        // CommonMark: a ````repl block only closes at a >=4-backtick line,
        // so a Python triple-quoted string containing ``` fences survives.
        // (Like the legacy scan, `code` keeps the newline that ended the
        // opening fence line.)
        let text = "````repl\nmd = \"\"\"\n```python\n\"\"\"\n````\nafter";
        let blocks = extract_repl_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code, "\nmd = \"\"\"\n```python\n\"\"\"");
        assert_eq!(blocks[0].start_offset, 0);
    }

    #[test]
    fn inner_fence_terminates_three_backtick_block() {
        // With a 3-backtick opening fence an inner ``` line IS the close
        // (same ambiguity CommonMark has) — pin the truncation behavior.
        let blocks = extract_repl_blocks("```repl\ns = '''\n```\nrest");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code, "\ns = '''");
    }
}
