//! `llm/listConfigurableProviders` / `llm/listProviders` /
//! `llm/discoverModels`, and the `llm-pi-ai` settings namespace they feed —
//! the vendored Models settings page over CodeSmith's provider config.
//!
//! The page's generic OpenAI-compatible editor keys its layout on the
//! namespace name `llm-pi-ai` exactly (anything else renders a hint-only
//! "unknown" card), so the bridge serves that name over config.toml's
//! `[providers]` tables:
//!
//! - the namespace **value** is `{providers: {<route>: profile}}` where a
//!   route is a builtin provider slug or a `[[providers.custom]]` id, and a
//!   profile is `{apiKeyEnv?, displayName?, api?, baseURL?, models: […]}`.
//!   `apiKeyEnv` is always the page's own `<ROUTE>_API_KEY` derivation
//!   (never persisted — it is recomputable), `api` is the single protocol
//!   CodeSmith custom gateways speak, and `models` carries the provider's
//!   one configured model (`id` → `model`, `name` → `display_model`,
//!   `contextWindow` → `context_window`, an `input` naming `image` →
//!   `vision = true`).
//! - **writes** are the page's path ops against `providers.<route>`
//!   (`settings/mutate`, dispatched in `settings.rs`), applied surgically
//!   through `toml_edit` so comments, unknown keys, and CLI-owned fields
//!   elsewhere in config.toml survive byte-for-byte.
//! - the **schema envelope** mirrors upstream pi-ai's Config shape closely
//!   enough for the page's probes — `providers.<route>.api` (the protocol
//!   union) among them — while `models[].reasoningEfforts` is deliberately
//!   absent: CodeSmith configures no per-model reasoning levels, so the
//!   page hides that column instead of letting it edit a field nothing
//!   reads.
//!
//! Known limitations, beside the module docs above: config.toml configures
//! ONE model per provider — a write naming two or more rows is refused
//! rather than silently dropping the rest, and a row's `maxTokens` has no
//! config home (typed, saved as absent); provider edits are load-time state
//! for the runtime, so a running `codesmith serve` sees them only after a
//! restart; a hand-declared custom route needs a provider factory
//! (`providers.toml` or an extension) before the runtime can serve it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use toml_edit::{DocumentMut, Item, Table};

use codesmith_config::{
    custom_endpoint_provider_default, provider_is_self_hosted, resolve_config_path, ProviderKind,
    ProvidersToml,
};

use super::credentials::ref_of_slot;
use super::{rpc_error, rpc_ok, HostState};

/// The settings namespace the vendored page's editor layout is keyed on.
pub const NS: &str = "llm-pi-ai";

/// The only wire protocol CodeSmith custom gateways speak (the
/// `openai-compat` provider backend); the page's protocol picker offers it
/// alone.
const CUSTOM_PROTOCOL: &str = "openai-completions";

/// The builtin route a slug names, if any (exact slugs only — the runtime's
/// alias spellings are not settings keys).
fn builtin_kind(slug: &str) -> Option<ProviderKind> {
    ProviderKind::ALL.iter().find(|k| k.as_str() == slug).copied()
}

/// The `[providers.<key>]` table name for one builtin — the serde field
/// name of `ProvidersToml`, derived from the variant so the two can never
/// drift apart ("NvidiaNim" -> "nvidia_nim").
fn toml_key(kind: ProviderKind) -> String {
    let variant = format!("{kind:?}");
    let mut out = String::new();
    for ch in variant.chars() {
        if ch.is_ascii_uppercase() && !out.is_empty() {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// The schemastery `toJSON()` envelope for the namespace, shaped after
/// upstream pi-ai's `Config` (a volatile `providers` dict of profile
/// objects) with only the fields the page and this bridge actually read or
/// write. Field ids and probe paths must track the vendored
/// `store.ts`/`ProviderEditor.tsx` reads.
const SCHEMA: &str = r#"{"uid":1,"refs":{
"1":{"type":"object","meta":{"default":{}},"dict":{"providers":2}},
"2":{"type":"dict","meta":{"default":{},"volatile":true},"sKey":3,"inner":4},
"3":{"type":"string","meta":{}},
"4":{"type":"object","meta":{"default":{}},"dict":{"apiKeyEnv":5,"displayName":6,"api":7,"baseURL":8,"models":9}},
"5":{"type":"string","meta":{"role":"credential-ref"}},
"6":{"type":"string","meta":{}},
"7":{"type":"union","meta":{},"list":[10]},
"8":{"type":"string","meta":{}},
"9":{"type":"array","meta":{"default":[]},"inner":11},
"10":{"type":"const","meta":{"required":true},"value":"openai-completions"},
"11":{"type":"object","meta":{"default":{}},"dict":{"id":12,"name":13,"contextWindow":14,"maxTokens":15,"input":16}},
"12":{"type":"string","meta":{"required":true}},
"13":{"type":"string","meta":{}},
"14":{"type":"number","meta":{"step":1,"min":1}},
"15":{"type":"number","meta":{"step":1,"min":1}},
"16":{"type":"array","meta":{"default":[]},"inner":17},
"17":{"type":"union","meta":{},"list":[18,19]},
"18":{"type":"const","meta":{"required":true},"value":"text"},
"19":{"type":"const","meta":{"required":true},"value":"image"}
}}"#;

/// The config.toml the bridge reads and writes (the same resolution
/// `codesmith serve` uses: `CODESMITH_CONFIG_PATH`, then the CodeSmith
/// home).
pub fn config_path() -> Result<PathBuf> {
    resolve_config_path(None)
}

fn read_config(path: &Path) -> Result<DocumentMut> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // No config yet: the dormant posture, not a failure.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    text.parse().with_context(|| format!("parse {}", path.display()))
}

