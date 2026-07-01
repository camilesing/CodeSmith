//! Unicode sanitization for hidden-character attack mitigation.
//!
//! Mirrors Claude Code's `src/utils/sanitization.ts` `partiallySanitizeUnicode`
//! and `recursivelySanitizeUnicode`. Defends against Unicode-based hidden
//! prompt injection — notably the Unicode Tag characters (U+E0000–U+E007F)
//! demonstrated in HackerOne report #3086545, which are invisible to users but
//! are processed by the model. Applied to all MCP tool-call `input` fields
//! (and, via the tool-result scrubbing slice, to tool outputs before they enter
//! the transcript).
//!
//! Reference: <https://embracethered.com/blog/posts/2024/hiding-and-finding-text-with-unicode-tags/>

use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

/// Maximum fixpoint iterations. Mirrors Claude Code's `MAX_ITERATIONS = 10`.
/// Exceeding this is a defense-in-depth signal, not a crash: we log and return
/// the best-effort value.
const SANITIZE_MAX_ITERATIONS: usize = 10;

/// NFKC-normalize and strip dangerous Unicode from a string.
///
/// Each iteration:
/// 1. NFKC normalization (compatibility decomposition + canonical composition).
/// 2. Strip chars whose Unicode general category is `Format` (Cf), `PrivateUse`
///    (Co), or `Unassigned` (Cn) — the primary defense, mirroring the
///    `\p{Cf}\p{Co}\p{Cn}` property classes.
/// 3. Explicit-range fallback (belt-and-suspenders, matches the reference's
///    Step 3): zero-width/direction controls, BOM, BMP + supplementary PUA, and
///    the Unicode Tag block (U+E0000–U+E007F) that is the #3086545 vector.
///
/// Iterates to a fixpoint up to [`SANITIZE_MAX_ITERATIONS`]. If the fixpoint is
/// not reached, logs a warning and returns the current value — never panics on
/// untrusted input.
pub fn partially_sanitize_unicode(input: &str) -> String {
    let mut current = input.to_string();
    let mut previous = String::new();
    let mut iterations = 0;

    while current != previous && iterations < SANITIZE_MAX_ITERATIONS {
        previous = current.clone();

        // Step 1: NFKC normalization.
        let nfkc: String = current.nfkc().collect();

        // Steps 2 + 3: strip dangerous categories and explicit ranges.
        current = nfkc.chars().filter(|c| !is_dangerous_char(*c)).collect();

        iterations += 1;
    }

    if iterations >= SANITIZE_MAX_ITERATIONS && current != previous {
        tracing::warn!(
            input_preview = %input.chars().take(100).collect::<String>(),
            iterations,
            SANITIZE_MAX_ITERATIONS,
            "Unicode sanitization reached max iterations without reaching a fixpoint; \
             returning best-effort result"
        );
    }
    current
}

/// Whether a char is dangerous and should be stripped.
fn is_dangerous_char(c: char) -> bool {
    // Step 2: Unicode general-category classes Cf / Co / Cn.
    let cat = c.general_category();
    if matches!(
        cat,
        GeneralCategory::Format | GeneralCategory::PrivateUse | GeneralCategory::Unassigned
    ) {
        return true;
    }
    // Step 3: explicit ranges — a self-documenting fallback in case the
    // property tables ever diverge, and to make the stripped set auditable.
    match c as u32 {
        0x200B..=0x200F // zero-width space + LTR/RTL marks
        | 0x202A..=0x202E // direction formatting characters
        | 0x2066..=0x2069 // direction isolate characters
        | 0xFEFF // UTF-8 BOM / ZERO WIDTH NO-BREAK SPACE
        | 0xE000..=0xF8FF // BMP private-use area
        | 0xE0000..=0xE007F // Unicode Tag block (the #3086545 vector)
        | 0xF0000..=0xFFFFD // supplementary PUA-A
        | 0x100000..=0x10FFFD // supplementary PUA-B
        => true,
        _ => false,
    }
}

