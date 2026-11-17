//! Surgical `config.toml` edits: set or unset one dotted path in a
//! [`toml_edit::DocumentMut`] without disturbing comments, formatting, or
//! unknown keys. Persist with [`crate::write_config_file_private`] (atomic
//! tmp+fsync+rename, 0600) — never through `ConfigStore::save`, which
//! round-trips the typed `ConfigToml` and drops comments and extras.
//!
//! Known limitations: no array-index path segments (lists are replaced
//! whole); a set through an inline-table intermediate is refused — only
//! real tables are descended into.

use toml_edit::{DocumentMut, Item, Table, Value};

/// Split a dotted path, rejecting the shapes this module cannot address:
/// empty segments and array indexing (`a[0]`).
fn split_path(path: &str) -> Result<Vec<&str>, String> {
    if path.is_empty() {
        return Err("empty config path".to_string());
    }
    let mut segments = Vec::new();
    for segment in path.split('.') {
        if segment.is_empty() {
            return Err(format!("empty segment in config path {path:?}"));
        }
        if segment.contains('[') || segment.contains(']') {
            return Err(format!("array indexing is not supported: {path:?}"));
        }
        segments.push(segment);
    }
    Ok(segments)
}

/// Convert a parsed `toml::Value` into its `toml_edit` counterpart so a
/// caller that already holds `toml::Value` (e.g. deserialized from an API
/// payload) can set it without a second parse.
fn to_edit_value(value: toml::Value) -> Value {
    match value {
        toml::Value::String(s) => s.into(),
        toml::Value::Integer(i) => i.into(),
        toml::Value::Float(f) => f.into(),
        toml::Value::Boolean(b) => b.into(),
        toml::Value::Datetime(d) => d.into(),
        toml::Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(to_edit_value(item));
            }
            array.into()
        }
        toml::Value::Table(table) => {
            let mut inline = toml_edit::InlineTable::new();
            for (key, item) in table {
                inline.insert(&key, to_edit_value(item));
            }
            inline.into()
        }
    }
}

/// Set `path` (dotted) to `value`, creating intermediate tables as needed.
pub fn doc_set(doc: &mut DocumentMut, path: &str, value: toml::Value) -> Result<(), String> {
    let segments = split_path(path)?;
    let mut table = doc.as_table_mut();
    for (index, segment) in segments.iter().enumerate() {
        if index + 1 == segments.len() {
            table.insert(segment, Item::Value(to_edit_value(value)));
            return Ok(());
        }
        let entry = table
            .entry(segment)
            .or_insert_with(|| Item::Table(Table::new()));
        match entry {
            Item::Table(nested) => table = nested,
            Item::Value(Value::InlineTable(_)) => {
                return Err(format!(
                    "config path {path:?}: segment {segment:?} is an inline value, not a table"
                ));
            }
            other => {
                return Err(format!(
                    "config path {path:?}: segment {segment:?} is a {}, not a table",
                    other.type_name()
                ));
            }
        }
    }
    unreachable!("split_path rejects empty segment lists")
}

/// Remove the leaf at `path`. Returns `Ok(false)` when the key is absent.
/// Tables left empty by the removal are collapsed to implicit so no stray
/// `[section]` headers linger in the rendered document.
pub fn doc_unset(doc: &mut DocumentMut, path: &str) -> Result<bool, String> {
    let segments = split_path(path)?;
    let mut table = doc.as_table_mut();
    for (index, segment) in segments.iter().enumerate() {
        if index + 1 == segments.len() {
            let removed = table.remove(segment).is_some();
            if removed {
                collapse_emptied_tables(doc, &segments[..segments.len() - 1]);
            }
            return Ok(removed);
        }
        match table.get_mut(segment) {
            Some(Item::Table(nested)) => table = nested,
            Some(Item::Value(Value::InlineTable(inline))) => {
                if index + 2 == segments.len() {
                    return Ok(inline.remove(segments[index + 1]).is_some());
                }
                return Ok(false);
            }
            _ => return Ok(false),
        }
    }
    unreachable!("split_path rejects empty segment lists")
}

