//! `credentials/describe|set|unset` — the vendored Models settings page's
//! credential half, mapped onto CodeSmith's keyring (`crates/secrets`).
//!
//! The page's references follow its own convention (`deriveKeyRef`): a
//! provider route `nvidia-nim` addresses its key as `NVIDIA_NIM_API_KEY`.
//! The slot a reference resolves to is the same lowercase slug the CLI's
//! `auth` commands and `codesmith login` address (`provider_slot`), so the
//! two surfaces write and read one credential slot.
//!
//! Wire contract (vendored `api-settings-controller` credentials remote,
//! object-keyed args per the gateway's `prepareInvocation`): `describe`
//! answers one `{configured, source?, writable}` view per reference — never
//! the value — and `set`/`unset` write through `Secrets` and follow with a
//! `credentials/record-updated` event frame on `$events` so every open
//! surface re-reads. A store-backed value reports source `file` — the
//! vendored page's spelling for "saved by this page's credential provider"
//! (upstream's local provider labels its store `file`); an environment hit
//! reports `env`. The label names the page's branch, not the physical
//! backend: CodeSmith's file store and OS keychain are both page-writable.
//!
//! `describe` also treats a config.toml inline key as configured: the
//! runtime resolves a provider key from the route's `[providers.<route>]`
//! table (or a `[[providers.custom]]` entry, or the root `api_key` where the
//! runtime accepts it), so a key the CLI wrote there is live even though
//! this page never stored it. That view carries `configured` alone — no
//! `source`, because the page did not save it, and `writable: false`,
//! because this seam cannot replace or clear a config-file key (the page
//! therefore offers no remove action for it).
//!
//! A key stored here is live for the runtime too: `codesmith serve` and
//! `codesmith exec` resolve provider keys from config.toml, then the process
//! environment, then this credential store — builtins by their provider
//! slot, the active custom route by its `[[providers.custom]]` id. Client
//! resolution happens at engine/thread construction, so a key saved after a
//! backend started applies at its next start; and an environment key still
//! wins over a page-saved one (the store is the last layer, consulted only
//! before the missing-key error).
//!
//! Known limitations: `writable` is otherwise a constant `true` — the
//! auto-detected store is writable whenever it exists, and a store that
//! cannot write fails the write itself. The event frame rides
//! `record-updated`; upstream's reference seam names its set/unset fan-out
//! `reference-updated`, and the vendored page listens to both.

use std::collections::HashSet;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use codesmith_secrets::{SecretSource, Secrets};

use super::{rpc_error, rpc_ok, HostState};