/// Recursively sanitize a JSON value: `Object` keys AND values, `Array`
/// elements, and `String` leaves. Numbers, booleans, and null pass through.
///
/// Mirrors Claude Code's `recursivelySanitizeUnicode`. Applied to all MCP
/// tool-call `input` fields as the last line of defense against steganographic
/// data riding tool arguments.
pub fn recursively_sanitize_unicode(value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(partially_sanitize_unicode(&s)),
        Value::Array(arr) => {
            Value::Array(arr.into_iter().map(recursively_sanitize_unicode).collect())
        }
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                // Sanitize the key too — a Unicode-polluted key name is just as
                // dangerous as a polluted value.
                let key = partially_sanitize_unicode(&k);
                let val = recursively_sanitize_unicode(v);
                out.insert(key, val);
            }
            Value::Object(out)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_zero_width_space() {
        let input = "hello\u{200B}world";
        assert_eq!(partially_sanitize_unicode(input), "helloworld");
    }

    #[test]
    fn strips_bom() {
        let input = "\u{FEFF}hello";
        assert_eq!(partially_sanitize_unicode(input), "hello");
    }

    #[test]
    fn strips_bmp_private_use() {
        let input = "a\u{E000}b";
        assert_eq!(partially_sanitize_unicode(input), "ab");
    }

    #[test]
    fn strips_unicode_tag_character() {
        // U+E0001 LANGUAGE TAG — the HackerOne #3086545 vector.
        let input = "clean\u{E0001}text";
        assert_eq!(partially_sanitize_unicode(input), "cleantext");
    }

    #[test]
    fn strips_supplementary_pua() {
        let input = "x\u{F0000}y";
        assert_eq!(partially_sanitize_unicode(input), "xy");
    }

    #[test]
    fn nfkc_normalizes_compatibility_sequence() {
        // U+FB01 'ﬁ' (LATIN SMALL LIGATURE FI) → NFKC → "fi".
        let input = "ofﬁce";
        assert_eq!(partially_sanitize_unicode(input), "office");
    }

    #[test]
    fn is_idempotent() {
        let input = "hello\u{200B}world\u{FEFF}";
        let once = partially_sanitize_unicode(input);
        let twice = partially_sanitize_unicode(&once);
        assert_eq!(once, twice);
        assert_eq!(once, "helloworld");
    }

    #[test]
    fn preserves_normal_text() {
        let input = "Hello, 世界! 123 \n\t tab";
        assert_eq!(partially_sanitize_unicode(input), input);
    }

    #[test]
    fn does_not_panic_on_pathological_input() {
        // A run of direction-override chars that could in theory keep interacting.
        let input: String = "\u{202E}".repeat(1000);
        let result = partially_sanitize_unicode(&input);
        assert!(
            result.is_empty(),
            "all dangerous chars should be stripped, got {result:?}"
        );
    }

    #[test]
    fn recursively_sanitizes_object_keys_and_values() {
        let input = json!({
            "ke\u{200B}y": "val\u{200B}ue",
            "nested": { "in\u{FEFF}ner": "da\u{E000}ta" },
            "arr": ["a\u{200B}", "b"],
            "num": 42,
            "flag": true,
        });
        let out = recursively_sanitize_unicode(input);
        assert_eq!(
            out,
            json!({
                "key": "value",
                "nested": { "inner": "data" },
                "arr": ["a", "b"],
                "num": 42,
                "flag": true,
            })
        );
    }

    #[test]
    fn recursively_passes_through_scalars() {
        assert_eq!(recursively_sanitize_unicode(json!(42)), json!(42));
        assert_eq!(recursively_sanitize_unicode(json!(true)), json!(true));
        assert_eq!(recursively_sanitize_unicode(json!(null)), json!(null));
    }

    #[test]
    fn recursively_strips_tag_char_in_string_leaf() {
        let input = json!("inject\u{E0001}ion");
        assert_eq!(recursively_sanitize_unicode(input), json!("injection"));
    }
}
