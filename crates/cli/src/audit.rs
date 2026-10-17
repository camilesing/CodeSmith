//! Lightweight audit logging for sensitive operations.

use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use serde_json::{Value, json};

use crate::utils::open_append;

/// Append an audit event to `~/.codesmith/audit.log`.
///
/// This helper is best-effort by design: callers should not fail critical flows
/// if audit persistence fails.
pub fn log_sensitive_event(event: &str, details: Value) {
    if let Err(err) = append_event(event, details) {
        crate::logging::warn(format!("audit log write failed for event {event}: {err}"));
    }
}

fn append_event(event: &str, details: Value) -> anyhow::Result<()> {
    let path = default_audit_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        // The audit log records details of sensitive operations and
        // `open_append` creates the file 0600; the containing
        // `~/.codesmith` must not stay world-accessible when this helper
        // is the first to create it on a fresh machine.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::metadata(parent)?.permissions();
            if perms.mode() & 0o077 != 0 {
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }
    }
    // Open the append handle (owner-only on Unix via `open_append`), then
    // write each event as one syscall and fsync so the record is durably
    // on disk.
    let mut writer = open_append(&path)?;
    let record = json!({
        "ts": Utc::now().to_rfc3339(),
        "event": event,
        "details": redact_sensitive_keys(details),
    });
    let line = serde_json::to_string(&record)?;
    use std::io::Write;
    // One write syscall for line + newline: under O_APPEND a single
    // `write_all` keeps the record atomic in practice, so concurrent CLI
    // processes cannot interleave a torn half-line into the audit trail.
    // (`writeln!` on the BufWriter can flush mid-record whenever the line
    // exceeds the buffer capacity.) Durability is unchanged — the fsync
    // below covers the bytes that just went out.
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    writer.get_mut().write_all(&bytes)?;
    writer.get_mut().sync_all()?;
    Ok(())
}

/// Key markers whose values are masked before `details` reaches the
/// long-lived plaintext audit log. Callers of [`log_sensitive_event`]
/// naturally pass auth/token/PII-adjacent payloads; a single slip would
/// otherwise persist for the log's lifetime. Over-masking is the safe
/// direction — a redacted field can always be re-derived from the source
/// operation, a leaked credential cannot be un-leaked.
const SENSITIVE_KEY_MARKERS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "authorization",
    "api_key",
    "apikey",
    "access_key",
    "credential",
    "private_key",
];

fn redact_sensitive_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let lower = k.to_ascii_lowercase();
                    if SENSITIVE_KEY_MARKERS.iter().any(|m| lower.contains(m)) {
                        (k, Value::String("[REDACTED]".to_string()))
                    } else {
                        (k, redact_sensitive_keys(v))
                    }
                })
                .collect(),
        ),
        Value::Array(arr) => Value::Array(arr.into_iter().map(redact_sensitive_keys).collect()),
        other => other,
    }
}

fn default_audit_path() -> anyhow::Result<PathBuf> {
    let home = codesmith_config::codesmith_home()
        .map_err(|err| anyhow::anyhow!("CodeSmith home directory not found: {err}"))?;
    Ok(home.join("audit.log"))
}

#[cfg(test)]
mod tests {
    use super::redact_sensitive_keys;
    use serde_json::json;

    #[test]
    fn redacts_sensitive_keys_at_any_depth() {
        let input = json!({
            "provider": "deepseek",
            "api_key": "sk-live-abc",
            "nested": {
                "Authorization": "Bearer xyz",
                "refreshToken": "rt",
                "model": "deepseek-chat"
            },
            "items": [{"password": "hunter2", "name": "ok"}]
        });
        let out = redact_sensitive_keys(input);
        assert_eq!(out["api_key"], "[REDACTED]");
        assert_eq!(out["nested"]["Authorization"], "[REDACTED]");
        assert_eq!(out["nested"]["refreshToken"], "[REDACTED]");
        assert_eq!(out["items"][0]["password"], "[REDACTED]");
        // Non-sensitive fields survive verbatim.
        assert_eq!(out["provider"], "deepseek");
        assert_eq!(out["nested"]["model"], "deepseek-chat");
        assert_eq!(out["items"][0]["name"], "ok");
    }
}