/// The typed `[providers]` view (unknown keys inside it ignored — the
/// bridge never serializes the whole file, it edits it in place). The
/// table is re-rooted as a document so its entries deserialize straight
/// into `ProvidersToml`'s per-provider fields.
fn providers_toml(doc: &DocumentMut) -> Result<ProvidersToml> {
    let Some(item) = doc.get("providers") else {
        return Ok(ProvidersToml::default());
    };
    if item.is_none() {
        return Ok(ProvidersToml::default());
    }
    let Some(table) = item.as_table() else {
        anyhow::bail!("the [providers] entry is not a table");
    };
    let mut root = DocumentMut::new();
    for (key, value) in table.iter() {
        root.insert(key, value.clone());
    }
    toml_edit::de::from_document(root).context("parse the [providers] tables")
}

// === Namespace view ========================================================

/// The `llm-pi-ai` namespace view for `settings/describe`: the resolved
/// value is the live config, every configured route is user-layer (the
/// composition base is empty — removal restores defaults).
pub fn namespace_view(revision: u64) -> Result<Value> {
    namespace_view_at(&config_path()?, revision)
}

fn namespace_view_at(path: &Path, revision: u64) -> Result<Value> {
    let doc = read_config(path)?;
    let providers = providers_value(&doc)?;
    let mut view = json!({
        "autoGenerate": true,
        "ns": NS,
        "schema": serde_json::from_str::<Value>(SCHEMA).expect("llm-pi-ai schema envelope"),
        "value": { "providers": providers },
        "base": { "providers": {} },
        "applies": "live",
        "secrets": [],
        "revision": revision,
    });
    if !providers.is_empty() {
        view["user"] = json!({ "providers": providers });
    }
    Ok(view)
}

/// The providers map the namespace value and user layer share: builtin
/// providers with any configured field, plus every `[[providers.custom]]`
/// entry. Key-requiring routes (everything but the self-hosted runtimes —
/// and customs always, whose gateway may not need one but the missing-key
/// dot is the safer default) carry the derived `apiKeyEnv`; self-hosted
/// ones carry none, so the row asks for no key.
fn providers_value(doc: &DocumentMut) -> Result<Map<String, Value>> {
    let typed = providers_toml(doc)?;
    let mut providers = Map::new();
    for kind in ProviderKind::ALL {
        let config = typed.for_provider(kind);
        let configured = config.api_key.as_deref().is_some_and(|v| !v.is_empty())
            || config.base_url.as_deref().is_some_and(|v| !v.is_empty())
            || config.model.as_deref().is_some_and(|v| !v.is_empty())
            || config.auth_mode.as_deref().is_some_and(|v| !v.is_empty())
            || !config.http_headers.is_empty();
        if !configured {
            continue;
        }
        let mut profile = Map::new();
        if !provider_is_self_hosted(kind) {
            profile.insert("apiKeyEnv".into(), json!(ref_of_slot(kind.as_str())));
        }
        if let Some(url) = non_empty(config.base_url.as_deref()) {
            profile.insert("baseURL".into(), json!(url));
        }
        insert_model_row(&mut profile, builtin_table(doc, &toml_key(kind)));
        providers.insert(kind.as_str().to_string(), Value::Object(profile));
    }
    for custom in &typed.custom {
        let mut profile = Map::new();
        profile.insert("apiKeyEnv".into(), json!(ref_of_slot(&custom.id)));
        if let Some(name) = non_empty(custom.display_name.as_deref()) {
            profile.insert("displayName".into(), json!(name));
        }
        profile.insert("api".into(), json!(CUSTOM_PROTOCOL));
        if let Some(url) = non_empty(custom.base_url.as_deref()) {
            profile.insert("baseURL".into(), json!(url));
        }
        insert_model_row(&mut profile, custom_table(doc, &custom.id));
        providers.insert(custom.id.clone(), Value::Object(profile));
    }
    Ok(providers)
}

