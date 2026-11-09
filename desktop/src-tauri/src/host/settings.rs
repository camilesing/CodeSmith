//! `settings/describe|mutate|update` — the desktop shell's preference
//! namespaces plus the `llm-pi-ai` provider namespace, persisted at
//! `~/.codesmith/desktop/settings.json` (UI preferences) and config.toml
//! (providers, via `host::llm`).
//!
//! The UI namespaces are the vendored client's preferences (theme, fonts,
//! enter behavior, transcript view) — desktop-shell state. The provider
//! namespace `llm-pi-ai` is the vendored Models settings page's view over
//! config.toml (`host::llm` owns the mapping); its revision counter lives
//! in the same document so a stale editor is refused with
//! `settings/conflict` exactly like the UI namespaces.
//!
//! Wire contract (vendored `api-remotes` `SettingsDescribeValue`): each
//! namespace view carries a schemastery `toJSON()` envelope, the redacted
//! resolved `value`, and a monotonic `revision` writes echo back as
//! `expectedRevision`. Writes return the updated namespace view; a
//! `settings/document-updated` event frame follows on `$events` so other
//! surfaces re-read.
//!
//! Known limitation: `settings/openSettingsDocument` answers
//! `{opened:false}` — there is no external document editor to hand off to.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::llm;
use super::HostState;

/// One namespace's static schema envelope + its section of the document.
struct NamespaceSpec {
    ns: &'static str,
    /// schemastery `toJSON()` envelope, captured from the vendored packages
    /// at the pinned upstream commit (field ids and defaults must track
    /// `theme-settings.ts` / `chat-settings.ts` / `submission-settings.ts`).
    schema: Value,
    /// Defaults the resolved value falls back to.
    defaults: Value,
}

const THEME_SCHEMA: &str = r#"{"uid":29,"refs":{"2":{"type":"const","meta":{"required":true},"value":"light"},"4":{"type":"const","meta":{"required":true},"value":"dark"},"6":{"type":"const","meta":{"required":true},"value":"system"},"7":{"type":"union","meta":{"default":"system"},"list":[2,4,6]},"12":{"type":"number","meta":{"step":1,"min":10,"max":22,"default":14}},"17":{"type":"number","meta":{"step":1,"min":10,"max":16,"default":11}},"22":{"type":"number","meta":{"step":1,"min":10,"max":20,"default":13}},"24":{"type":"string","meta":{"default":""}},"26":{"type":"string","meta":{"default":""}},"28":{"type":"string","meta":{"default":""}},"29":{"type":"object","meta":{"default":{}},"dict":{"preference":7,"fontSize":12,"codeFontSize":17,"terminalFontSize":22,"textFontFamily":24,"codeFontFamily":26,"terminalFontFamily":28}}}}"#;
const CONVERSATION_SCHEMA: &str = r#"{"uid":27,"refs":{"23":{"type":"const","meta":{"required":true},"value":"queue"},"25":{"type":"const","meta":{"required":true},"value":"steer"},"26":{"type":"union","meta":{"default":"queue"},"list":[23,25]},"27":{"type":"object","meta":{"default":{}},"dict":{"busyEnter":26}}}}"#;
const CHAT_SCHEMA: &str = r#"{"uid":68,"refs":{"32":{"type":"const","meta":{"required":true},"value":"completion"},"34":{"type":"const","meta":{"required":true},"value":"next-input"},"35":{"type":"union","meta":{"default":"completion"},"list":[32,34]},"38":{"type":"const","meta":{"required":true},"value":"all"},"40":{"type":"const","meta":{"required":true},"value":"final"},"41":{"type":"union","meta":{"default":"all"},"list":[38,40]},"44":{"type":"const","meta":{"required":true},"value":"sidebar"},"46":{"type":"const","meta":{"required":true},"value":"new-tab"},"47":{"type":"union","meta":{"default":"sidebar"},"list":[44,46]},"50":{"type":"const","meta":{"required":true},"value":"compact"},"52":{"type":"const","meta":{"required":true},"value":"detailed"},"53":{"type":"union","meta":{"default":"detailed"},"list":[50,52]},"56":{"type":"const","meta":{"required":true},"value":"compact"},"58":{"type":"const","meta":{"required":true},"value":"standard"},"60":{"type":"const","meta":{"required":true},"value":"detailed"},"62":{"type":"const","meta":{"required":true},"value":"verbose"},"64":{"type":"const","meta":{"required":true},"value":"normal"},"66":{"type":"const","meta":{"required":true},"value":"expanded"},"67":{"type":"union","meta":{"loose":true},"list":[56,58,60,62,64,66]},"68":{"type":"object","meta":{"default":{}},"dict":{"collapseTiming":35,"foldedReplies":41,"linkOpening":47,"performanceUsage":53,"transcriptView":67}}}}"#;
/// The vendored settings-general plugin's own runtime namespace — upstream
/// `z.object({ welcomeNoticeVersion: z.string().volatile() })`.
const SETTINGS_GENERAL_SCHEMA: &str = r#"{"uid":2,"refs":{"1":{"type":"string","meta":{"volatile":true}},"2":{"type":"object","meta":{"default":{}},"dict":{"welcomeNoticeVersion":1}}}}"#;
/// The welcome-notice copy version the vendored page acknowledges through
/// the `ui-settings-general` namespace. Seeding the default with it keeps
/// the notice hidden; an upstream bump makes it appear once, and its own
/// dismiss then persists the new value through this namespace.
const WELCOME_NOTICE_VERSION: &str = "2026-09-28.1";