/// The reference grammar the credentials remote admits (POSIX identifier,
/// upstream's own `credentialRefSchema`).
fn ref_is_well_formed(r: &str) -> bool {
    !r.is_empty()
        && r.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The page's `deriveKeyRef` spelling of one provider slot's key reference:
/// uppercase, every non-alphanumeric run collapsed to one `_`, `_API_KEY`.
pub fn ref_of_slot(slot: &str) -> String {
    let mut stem = String::new();
    let mut in_run = false;
    for ch in slot.chars().map(|c| c.to_ascii_uppercase()) {
        if ch.is_ascii_alphanumeric() {
            stem.push(ch);
            in_run = false;
        } else if !in_run {
            stem.push('_');
            in_run = true;
        }
    }
    format!("{stem}_API_KEY")
}

/// The provider slot a key reference addresses: `<ROUTE>_API_KEY` back to
/// the lowercase dashed slug, or the lowercased reference itself when it
/// carries no suffix (native-auth environment names the page may describe).
pub fn slot_of_ref(r: &str) -> String {
    let stem = r.strip_suffix("_API_KEY").unwrap_or(r).to_ascii_lowercase();
    stem.replace('_', "-")
}

/// `credentials/describe` — `{refs: [...]}` → one redacted view per ref.
pub async fn describe(args: &Value) -> Value {
    let refs = match args.get("refs").and_then(Value::as_array) {
        Some(refs) => refs
            .iter()
            .filter_map(|r| r.as_str().map(str::to_string))
            .collect::<Vec<_>>(),
        None => {
            return rpc_error(
                "gateway/arguments-invalid",
                "credentials/describe: args must carry a refs array",
            )
        }
    };
    if refs.len() > 64 {
        return rpc_error(
            "gateway/arguments-invalid",
            "credentials/describe: at most 64 references per request",
        );
    }
    if let Some(bad) = refs.iter().find(|r| !ref_is_well_formed(r)) {
        return rpc_error(
            "gateway/arguments-invalid",
            format!("credentials/describe: reference {bad:?} is not a POSIX identifier"),
        );
    }
    // Keyring reads may hit the OS credential store and the config probe
    // reads config.toml — keep both off the async worker threads.
    let slots: Vec<String> = refs.iter().map(|r| slot_of_ref(r)).collect();
    let views = tokio::task::spawn_blocking(move || {
        let inline = super::llm::inline_key_slots(&slots).map_err(|err| format!("{err:#}"))?;
        Ok::<_, String>(describe_refs(&Secrets::auto_detect(), &refs, &inline))
    })
    .await;
    match views {
        Ok(Ok(views)) => rpc_ok(Value::Object(views)),
        Ok(Err(message)) => rpc_error(
            "gateway/internal",
            format!("credentials/describe: read the provider config: {message}"),
        ),
        Err(err) => {
            eprintln!("codesmith desktop: credentials describe join failed: {err}");
            rpc_ok(Value::Object(serde_json::Map::new()))
        }
    }
}

/// The pure describe core over one secrets facade (testable without the
/// process env): an inline config key wins (the runtime sends it), then each
/// reference's slot through the store → env chain, projected to exactly the
/// fields `CredentialInfo` declares.
fn describe_refs(
    secrets: &Secrets,
    refs: &[String],
    inline: &HashSet<String>,
) -> serde_json::Map<String, Value> {
    let mut views = serde_json::Map::new();
    for r in refs {
        let slot = slot_of_ref(r);
        let view = if inline.contains(&slot) {
            // The value lives in config.toml, which this seam can neither
            // replace nor clear; no `source` (the page did not save it), and
            // not writable (the page must not offer removal of a key its
            // unset/delete would leave standing).
            json!({ "configured": true, "writable": false })
        } else {
            match secrets.resolve_with_source(&slot) {
                Some((_, SecretSource::Keyring)) => json!({
                    "configured": true,
                    "source": "file",
                    "writable": true,
                }),
                Some((_, SecretSource::Env)) => json!({
                    "configured": true,
                    "source": "env",
                    "writable": true,
                }),
                None => json!({ "configured": false, "writable": true }),
            }
        };
        views.insert(r.clone(), view);
    }
    views
}

/// `credentials/set` — `{ref, value}` → store the value under the slot.
/// The value only crosses in this direction; no read path returns it.
pub async fn set(host: &HostState, args: &Value) -> Value {
    let (r, value) = match read_ref_and_value(args) {
        Ok(pair) => pair,
        Err(err) => {
            return rpc_error("gateway/arguments-invalid", format!("credentials/set: {err}"))
        }
    };
    let slot = slot_of_ref(&r);
    match run_keyring_write(slot, Some(value)).await {
        Ok(()) => {
            push_record_updated(host, &r).await;
            rpc_ok(Value::Null)
        }
        Err(message) => rpc_error("credential/rejected", format!("credentials/set: {message}")),
    }
}

/// `credentials/unset` — `{ref}` → remove the reference (idempotent).
pub async fn unset(host: &HostState, args: &Value) -> Value {
    let Some(r) = args.get("ref").and_then(Value::as_str).map(str::to_string) else {
        return rpc_error(
            "gateway/arguments-invalid",
            "credentials/unset: args must carry a ref string",
        );
    };
    if !ref_is_well_formed(&r) {
        return rpc_error(
            "gateway/arguments-invalid",
            format!("credentials/unset: reference {r:?} is not a POSIX identifier"),
        );
    }
    let slot = slot_of_ref(&r);
    match run_keyring_write(slot, None).await {
        Ok(()) => {
            push_record_updated(host, &r).await;
            rpc_ok(Value::Null)
        }
        Err(message) => rpc_error("credential/rejected", format!("credentials/unset: {message}")),
    }
}

fn read_ref_and_value(args: &Value) -> Result<(String, String)> {
    let r = args
        .get("ref")
        .and_then(Value::as_str)
        .context("args must carry a ref string")?
        .to_string();
    if !ref_is_well_formed(&r) {
        bail!("reference {r:?} is not a POSIX identifier");
    }
    let value = args
        .get("value")
        .and_then(Value::as_str)
        .context("args must carry a value string")?
        .to_string();
    if value.is_empty() {
        bail!("value must be non-empty");
    }
    Ok((r, value))
}

/// One keyring write off the async workers: `Some(value)` stores, `None`
/// deletes (a delete of a slot nothing stored is success — upstream's own
/// idempotent unset contract).
async fn run_keyring_write(slot: String, value: Option<String>) -> std::result::Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let secrets = Secrets::auto_detect();
        match value {
            Some(value) => secrets.set(&slot, &value).map(|_| ()).map_err(|e| e.to_string()),
            None => secrets
                .delete(&slot)
                .or_else(|err| {
                    let missing = matches!(
                        &err,
                        codesmith_secrets::SecretsError::Io(io)
                            if io.kind() == std::io::ErrorKind::NotFound
                    );
                    if missing {
                        Ok(())
                    } else {
                        Err(err)
                    }
                })
                .map_err(|e| e.to_string()),
        }
    })
    .await
    .map_err(|err| format!("join failed: {err}"))?
}