/// The `models` row of one profile: config.toml configures a single model,
/// so the row list is `[]` (serve the provider's default) or one entry
/// mapping the provider table's model trio.
fn insert_model_row(profile: &mut Map<String, Value>, table: Option<&Table>) {
    let Some(table) = table else {
        profile.insert("models".into(), json!([]));
        return;
    };
    let Some(model) = table.get("model").and_then(|v| v.as_str()) else {
        profile.insert("models".into(), json!([]));
        return;
    };
    if model.is_empty() {
        profile.insert("models".into(), json!([]));
        return;
    }
    let mut row = Map::new();
    row.insert("id".into(), json!(model));
    if let Some(name) = table.get("display_model").and_then(|v| v.as_str()) {
        row.insert("name".into(), json!(name));
    }
    if let Some(window) = table.get("context_window").and_then(|v| v.as_integer()) {
        row.insert("contextWindow".into(), json!(u64::try_from(window).unwrap_or(0)));
    }
    match table.get("vision").and_then(|v| v.as_bool()) {
        Some(true) => {
            row.insert("input".into(), json!(["text", "image"]));
        }
        Some(false) => {
            row.insert("input".into(), json!(["text"]));
        }
        None => {}
    }
    profile.insert("models".into(), Value::Array(vec![Value::Object(row)]));
}

fn builtin_table<'a>(doc: &'a DocumentMut, toml_key: &str) -> Option<&'a Table> {
    doc.get("providers")?.get(toml_key)?.as_table()
}

fn custom_table<'a>(doc: &'a DocumentMut, id: &str) -> Option<&'a Table> {
    doc.get("providers")?
        .get("custom")?
        .as_array_of_tables()?
        .iter()
        .find(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(id))
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.trim().is_empty())
}

// === Mutate (config.toml writes) ===========================================

/// Apply the page's `settings/mutate` path ops for `llm-pi-ai` onto
/// config.toml. Surgical `toml_edit` edits only — everything the bridge
/// does not own (comments, other tables, unknown keys) survives.
pub fn apply_ops(ops: &[Value]) -> Result<(), String> {
    let path = config_path().map_err(|err| format!("resolve config.toml: {err:#}"))?;
    apply_ops_at(&path, ops)
}

fn apply_ops_at(path: &Path, ops: &[Value]) -> Result<(), String> {
    let mut doc = read_config(path).map_err(|err| format!("read config.toml: {err:#}"))?;
    for op in ops {
        let failure = apply_one(&mut doc, op);
        failure.map_err(|message| format!("llm-pi-ai: {message}"))?;
    }
    codesmith_config::write_config_file_private(path, &doc.to_string())
        .map_err(|err| format!("write {}: {err:#}", path.display()))
}

fn apply_one(doc: &mut DocumentMut, op: &Value) -> Result<(), String> {
    let kind = op.get("op").and_then(Value::as_str).unwrap_or_default();
    let segments = op
        .get("path")
        .and_then(Value::as_array)
        .map(|path| path.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    match (kind, segments.as_slice()) {
        ("set", ["providers", route]) => {
            set_whole_profile(doc, route, op.get("value").unwrap_or(&Value::Null))
        }
        ("unset", ["providers", route]) => remove_profile(doc, route),
        ("set", ["providers", route, field]) => {
            set_profile_field(doc, route, field, op.get("value"))
        }
        ("unset", ["providers", route, field]) => set_profile_field(doc, route, field, None),
        _ => Err(format!(
            "settings op {kind:?} at {segments:?} is not a providers profile write"
        )),
    }
}

/// A valid route id for a newly declared custom provider: the same shape
/// the vendored create card enforces (a lowercase stem the credential
/// reference grammar can carry).
fn valid_custom_route(route: &str) -> bool {
    let bytes = route.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && !route.ends_with('-')
        && !route.contains("--")
}

/// Write the model fields of one provider table from the page's `models`
/// array. Zero rows clear the override; one row writes it; more are
/// refused — config.toml configures one model per provider.
fn write_models(table: &mut Table, models: &Value) -> Result<(), String> {
    let clear = |table: &mut Table| {
        for key in ["model", "display_model", "context_window", "vision"] {
            table.remove(key);
        }
    };
    let Some(rows) = models.as_array() else {
        clear(table);
        return Ok(());
    };
    match rows.as_slice() {
        [] => {
            clear(table);
            Ok(())
        }
        [row] => {
            let Some(id) = row.get("id").and_then(Value::as_str).filter(|id| !id.trim().is_empty())
            else {
                return Err("a model row without an id cannot be saved".into());
            };
            table["model"] = toml_edit::value(id);
            set_optional_string(table, "display_model", row.get("name"));
            set_optional_integer(table, "context_window", row.get("contextWindow"));
            match row.get("input").and_then(Value::as_array) {
                Some(input) => {
                    table["vision"] =
                        toml_edit::value(input.iter().any(|m| m.as_str() == Some("image")));
                }
                None => {
                    table.remove("vision");
                }
            }
            Ok(())
        }
        rows => Err(format!(
            "config.toml configures one model per provider; the write named {}",
            rows.len()
        )),
    }
}

fn set_optional_string(table: &mut Table, key: &str, value: Option<&Value>) {
    match value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(text) => table[key] = toml_edit::value(text),
        None => {
            table.remove(key);
        }
    }
}