fn specs() -> Vec<NamespaceSpec> {
    vec![
        NamespaceSpec {
            ns: "ui-theme",
            schema: serde_json::from_str(THEME_SCHEMA).expect("theme schema envelope"),
            defaults: json!({
                "preference": "system",
                "fontSize": 14,
                "codeFontSize": 11,
                "terminalFontSize": 13,
                "textFontFamily": "",
                "codeFontFamily": "",
                "terminalFontFamily": "",
            }),
        },
        NamespaceSpec {
            ns: "ui-conversation",
            schema: serde_json::from_str(CONVERSATION_SCHEMA)
                .expect("conversation schema envelope"),
            defaults: json!({ "busyEnter": "queue" }),
        },
        NamespaceSpec {
            ns: "ui-chat",
            schema: serde_json::from_str(CHAT_SCHEMA).expect("chat schema envelope"),
            defaults: json!({
                "transcriptView": "detailed",
                "collapseTiming": "completion",
                "foldedReplies": "all",
                "linkOpening": "sidebar",
                "performanceUsage": "detailed",
            }),
        },
        NamespaceSpec {
            ns: "ui-settings-general",
            schema: serde_json::from_str(SETTINGS_GENERAL_SCHEMA)
                .expect("settings-general schema envelope"),
            defaults: json!({ "welcomeNoticeVersion": WELCOME_NOTICE_VERSION }),
        },
    ]
}

/// The persisted document: `{ "revision": {ns: n}, "sections": {ns: {...}} }`.
struct Document {
    revisions: serde_json::Map<String, Value>,
    sections: serde_json::Map<String, Value>,
    path: PathBuf,
}

fn document_path(home: &std::path::Path) -> PathBuf {
    home.join("desktop").join("settings.json")
}

fn load_document(home: &std::path::Path) -> Document {
    let path = document_path(home);
    let parsed: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}));
    let empty_map = serde_json::Map::new();
    let revisions = parsed
        .get("revisions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let sections = parsed
        .get("sections")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or(empty_map);
    Document {
        revisions,
        sections,
        path,
    }
}

