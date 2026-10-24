//! Network policy store edits (moved from the retired
//! `commands/network.rs`): allow/deny host lists + the default policy.
//!
//! Edits round-trip `config.toml` through `toml_edit` so user comments,
//! formatting, and key order survive, and each load→mutate→save cycle runs
//! under the runtime's fd-lock helper so two concurrent CLI sessions
//! serialize instead of silently dropping each other's edits (last writer
//! no longer wins whole-file). The lock is shared with the other
//! config.toml writers — `persist_root_string_key`, workspace trust, and
//! the first-run wizard's batched persist all route through the same
//! helper. Known limitation: the whole-file writers (trust, wizard,
//! root-key persists) serialize correctly but still drop comments and
//! re-sort keys on THEIR edits; only this store is format-preserving.

use std::fs;
use std::path::Path;

use anyhow::{Context, bail};
use toml_edit::{DocumentMut, Item, Table, Value as TomlValue};

use codesmith_agent_runtime::network_policy::{host_from_url, normalize_host};
// Generic over any path despite the name: same fd-lock + WouldBlock-retry
// pattern this store needs, derived from a sibling `<file>.toml.lock`.
use codesmith_agent_runtime::settings_file::with_settings_lock;

pub(crate) fn network_inner(arg: Option<&str>) -> anyhow::Result<String> {
    let raw = arg.map(str::trim).unwrap_or("");
    if raw.is_empty() || raw.eq_ignore_ascii_case("list") {
        return list_policy();
    }

    let mut parts = raw.split_whitespace();
    let Some(command) = parts.next() else {
        return list_policy();
    };
    let command = command.to_ascii_lowercase();

    match command.as_str() {
        "allow" | "deny" | "remove" | "forget" => {
            let Some(host_arg) = parts.next() else {
                bail!("Usage: /network {command} <host>");
            };
            if parts.next().is_some() {
                bail!("Usage: /network {command} <host>");
            }
            let host = normalize_host_arg(host_arg)?;
            let edit = match command.as_str() {
                "allow" => NetworkEdit::Allow,
                "deny" => NetworkEdit::Deny,
                _ => NetworkEdit::Remove,
            };
            update_host(edit, &host)
        }
        "default" => {
            let Some(value) = parts.next() else {
                bail!("Usage: /network default <allow|deny|prompt>");
            };
            if parts.next().is_some() {
                bail!("Usage: /network default <allow|deny|prompt>");
            }
            update_default(value)
        }
        _ => bail!(usage()),
    }
}

fn usage() -> &'static str {
    "Usage: /network [list|allow <host>|deny <host>|remove <host>|default <allow|deny|prompt>]"
}

#[derive(Clone, Copy)]
enum NetworkEdit {
    Allow,
    Deny,
    Remove,
}

fn list_policy() -> anyhow::Result<String> {
    let path = super::auto_route::config_toml_path(None)?;
    let doc = load_config_doc(&path)?;
    let network = doc.get("network").and_then(Item::as_table);
    let default = network
        .and_then(|table| table.get("default"))
        .and_then(Item::as_str)
        .unwrap_or("prompt");
    let allow = network
        .map(|table| string_array(table, "allow"))
        .unwrap_or_default();
    let deny = network
        .map(|table| string_array(table, "deny"))
        .unwrap_or_default();

    Ok(format!(
        "Network policy ({})\n\
         default = {default}\n\
         allow = {}\n\
         deny = {}\n\n\
         Use `/network allow <host>` to allow a host, `/network deny <host>` to block it, or `/network remove <host>` to clear an entry.",
        path.display(),
        display_list(&allow),
        display_list(&deny)
    ))
}

fn update_host(edit: NetworkEdit, host: &str) -> anyhow::Result<String> {
    let path = super::auto_route::config_toml_path(None)?;
    with_settings_lock(&path, || {
        let mut doc = load_config_doc(&path)?;
        let network = network_table_mut(&mut doc)?;

        match edit {
            NetworkEdit::Allow => {
                remove_host(network, "deny", host)?;
                add_host(network, "allow", host)?;
            }
            NetworkEdit::Deny => {
                remove_host(network, "allow", host)?;
                add_host(network, "deny", host)?;
            }
            NetworkEdit::Remove => {
                remove_host(network, "allow", host)?;
                remove_host(network, "deny", host)?;
            }
        }

        save_config_doc(&path, &doc)?;
        let action = match edit {
            NetworkEdit::Allow => "allowed",
            NetworkEdit::Deny => "denied",
            NetworkEdit::Remove => "removed",
        };
        Ok(format!(
            "Network host {action}: {host}\nSaved to {}. Retry the command now.",
            path.display()
        ))
    })
}

fn update_default(value: &str) -> anyhow::Result<String> {
    let normalized = match value.trim().to_ascii_lowercase().as_str() {
        "allow" => "allow",
        "deny" | "block" => "deny",
        "prompt" | "ask" => "prompt",
        _ => bail!("Usage: /network default <allow|deny|prompt>"),
    };

    let path = super::auto_route::config_toml_path(None)?;
    with_settings_lock(&path, || {
        let mut doc = load_config_doc(&path)?;
        let network = network_table_mut(&mut doc)?;
        network.insert("default", toml_edit::value(normalized));
        save_config_doc(&path, &doc)?;

        Ok(format!(
            "Network default set to {normalized}\nSaved to {}.",
            path.display()
        ))
    })
}

fn load_config_doc(path: &Path) -> anyhow::Result<DocumentMut> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;
    raw.parse::<DocumentMut>()
        .with_context(|| format!("failed to parse config at {}", path.display()))
}