fn set_optional_integer(table: &mut Table, key: &str, value: Option<&Value>) {
    match value
        .and_then(Value::as_u64)
        .and_then(|v| i64::try_from(v).ok())
        .filter(|v| *v > 0)
    {
        Some(number) => table[key] = toml_edit::value(number),
        None => {
            table.remove(key);
        }
    }
}

/// `["providers", route] = profile` — the create card's whole-profile write
/// and the editor's native-profile materialization (`{}`).
fn set_whole_profile(doc: &mut DocumentMut, route: &str, value: &Value) -> Result<(), String> {
    let profile = value
        .as_object()
        .ok_or_else(|| format!("providers.{route} must be set to a profile object, not a scalar"))?;
    if let Some(kind) = builtin_kind(route) {
        let key = toml_key(kind);
        let providers = providers_table(doc);
        let table = providers
            .entry(key.as_str())
            .or_insert_with(|| Item::Table(Table::new()));
        let table = table
            .as_table_mut()
            .ok_or_else(|| format!("providers.{key} is not a table"))?;
        return write_profile_fields(table, route, profile, false);
    }
    // A custom route: the declaration IS the entry.
    let taken = custom_ids(doc).iter().any(|id| id == route);
    if !taken {
        if !valid_custom_route(route) {
            return Err(format!(
                "route {route:?} is not a valid custom provider id (lowercase letters, digits, dashes)"
            ));
        }
        if non_empty(profile.get("baseURL").and_then(Value::as_str)).is_none() {
            return Err(format!(
                "a custom provider needs a baseURL (set providers.{route}.baseURL)"
            ));
        }
    }
    let providers = providers_table(doc);
    let customs = providers
        .entry("custom")
        .or_insert_with(|| Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    let customs = customs
        .as_array_of_tables_mut()
        .ok_or_else(|| "providers.custom is not an array of tables".to_string())?;
    let existing = customs
        .iter_mut()
        .find(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(route));
    match existing {
        Some(entry) => write_profile_fields(entry, route, profile, true),
        None => {
            let mut fresh = Table::new();
            fresh["id"] = toml_edit::value(route);
            customs.push(fresh);
            let entry = customs
                .iter_mut()
                .find(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(route))
                .expect("the entry was just pushed");
            write_profile_fields(entry, route, profile, true)
        }
    }
}

/// The field mapping shared by whole-profile sets and per-field ops.
/// `custom` names a `[[providers.custom]]` entry (the only routes that may
/// carry `displayName`/`api`); a field with no config home is refused —
/// `apiKeyEnv` is the page's own derivation (validated, never stored) and
/// `maxTokens`/`input` extras live inside the model row mapping.
fn write_profile_fields(
    table: &mut Table,
    route: &str,
    profile: &Map<String, Value>,
    custom: bool,
) -> Result<(), String> {
    for (field, value) in profile {
        match field.as_str() {
            "apiKeyEnv" => {
                if value.as_str() != Some(&ref_of_slot(route)) {
                    return Err(format!(
                        "providers.{route}.apiKeyEnv must be {} (the page's own derivation)",
                        ref_of_slot(route)
                    ));
                }
            }
            "baseURL" => set_optional_string(table, "base_url", Some(value)),
            "models" => write_models(table, value)?,
            "displayName" if custom => set_optional_string(table, "display_name", Some(value)),
            "api" if custom => {
                if value.as_str() != Some(CUSTOM_PROTOCOL) {
                    return Err(format!(
                        "providers.{route}.api must be {CUSTOM_PROTOCOL:?} (the one protocol CodeSmith gateways speak)"
                    ));
                }
                // Derived on read; never stored.
            }
            other => {
                return Err(format!("providers.{route}.{other} has no config.toml mapping"));
            }
        }
    }
    Ok(())
}

/// `["providers", route, field]` set/unset — the editor's per-field ops.
fn set_profile_field(
    doc: &mut DocumentMut,
    route: &str,
    field: &str,
    value: Option<&Value>,
) -> Result<(), String> {
    // The page's derived fields: their ops are no-ops on config.
    if field == "apiKeyEnv" || (field == "api" && custom_ids(doc).iter().any(|id| id == route)) {
        return Ok(());
    }
    // An unset becomes the write that clears the field's config keys.
    let mut single = Map::new();
    match (field, value) {
        (_, Some(value)) => {
            single.insert(field.to_string(), value.clone());
        }
        ("baseURL", None) => {
            single.insert("baseURL".into(), Value::String(String::new()));
        }
        ("models", None) => {
            single.insert("models".into(), json!([]));
        }
        ("displayName", None) if custom_ids(doc).iter().any(|id| id == route) => {
            single.insert("displayName".into(), Value::String(String::new()));
        }
        (other, None) => {
            return Err(format!("providers.{route}.{other} has no config.toml mapping"));
        }
    }
    if let Some(kind) = builtin_kind(route) {
        let key = toml_key(kind);
        if value.is_none() && builtin_table(doc, &key).is_none() {
            // Clearing a field of a provider with no table: nothing to clear.
            return Ok(());
        }
        let providers = providers_table(doc);
        let table = providers
            .entry(key.as_str())
            .or_insert_with(|| Item::Table(Table::new()));
        let table = table
            .as_table_mut()
            .ok_or_else(|| format!("providers.{key} is not a table"))?;
        return write_profile_fields(table, route, &single, false);
    }
    let customs = doc
        .get_mut("providers")
        .and_then(|p| p.get_mut("custom"))
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| "providers.custom is not an array of tables".to_string())?;
    let entry = customs
        .iter_mut()
        .find(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(route))
        .ok_or_else(|| {
            format!("route {route:?} is neither a builtin provider nor a declared custom provider")
        })?;
    write_profile_fields(entry, route, &single, true)
}