impl Document {
    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let body = json!({ "revisions": self.revisions, "sections": self.sections });
        std::fs::write(&self.path, serde_json::to_string_pretty(&body)?)
            .with_context(|| format!("write {}", self.path.display()))?;
        Ok(())
    }

    fn revision_of(&self, ns: &str) -> u64 {
        self.revisions.get(ns).and_then(Value::as_u64).unwrap_or(0)
    }

    /// The resolved value: defaults overlaid with the stored user section.
    fn resolved(&self, spec: &NamespaceSpec) -> Value {
        let mut value = spec.defaults.clone();
        if let (Some(target), Some(overlay)) = (
            value.as_object_mut(),
            self.sections.get(spec.ns).and_then(Value::as_object),
        ) {
            for (key, item) in overlay {
                target.insert(key.clone(), item.clone());
            }
        }
        value
    }

    fn view(&self, spec: &NamespaceSpec) -> Value {
        let user = self.sections.get(spec.ns).cloned();
        let mut view = json!({
            "autoGenerate": true,
            "ns": spec.ns,
            "schema": spec.schema,
            "value": self.resolved(spec),
            "applies": "live",
            "secrets": [],
            "revision": self.revision_of(spec.ns),
        });
        if let Some(user) = user {
            view["user"] = user;
        }
        view
    }
}

/// A settings write refusal, with the wire code the vendored pages branch
/// on (`settings/conflict` for a stale `expectedRevision`,
/// `settings/rejected` for a write the owning store refuses).
pub struct WriteError {
    pub code: &'static str,
    pub message: String,
}

impl WriteError {
    fn conflict(ns: &str, expected: u64, actual: u64) -> Self {
        Self {
            code: "settings/conflict",
            message: format!(
                "settings namespace {ns} is at revision {actual}, not the expected {expected}"
            ),
        }
    }

    fn rejected(message: String) -> Self {
        Self { code: "settings/rejected", message }
    }
}

/// `settings/describe` — every desktop-owned namespace view, plus the
/// provider namespace over config.toml. A config.toml that does not parse
/// fails the whole read, naming the file.
pub fn describe(home: &std::path::Path) -> Result<Value> {
    let document = load_document(home);
    let mut namespaces: Vec<Value> = specs().iter().map(|spec| document.view(spec)).collect();
    namespaces.push(
        llm::namespace_view(document.revision_of(llm::NS))
            .with_context(|| "read the provider config for settings/describe")?,
    );
    Ok(json!({
        "writable": true,
        "hasDocument": false,
        "namespaces": namespaces,
    }))
}

/// `settings/mutate(ns, ops, expectedRevision?)` and
/// `settings/update(ns, patch, expectedRevision?)` — both fold into the
/// stored user section, bump the namespace revision, persist, notify
/// `settings/document-updated`, and answer the updated view. The provider
/// namespace routes its ops onto config.toml (`host::llm`) and keeps only
/// its revision counter in this document.
pub async fn mutate(
    host: &HostState,
    home: &std::path::Path,
    endpoint: &str,
    args: &Value,
) -> std::result::Result<Value, WriteError> {
    let view = if args.get("ns").and_then(Value::as_str) == Some(llm::NS) {
        provider_fold(endpoint, home, args)?
    } else {
        fold(home, endpoint, args)
            .map_err(|err| fold_write_error(&format!("{err:#}")))?
    };
    let ns = view["ns"].as_str().unwrap_or_default().to_string();
    let revision = view["revision"].as_u64().unwrap_or(0);
    // Invalidate every open client surface the way the Host would.
    host.push_event_frame(json!({
        "type": "emit",
        "event": "settings/document-updated",
        "args": [ns, revision],
    }))
    .await;
    Ok(view)
}

/// `fold`'s one conflict spelling (its stale-revision bail, five lines
/// below) surfaces as the page's `settings/conflict`; everything else is a
/// plain refusal.
fn fold_write_error(message: &str) -> WriteError {
    if message.starts_with("settings revision ") {
        WriteError { code: "settings/conflict", message: message.to_string() }
    } else {
        WriteError::rejected(message.to_string())
    }
}