fn save_config_doc(path: &Path, doc: &DocumentMut) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }
    codesmith_agent_runtime::utils::write_atomic(path, doc.to_string().as_bytes())
        .with_context(|| format!("failed to write config at {}", path.display()))
}

fn network_table_mut(doc: &mut DocumentMut) -> anyhow::Result<&mut Table> {
    let entry = doc
        .entry("network")
        .or_insert_with(|| Item::Table(Table::new()));
    // No default/audit keys are inserted here: the runtime's deserialization
    // applies those defaults when the keys are absent, and writing them
    // unconditionally mutated keys the user never asked to change on every
    // `/network allow <host>` edit.
    entry
        .as_table_mut()
        .context("`network` section in config.toml must be a table")
}

fn string_array(table: &Table, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(Item::as_array)
        .into_iter()
        .flatten()
        .filter_map(TomlValue::as_str)
        .map(ToString::to_string)
        .collect()
}

fn string_array_mut<'a>(
    table: &'a mut Table,
    key: &str,
) -> anyhow::Result<&'a mut toml_edit::Array> {
    let value = table
        .entry(key)
        .or_insert_with(|| Item::Value(TomlValue::Array(toml_edit::Array::new())));
    value
        .as_array_mut()
        .with_context(|| format!("`network.{key}` must be an array of strings"))
}

fn add_host(table: &mut Table, key: &str, host: &str) -> anyhow::Result<()> {
    let list = string_array_mut(table, key)?;
    if !list
        .iter()
        .filter_map(TomlValue::as_str)
        .any(|existing| normalize_host(existing) == host)
    {
        list.push(host.to_string());
    }
    Ok(())
}

fn remove_host(table: &mut Table, key: &str, host: &str) -> anyhow::Result<()> {
    let list = string_array_mut(table, key)?;
    list.retain(|value| {
        value
            .as_str()
            .is_none_or(|existing| normalize_host(existing) != host)
    });
    Ok(())
}

fn normalize_host_arg(input: &str) -> anyhow::Result<String> {
    let trimmed = input.trim();
    // Scheme detection is case-insensitive (RFC 3986 schemes): `HTTPS://…`
    // used to fall through to the "not a URL path" bail even though the
    // URL parser behind it accepts uppercase schemes.
    let lower = trimmed.to_ascii_lowercase();
    let host = if lower.starts_with("http://") || lower.starts_with("https://") {
        host_from_url(trimmed).context("URL must include a host")?
    } else {
        if trimmed.contains("://") || trimmed.contains('/') {
            bail!("Pass a host like `github.com`, not a URL path");
        }
        trimmed.to_string()
    };

    let normalized = normalize_host(&host);
    if normalized.is_empty() {
        bail!("host cannot be empty");
    }
    Ok(normalized)
}

fn display_list(values: &[String]) -> String {
    if values.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", values.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codesmith_agent_runtime::test_support::{EnvVarGuard, lock_test_env};

    /// Point `CODESMITH_CONFIG_PATH` at a temp file seeded with `raw` and
    /// return the guard (keep it alive for the whole test — dropping it
    /// restores the env var), the temp dir, and the file's path. The env
    /// lock serializes the mutation against every other env-mutating test.
    /// Without the redirect every edit lands in the user's real
    /// `~/.codesmith/config.toml`.
    fn scoped_config(raw: &str) -> (EnvVarGuard, tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, raw).expect("seed config");
        let guard = EnvVarGuard::set("CODESMITH_CONFIG_PATH", &path);
        (guard, dir, path)
    }

    #[test]
    fn edits_preserve_comments_and_key_order() {
        let _env = lock_test_env();
        let seed = "# user-managed config\n\
                    theme = \"dark\" # keep my theme\n\n\
                    [network]\n\
                    # trusted hosts\n\
                    allow = [\"registry.npmjs.org\"]\n";
        let (_guard, _dir, path) = scoped_config(seed);

        network_inner(Some("allow api.example.com")).expect("edit succeeds");

        let body = fs::read_to_string(&path).expect("reread config");
        // Comments, key order, and unrelated sections survive the edit.
        assert!(body.contains("# user-managed config"));
        assert!(body.contains("# keep my theme"));
        assert!(body.contains("# trusted hosts"));
        let theme_pos = body.find("theme").expect("theme key kept");
        let network_pos = body.find("[network]").expect("network section kept");
        assert!(
            theme_pos < network_pos,
            "top-level keys stay before sections"
        );
        assert!(body.contains("\"registry.npmjs.org\""));
        assert!(body.contains("\"api.example.com\""));
    }

    #[test]
    fn default_edit_creates_only_the_default_key() {
        let _env = lock_test_env();
        let seed = "";
        let (_guard, _dir, path) = scoped_config(seed);

        network_inner(Some("default deny")).expect("edit succeeds");

        let body = fs::read_to_string(&path).expect("reread config");
        assert!(body.contains("default = \"deny\""));
        // The runtime deserializes absent keys with its own defaults; the
        // edit must not materialize them into the file.
        assert!(!body.contains("audit"));
        assert!(!body.contains("allow"));
        assert!(!body.contains("deny = ["));
    }

    #[test]
    fn list_reads_the_written_policy() {
        let _env = lock_test_env();
        let seed = "[network]\nallow = [\"api.example.com\"]\ndefault = \"deny\"\n";
        let (_guard, _dir, _path) = scoped_config(seed);

        let out = network_inner(Some("list")).expect("list succeeds");
        assert!(out.contains("api.example.com"));
        assert!(out.contains("default = deny"));
    }
}