/// Remove one provider entirely: a builtin's table (restoring its defaults)
/// or a `[[providers.custom]]` entry.
fn remove_profile(doc: &mut DocumentMut, route: &str) -> Result<(), String> {
    if let Some(kind) = builtin_kind(route) {
        if let Some(providers) = doc.get_mut("providers").and_then(Item::as_table_mut) {
            providers.remove(toml_key(kind).as_str());
        }
        return Ok(());
    }
    let customs = doc
        .get_mut("providers")
        .and_then(|p| p.get_mut("custom"))
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| "providers.custom is not an array of tables".to_string())?;
    let index = customs
        .iter()
        .position(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(route))
        .ok_or_else(|| format!("no custom provider {route:?} to remove"))?;
    customs.remove(index);
    Ok(())
}

fn providers_table(doc: &mut DocumentMut) -> &mut Table {
    doc.entry("providers")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .expect("providers is a table")
}

fn custom_ids(doc: &DocumentMut) -> Vec<String> {
    doc.get("providers")
        .and_then(|p| p.get("custom"))
        .and_then(Item::as_array_of_tables)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("id").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// === llm/* endpoints =======================================================

/// `llm/listConfigurableProviders` — the directory the page joins with live
/// routes: every builtin provider (declared `false` — the runtime ships
/// them) plus every `[[providers.custom]]` entry (declared `true`).
pub fn list_configurable_providers() -> Value {
    match directory_entries() {
        Ok(entries) => rpc_ok(entries),
        Err(err) => rpc_error("gateway/internal", format!("llm/listConfigurableProviders: {err:#}")),
    }
}

fn directory_entries() -> Result<Value> {
    let doc = read_config(&config_path()?)?;
    directory_entries_at(&doc)
}

fn directory_entries_at(doc: &DocumentMut) -> Result<Value> {
    let typed = providers_toml(doc)?;
    let mut entries = Vec::new();
    for kind in ProviderKind::ALL {
        let mut entry = json!({
            "provider": kind.as_str(),
            "displayName": kind.display_name(),
            "settingsNs": NS,
            "settingsPath": ["providers", kind.as_str()],
            "declared": false,
        });
        if let Some(url) = custom_endpoint_provider_default(kind) {
            entry["defaultBaseURL"] = json!(url);
        }
        entries.push(entry);
    }
    for custom in &typed.custom {
        entries.push(json!({
            "provider": custom.id,
            "displayName": custom.display_name.clone().unwrap_or_else(|| custom.id.clone()),
            "settingsNs": NS,
            "settingsPath": ["providers", custom.id],
            "declared": true,
        }));
    }
    Ok(Value::Array(entries))
}

/// `llm/listProviders` — the routes the runtime actually serves, from its
/// model catalog.
pub async fn list_providers(host: &HostState) -> Value {
    match host.backend.get::<Value>("/v1/models").await {
        Ok(models) => {
            let mut ids: Vec<String> = Vec::new();
            if let Some(rows) = models.as_array() {
                for row in rows {
                    if let Some(provider) = row.get("provider").and_then(Value::as_str) {
                        if !ids.iter().any(|id| id == provider) {
                            ids.push(provider.to_string());
                        }
                    }
                }
            }
            let rows: Vec<Value> = ids
                .iter()
                .map(|id| {
                    json!({
                        "id": id,
                        "name": builtin_kind(id).map(ProviderKind::display_name).unwrap_or(id.as_str()),
                    })
                })
                .collect();
            rpc_ok(Value::Array(rows))
        }
        Err(err) => rpc_error("gateway/internal", format!("llm/listProviders: {err:#}")),
    }
}

/// `llm/discoverModels` — one endpoint interrogation. With a `baseURL` the
/// bridge asks the endpoint's own model listing (OpenAI-compatible
/// `{data: [...]}`); without one, a named route answers from the runtime's
/// catalog, exactly the registry answer the page expects a known route to
/// give.
pub async fn discover_models(host: &HostState, args: &Value) -> Value {
    let ns = args.get("settingsNs").and_then(Value::as_str).unwrap_or_default();
    if ns != NS {
        return rpc_error(
            "gateway/arguments-invalid",
            format!("llm/discoverModels: unknown settings namespace {ns:?}"),
        );
    }
    let provider = args.get("provider").and_then(Value::as_str);
    let base_url = args
        .get("baseURL")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty());
    let Some(base_url) = base_url else {
        let Some(provider) = provider else {
            return rpc_error(
                "llm/model-discovery-rejected",
                "llm/discoverModels: a draft with neither a route nor an endpoint has nothing to ask",
            );
        };
        return match host.backend.get::<Value>("/v1/models").await {
            Ok(models) => {
                let rows = models
                    .as_array()
                    .map(|all| {
                        all.iter()
                            .filter(|row| {
                                row.get("provider").and_then(Value::as_str) == Some(provider)
                            })
                            .map(|row| {
                                let mut discovered = json!({ "id": row["id"] });
                                if let Some(window) =
                                    row.get("context_window").and_then(Value::as_u64)
                                {
                                    discovered["contextWindow"] = json!(window);
                                }
                                discovered
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                rpc_ok(Value::Array(rows))
            }
            Err(err) => rpc_error(
                "gateway/internal",
                format!("llm/discoverModels: runtime catalog unavailable: {err:#}"),
            ),
        };
    };
    interrogate_endpoint(base_url, provider, args.get("apiKey").and_then(Value::as_str)).await
}

/// GET `{base}/models` with the key the form typed, else the named route's
/// stored credential.
async fn interrogate_endpoint(
    base_url: &str,
    provider: Option<&str>,
    api_key: Option<&str>,
) -> Value {
    let typed_key = api_key.map(str::trim).filter(|key| !key.is_empty());
    let api_key = match typed_key {
        Some(key) => Some(key.to_string()),
        None => match provider {
            Some(provider) => {
                let slot = super::credentials::slot_of_ref(provider);
                tokio::task::spawn_blocking(move || {
                    codesmith_secrets::Secrets::auto_detect().resolve(&slot)
                })
                .await
                .unwrap_or(None)
            }
            None => None,
        },
    };
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("static reqwest client config");
    let mut request = client.get(&url);
    if let Some(key) = api_key.as_deref() {
        request = request.bearer_auth(key);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            return rpc_error(
                "llm/model-discovery-rejected",
                format!("llm/discoverModels: {url} did not answer: {err}"),
            )
        }
    };
    if !response.status().is_success() {
        return rpc_error(
            "llm/model-discovery-rejected",
            format!("llm/discoverModels: {url} answered {}", response.status()),
        );
    }
    let body: Value = match response.json().await {
        Ok(body) => body,
        Err(err) => {
            return rpc_error(
                "llm/model-discovery-rejected",
                format!("llm/discoverModels: {url} did not answer JSON: {err}"),
            )
        }
    };
    // OpenAI-compatible listings carry `{data: [{id, display_name?,
    // context_length?}]}`; ollama spells the window `context_window`.
    let Some(rows) = body.get("data").and_then(Value::as_array) else {
        return rpc_error(
            "llm/model-discovery-rejected",
            format!("llm/discoverModels: {url} listed no models data array"),
        );
    };
    let models: Vec<Value> = rows
        .iter()
        .filter_map(|row| {
            let id = row.get("id").and_then(Value::as_str)?;
            let mut discovered = json!({ "id": id });
            if let Some(name) = row
                .get("display_name")
                .or_else(|| row.get("name"))
                .and_then(Value::as_str)
            {
                discovered["name"] = json!(name);
            }
            for key in ["context_length", "context_window"] {
                if let Some(window) = row.get(key).and_then(Value::as_u64) {
                    discovered["contextWindow"] = json!(window);
                    break;
                }
            }
            if let Some(max) = row.get("max_tokens").and_then(Value::as_u64) {
                discovered["maxTokens"] = json!(max);
            }
            Some(discovered)
        })
        .collect();
    rpc_ok(Value::Array(models))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cds-llm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    /// The page's nodeAtPath walk over the envelope, mirrored so the probe
    /// contract is tested, not just the envelope's shape.
    fn node_at<'a>(envelope: &'a Value, path: &[&str]) -> Option<&'a Value> {
        let refs = envelope.get("refs")?;
        let mut node = refs.get(envelope.get("uid")?.to_string())?;
        for key in path {
            node = match node.get("type").and_then(Value::as_str) {
                Some("object") => refs.get(node["dict"][key].to_string()),
                Some("dict" | "array") => refs.get(node["inner"].to_string()),
                _ => None,
            }?;
        }
        Some(node)
    }

    #[test]
    fn envelope_carries_the_protocol_probe_path() {
        let envelope: Value = serde_json::from_str(SCHEMA).unwrap();
        let protocols = node_at(&envelope, &["providers", "probe", "api"]).unwrap();
        assert_eq!(protocols["type"], "union");
        let choices: Vec<&str> = protocols["list"]
            .as_array()
            .unwrap()
            .iter()
            .map(|uid| envelope["refs"][uid.to_string()]["value"].as_str().unwrap())
            .collect();
        assert_eq!(choices, vec!["openai-completions"]);
        // The reasoning-efforts probe finds no node, so the page hides the
        // per-model reasoning column (CodeSmith configures none).
        let levels = node_at(&envelope, &["providers", "probe", "models", "0", "reasoningEfforts"]);
        assert!(levels.is_none());
    }

    #[test]
    fn namespace_view_serves_configured_routes_only() {
        let path = temp_config("view");
        std::fs::write(
            &path,
            r#"
default_text_model = "auto"

[providers.openai]
base_url = "https://gateway.example/v1"
model = "gpt-custom"
display_model = "GPT Custom"
context_window = 200000

[providers.openrouter]
api_key = "sk-inline"

[providers.ollama]
# self-hosted: configured, but the profile names no credential reference.
base_url = "http://localhost:11434/v1"

[[providers.custom]]
id = "acme-gateway"
base_url = "https://acme.example/v1"
model = "acme-1"
display_name = "Acme"
"#,
        )
        .unwrap();
        let view = namespace_view_at(&path, 3).unwrap();
        assert_eq!(view["ns"], NS);
        assert_eq!(view["revision"], 3);
        assert_eq!(view["base"], json!({ "providers": {} }));
        let providers = view["value"]["providers"].as_object().unwrap();
        assert_eq!(providers.len(), 4, "unconfigured builtins stay dormant");
        let openai = &providers["openai"];
        assert_eq!(openai["apiKeyEnv"], "OPENAI_API_KEY");
        assert_eq!(openai["baseURL"], "https://gateway.example/v1");
        assert_eq!(
            openai["models"],
            json!([{ "id": "gpt-custom", "name": "GPT Custom", "contextWindow": 200000 }])
        );
        let openrouter = &providers["openrouter"];
        assert_eq!(openrouter["apiKeyEnv"], "OPENROUTER_API_KEY");
        assert!(openrouter.get("baseURL").is_none());
        assert_eq!(openrouter["models"], json!([]));
        // Self-hosted: no credential reference, the row asks for no key.
        let ollama = &providers["ollama"];
        assert!(ollama.get("apiKeyEnv").is_none());
        let acme = &providers["acme-gateway"];
        assert_eq!(acme["apiKeyEnv"], "ACME_GATEWAY_API_KEY");
        assert_eq!(acme["displayName"], "Acme");
        assert_eq!(acme["api"], "openai-completions");
        assert_eq!(acme["models"], json!([{ "id": "acme-1" }]));
        // Every configured route is user-layer (removable).
        assert_eq!(view["user"]["providers"].as_object().unwrap().len(), 4);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn directory_lists_builtins_then_customs() {
        let path = temp_config("directory");
        std::fs::write(
            &path,
            r#"
[[providers.custom]]
id = "acme-gateway"
base_url = "https://acme.example/v1"
display_name = "Acme"
"#,
        )
        .unwrap();
        let doc = read_config(&path).unwrap();
        let entries = directory_entries_at(&doc).unwrap();
        let rows = entries.as_array().unwrap();
        assert_eq!(rows.len(), 17, "16 builtins plus the custom entry");
        let first = ProviderKind::ALL[0];
        assert_eq!(rows[0]["provider"], first.as_str());
        assert_eq!(rows[0]["settingsNs"], NS);
        assert_eq!(rows[0]["settingsPath"], json!(["providers", first.as_str()]));
        assert_eq!(rows[0]["declared"], false);
        let openai = rows.iter().find(|row| row["provider"] == "openai").unwrap();
        assert_eq!(openai["defaultBaseURL"], "https://api.openai.com/v1");
        let acme = rows.last().unwrap();
        assert_eq!(acme["provider"], "acme-gateway");
        assert_eq!(acme["displayName"], "Acme");
        assert_eq!(acme["declared"], true);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn create_custom_route_then_edit_fields_preserving_foreign_config() {
        let path = temp_config("mutate");
        std::fs::write(
            &path,
            "# hand-written comment\n[providers.moonshot]\napi_key = \"sk-keep\"\n\n[sandbox]\nenabled = true\n",
        )
        .unwrap();
        let ops = [json!({
            "op": "set", "path": ["providers", "acme-gateway"],
            "value": {
                "displayName": "Acme",
                "apiKeyEnv": "ACME_GATEWAY_API_KEY",
                "api": "openai-completions",
                "baseURL": "https://acme.example/v1",
                "models": [{ "id": "acme-1", "name": "Acme One", "contextWindow": 131072, "input": ["text", "image"] }],
            },
        })];
        apply_ops_at(&path, &ops).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# hand-written comment"), "comments survive");
        assert!(text.contains("[sandbox]"), "foreign tables survive");
        assert!(text.contains("sk-keep"), "other providers survive");
        let doc = read_config(&path).unwrap();
        let acme = custom_table(&doc, "acme-gateway").unwrap();
        assert_eq!(acme.get("id").unwrap().as_str(), Some("acme-gateway"));
        assert_eq!(
            acme.get("base_url").unwrap().as_str(),
            Some("https://acme.example/v1")
        );
        assert_eq!(acme.get("display_name").unwrap().as_str(), Some("Acme"));
        assert_eq!(acme.get("model").unwrap().as_str(), Some("acme-1"));
        assert_eq!(acme.get("display_model").unwrap().as_str(), Some("Acme One"));
        assert_eq!(acme.get("context_window").unwrap().as_integer(), Some(131072));
        assert_eq!(acme.get("vision").unwrap().as_bool(), Some(true));

        // The editor's per-field ops: retitle, clear the model override, and
        // the native-profile materialization for a dormant builtin.
        let ops = [
            json!({ "op": "set", "path": ["providers", "acme-gateway", "displayName"], "value": "Acme 2" }),
            json!({ "op": "unset", "path": ["providers", "acme-gateway", "models"] }),
            json!({ "op": "set", "path": ["providers", "vllm"], "value": {} }),
        ];
        apply_ops_at(&path, &ops).unwrap();
        let doc = read_config(&path).unwrap();
        let acme = custom_table(&doc, "acme-gateway").unwrap();
        assert_eq!(acme.get("display_name").unwrap().as_str(), Some("Acme 2"));
        assert!(acme.get("model").is_none());
        assert!(acme.get("display_model").is_none());
        assert!(builtin_table(&doc, "vllm").is_some());

        // Removal drops the custom entry and the builtin table alike.
        let ops = [
            json!({ "op": "unset", "path": ["providers", "acme-gateway"] }),
            json!({ "op": "unset", "path": ["providers", "vllm"] }),
        ];
        apply_ops_at(&path, &ops).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let doc: DocumentMut = text.parse().unwrap();
        assert!(custom_table(&doc, "acme-gateway").is_none());
        assert!(doc.get("providers").and_then(|p| p.get("vllm")).is_none());
        assert!(text.contains("sk-keep"), "the untouched provider still stands");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn writes_refuse_what_config_cannot_hold() {
        let path = temp_config("refuse");
        std::fs::write(&path, "").unwrap();
        // Two model rows on one route.
        let ops = [json!({
            "op": "set", "path": ["providers", "acme"],
            "value": { "baseURL": "https://a.example", "models": [{ "id": "a" }, { "id": "b" }] },
        })];
        let err = apply_ops_at(&path, &ops).unwrap_err();
        assert!(err.contains("one model"), "{err}");
        // An unknown profile field.
        let ops = [json!({
            "op": "set", "path": ["providers", "acme"],
            "value": { "baseURL": "https://a.example", "thinkingBudgets": { "low": 1 } },
        })];
        let err = apply_ops_at(&path, &ops).unwrap_err();
        assert!(err.contains("thinkingBudgets"), "{err}");
        // A builtin route cannot carry the custom identity fields.
        let ops = [json!({
            "op": "set", "path": ["providers", "moonshot"], "value": { "api": "openai-completions" },
        })];
        let err = apply_ops_at(&path, &ops).unwrap_err();
        assert!(err.contains("api"), "{err}");
        // A custom creation without an endpoint.
        let ops = [json!({
            "op": "set", "path": ["providers", "acme"], "value": { "models": [{ "id": "a" }] },
        })];
        let err = apply_ops_at(&path, &ops).unwrap_err();
        assert!(err.contains("baseURL"), "{err}");
        // A row without an id.
        let ops = [json!({
            "op": "set", "path": ["providers", "acme"],
            "value": { "baseURL": "https://a.example", "models": [{ "name": "no id" }] },
        })];
        assert!(apply_ops_at(&path, &ops).is_err());
        assert!(
            std::fs::read_to_string(&path).unwrap().is_empty(),
            "refused writes change nothing"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