/// The `llm-pi-ai` write core: check the revision, apply the ops onto
/// config.toml, bump the revision counter, answer the fresh view.
fn provider_fold(
    endpoint: &str,
    home: &std::path::Path,
    args: &Value,
) -> std::result::Result<Value, WriteError> {
    if endpoint != "settings/mutate" {
        return Err(WriteError::rejected(format!(
            "{endpoint} does not serve the {} namespace",
            llm::NS
        )));
    }
    let ops = args
        .get("ops")
        .and_then(Value::as_array)
        .ok_or_else(|| WriteError::rejected("settings mutate without ops".into()))?
        .clone();
    let mut document = load_document(home);
    if let Some(expected) = args.get("expectedRevision").and_then(Value::as_u64) {
        let current = document.revision_of(llm::NS);
        if expected != current {
            return Err(WriteError::conflict(llm::NS, expected, current));
        }
    }
    llm::apply_ops(&ops).map_err(WriteError::rejected)?;
    let next_revision = document.revision_of(llm::NS) + 1;
    document.revisions.insert(llm::NS.to_string(), json!(next_revision));
    document
        .save()
        .map_err(|err| WriteError::rejected(format!("{err:#}")))?;
    llm::namespace_view(next_revision).map_err(|err| WriteError::rejected(format!("{err:#}")))
}