/// After removing a leaf, walk back up the path marking tables that became
/// key-less as implicit — toml_edit then omits their headers on render.
fn collapse_emptied_tables(doc: &mut DocumentMut, parents: &[&str]) {
    let mut table = doc.as_table_mut();
    for (index, segment) in parents.iter().enumerate() {
        let Some(entry) = table.get_mut(segment) else {
            return;
        };
        let Item::Table(nested) = entry else {
            return;
        };
        if index + 1 == parents.len() {
            if nested.is_empty() {
                nested.set_implicit(true);
            }
            return;
        }
        table = nested;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write_config_file_private;

    fn doc(source: &str) -> DocumentMut {
        source.parse::<DocumentMut>().expect("test fixture parses")
    }

    #[test]
    fn set_preserves_comments_and_unknown_tables() {
        let mut doc = doc("# leading comment\nmodel = \"deepseek-chat\"\n\n[mystery]\nkey = 1\n");
        doc_set(
            &mut doc,
            "sandbox.filesystem.mode",
            "workspace-write".into(),
        )
        .expect("set succeeds");
        let rendered = doc.to_string();
        assert!(rendered.contains("# leading comment"));
        assert!(rendered.contains("[mystery]"));
        assert!(rendered.contains("mode = \"workspace-write\""));
        // Round-trips through the typed schema: unknown table rides extras.
        let parsed: crate::ConfigToml = toml::from_str(&rendered).expect("round-trip parses");
        assert_eq!(parsed.model.as_deref(), Some("deepseek-chat"));
    }

    #[test]
    fn set_then_unset_leaves_no_trace() {
        let source = "# tools comment\n[tools]\nalways_load = [\"read\"]\n";
        let mut doc = doc(source);
        doc_set(&mut doc, "tools.progressive_disclosure", true.into()).expect("set succeeds");
        assert!(doc_unset(&mut doc, "tools.progressive_disclosure").expect("unset succeeds"));
        assert!(!doc_unset(&mut doc, "tools.progressive_disclosure").expect("second unset"));
        let rendered = doc.to_string();
        assert!(rendered.contains("# tools comment"));
        assert!(rendered.contains("always_load"));
        assert!(!rendered.contains("progressive_disclosure"));
    }

    #[test]
    fn unset_collapses_emptied_table_headers() {
        let mut doc = doc("[sandbox]\nfilesystem = { mode = \"read-only\" }\n");
        // Dotted set materializes a real table; removing its only leaf must
        // not leave an empty `[sandbox.filesystem]` header behind.
        doc_set(&mut doc, "sandbox.network.enabled", true.into()).expect("set succeeds");
        assert!(doc_unset(&mut doc, "sandbox.network.enabled").expect("unset succeeds"));
        assert!(!doc.to_string().contains("[sandbox.network]"));
    }

    #[test]
    fn rejects_unaddressable_paths() {
        let mut doc = doc("model = \"x\"\n");
        assert!(doc_set(&mut doc, "", "y".into()).is_err());
        assert!(doc_set(&mut doc, "a[0]", "y".into()).is_err());
        assert!(doc_set(&mut doc, "a..b", "y".into()).is_err());
        // Scalar intermediate: cannot descend into `model`.
        assert!(doc_set(&mut doc, "model.nested", "y".into()).is_err());
        assert!(doc_unset(&mut doc, "").is_err());
    }

    #[test]
    fn set_accepts_composite_values() {
        let mut doc = doc("");
        let mut headers = toml::Table::new();
        headers.insert("x-header".to_string(), toml::Value::String("v".into()));
        doc_set(&mut doc, "http_headers", headers.into()).expect("map set succeeds");
        doc_set(
            &mut doc,
            "tools.always_load",
            toml::Value::Array(vec!["a".into(), "b".into()]),
        )
        .expect("list set succeeds");
        let parsed: crate::ConfigToml = toml::from_str(&doc.to_string()).expect("parses");
        assert_eq!(
            parsed.http_headers.get("x-header").map(String::as_str),
            Some("v")
        );
        assert_eq!(parsed.tools.expect("tools").always_load, vec!["a", "b"]);
    }

    #[cfg(unix)]
    #[test]
    fn write_via_private_helper_is_atomic_and_private() {
        let mut doc = doc("# keep me\nmodel = \"m\"\n");
        doc_set(&mut doc, "log_level", "debug".into()).expect("set succeeds");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        write_config_file_private(&path, &doc.to_string()).expect("write succeeds");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("# keep me"));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