/// Tell every connected client the credential state moved (the vendored
/// Models page refreshes on this frame).
async fn push_record_updated(host: &HostState, r: &str) {
    host.push_event_frame(json!({
        "type": "emit",
        "event": "credentials/record-updated",
        "args": [r],
    }))
    .await;
}

/// A file-store-free facade for tests.
#[cfg(test)]
pub(crate) fn test_secrets() -> Secrets {
    use std::sync::Arc;
    Secrets::new(Arc::new(codesmith_secrets::InMemoryKeyringStore::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_and_slot_round_trip() {
        for slot in [
            "moonshot",
            "nvidia-nim",
            "wanjie-ark",
            "xiaomi-mimo",
            "openrouter",
            "acme-gateway",
        ] {
            let r = ref_of_slot(slot);
            assert!(ref_is_well_formed(&r), "{r} must be a POSIX identifier");
            assert_eq!(slot_of_ref(&r), slot, "round trip via {r}");
        }
        assert_eq!(ref_of_slot("nvidia-nim"), "NVIDIA_NIM_API_KEY");
        // A reference without the suffix still resolves to a slot name.
        assert_eq!(slot_of_ref("AWS_PROFILE"), "aws-profile");
    }

    #[test]
    fn describe_reports_configured_source_and_never_the_value() {
        let secrets = test_secrets();
        secrets.set("openrouter", "sk-live-value").unwrap();
        let views = describe_refs(
            &secrets,
            &[ref_of_slot("openrouter"), ref_of_slot("novita")],
            &HashSet::new(),
        );
        let configured = &views[&ref_of_slot("openrouter")];
        assert_eq!(configured["configured"], true);
        // The vendored page's "saved" branch is `source === 'file'`; its local
        // provider labels the page-writable store that way.
        assert_eq!(configured["source"], "file");
        assert_eq!(configured["writable"], true);
        assert!(
            !configured.to_string().contains("sk-live-value"),
            "the value must never ride the describe view"
        );
        let missing = &views[&ref_of_slot("novita")];
        assert_eq!(missing["configured"], false);
        assert!(missing.get("source").is_none());
    }

    #[test]
    fn inline_config_keys_are_configured_but_not_page_writable() {
        // The runtime reads config.toml before the store, so a key the CLI
        // wrote there is live even though this page never stored it. The view
        // must say configured, must not claim a page-controlled source, and
        // must not invite an unset that would leave the key standing.
        let secrets = test_secrets();
        secrets.set("openrouter", "sk-shadowed").unwrap();
        let inline: HashSet<String> = ["openrouter".to_string()].into_iter().collect();
        let views = describe_refs(&secrets, &[ref_of_slot("openrouter")], &inline);
        let view = &views[&ref_of_slot("openrouter")];
        assert_eq!(view["configured"], true);
        assert!(
            view.get("source").is_none(),
            "a config-file key is not stored by this page"
        );
        assert_eq!(view["writable"], false);
        assert!(!view.to_string().contains("sk-shadowed"));
    }
}