/// The pure write core behind `settings/mutate` / `settings/update`. The
/// client packs `args` as an object keyed by the manifest's parameter names
/// (`{ns, ops, expectedRevision}` / `{ns, patch, expectedRevision}` — see the
/// gateway's `prepareInvocation`), never as a positional array.
fn fold(home: &std::path::Path, endpoint: &str, args: &Value) -> Result<Value> {
    let ns = args
        .get("ns")
        .and_then(Value::as_str)
        .context("settings write without namespace")?
        .to_string();
    let spec = specs()
        .into_iter()
        .find(|spec| spec.ns == ns)
        .with_context(|| format!("settings namespace {ns} is not owned by the desktop shell"))?;
    let expected = args.get("expectedRevision").and_then(Value::as_u64);

    let mut document = load_document(home);
    if let Some(expected) = expected {
        let current = document.revision_of(&ns);
        if expected != current {
            bail!("settings revision {current} does not match expected {expected}");
        }
    }

    let section = document
        .sections
        .entry(ns.clone())
        .or_insert_with(|| json!({}));
    let section_obj = section
        .as_object_mut()
        .with_context(|| format!("settings section {ns} is not an object"))?;
    if endpoint == "settings/update" {
        let patch = args.get("patch").cloned().unwrap_or_else(|| json!({}));
        let patch_obj = patch
            .as_object()
            .context("settings update patch is not an object")?;
        for (key, value) in patch_obj {
            section_obj.insert(key.clone(), value.clone());
        }
    } else {
        let ops = args
            .get("ops")
            .and_then(Value::as_array)
            .context("settings mutate without ops")?;
        for op in ops {
            let path = op
                .get("path")
                .and_then(Value::as_array)
                .context("settings mutate op without path")?;
            let mut target: &mut Value = section;
            for key in &path[..path.len().saturating_sub(1)] {
                let key = key
                    .as_str()
                    .context("settings mutate path segment is not a string")?;
                target = &mut target[key];
            }
            let leaf = path
                .last()
                .and_then(Value::as_str)
                .context("settings mutate path is empty")?;
            match op.get("op").and_then(Value::as_str) {
                Some("set") => {
                    target[leaf] = op.get("value").cloned().unwrap_or(Value::Null);
                }
                Some("unset") => {
                    if let Some(obj) = target.as_object_mut() {
                        obj.remove(leaf);
                    }
                }
                other => bail!("settings mutate op kind {other:?} is not set/unset"),
            }
        }
    }

    let next_revision = document.revision_of(&ns) + 1;
    document.revisions.insert(ns.clone(), json!(next_revision));
    document.save()?;
    Ok(document.view(&spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cds-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn describe_serves_defaults_with_valid_envelopes() {
        let home = temp_home("describe");
        // The provider namespace reads config.toml; point it at a scratch
        // file so the test does not depend on (or touch) the real one.
        let config = home.join("scratch-config.toml");
        std::fs::write(&config, "[providers.openai]\nbase_url = \"https://g.example/v1\"\n").unwrap();
        std::env::set_var("CODESMITH_CONFIG_PATH", &config);
        let value = describe(&home).expect("describe");
        std::env::remove_var("CODESMITH_CONFIG_PATH");
        let namespaces = value["namespaces"].as_array().unwrap();
        let ids: Vec<&str> = namespaces
            .iter()
            .map(|ns| ns["ns"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec!["ui-theme", "ui-conversation", "ui-chat", "ui-settings-general", "llm-pi-ai"]
        );
        let theme = &namespaces[0];
        assert_eq!(theme["value"]["preference"], "system");
        assert_eq!(theme["value"]["fontSize"], 14);
        // The envelope's root uid must exist in refs and be an object schema.
        let uid = theme["schema"]["uid"].as_u64().unwrap().to_string();
        let root = &theme["schema"]["refs"][&uid];
        assert_eq!(root["type"], "object");
        assert!(root["dict"]["preference"].is_u64(), "field refs resolve");
        // The welcome notice reads its namespace as already acknowledged.
        let general = &namespaces[3];
        assert_eq!(general["value"]["welcomeNoticeVersion"], WELCOME_NOTICE_VERSION);
        // The provider namespace rides along with the live config.
        let providers = &namespaces[4];
        assert_eq!(
            providers["value"]["providers"]["openai"]["baseURL"],
            "https://g.example/v1"
        );
        assert_eq!(providers["revision"], 0);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn mutate_folds_object_args_bumps_revision_and_persists() {
        let home = temp_home("mutate");
        // The wire shape is the client's object-keyed args (prepareInvocation),
        // exactly what the browser sends.
        let view = fold(
            &home,
            "settings/mutate",
            &json!({
                "ns": "ui-theme",
                "ops": [
                    { "op": "set", "path": ["preference"], "value": "dark" },
                    { "op": "set", "path": ["fontSize"], "value": 16 },
                ],
                "expectedRevision": 0,
            }),
        )
        .expect("mutate");
        assert_eq!(view["value"]["preference"], "dark");
        assert_eq!(view["value"]["fontSize"], 16);
        assert_eq!(view["revision"], 1);
        assert_eq!(view["user"]["preference"], "dark");
        // A stale expectedRevision is refused rather than silently merged.
        let stale = fold(
            &home,
            "settings/mutate",
            &json!({ "ns": "ui-theme", "ops": [ { "op": "set", "path": ["preference"], "value": "light" } ], "expectedRevision": 0 }),
        );
        assert!(stale.is_err(), "stale revision must be refused");
        // settings/update merges a patch object.
        let updated = fold(
            &home,
            "settings/update",
            &json!({ "ns": "ui-theme", "patch": { "fontSize": 18 } }),
        )
        .expect("update");
        assert_eq!(updated["value"]["fontSize"], 18);
        // The document survives a reload.
        let config = home.join("scratch-config.toml");
        std::fs::write(&config, "").unwrap();
        std::env::set_var("CODESMITH_CONFIG_PATH", &config);
        let reloaded = describe(&home).expect("describe");
        std::env::remove_var("CODESMITH_CONFIG_PATH");
        assert_eq!(reloaded["namespaces"][0]["value"]["fontSize"], 18);
        let _ = std::fs::remove_dir_all(&home);
    }
}
