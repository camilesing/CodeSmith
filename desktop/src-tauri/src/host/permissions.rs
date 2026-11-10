//! The permission-preset surface — the vendored `client-ui-permission-presets`
//! plugin's host half over CodeSmith's per-thread mode.
//!
//! Upstream models permissions as a process-level preset catalog plus a
//! per-session `permissions` projection. CodeSmith already has the matching
//! product concept in `thread.mode` (`agent` / `plan` / `yolo` /
//! `coordinator`), so the bridge maps the two rather than inventing a second
//! permission system:
//!
//! * catalog values are the upstream spellings; `danger-full-access` is
//!   CodeSmith's `yolo` (full access, no approvals), which also gives the
//!   picker its localized "Full access" label and risk confirmation;
//! * a pick submits `/permission <value>` (or the composer chip does), and
//!   `commands` PATCHes the thread mode;
//! * `permissions.currentValue` projects the reverse mapping, so the active
//!   mark and the settings row agree with the thread record.
//!
//! The new-session default lives in the desktop settings document
//! (`permission.defaultPreset`) and is applied by `session/create`; an
//! unknown stored value falls back to `agent` rather than failing creation.

use serde_json::{Value, json};

/// The default preset (also the runtime's default thread mode).
pub const DEFAULT_PRESET: &str = "agent";

/// The catalog value that maps onto CodeSmith's `yolo` mode. The vendored
/// picker keys its localized label and explicit risk gate on this exact
/// string.
pub const FULL_ACCESS_PRESET: &str = "danger-full-access";

/// Every catalog option, in display order.
pub const PRESETS: [&str; 4] = ["agent", "plan", FULL_ACCESS_PRESET, "coordinator"];

/// The catalog value for one thread mode. Unknown modes read as the default,
/// keeping `permissions.currentValue` inside the catalog the client holds.
pub fn preset_of_mode(mode: &str) -> &'static str {
    match mode {
        "plan" => "plan",
        "yolo" => FULL_ACCESS_PRESET,
        "coordinator" => "coordinator",
        _ => DEFAULT_PRESET,
    }
}

/// The thread mode for one catalog value (also accepts the raw mode names a
/// hand-typed `/permission yolo` may carry). `None` for anything else, which
/// the command catalog reports as a usage error.
pub fn mode_of_preset(preset: &str) -> Option<&'static str> {
    match preset {
        "agent" => Some("agent"),
        "plan" => Some("plan"),
        FULL_ACCESS_PRESET | "yolo" => Some("yolo"),
        "coordinator" => Some("coordinator"),
        _ => None,
    }
}

/// `permissionPresets/catalog` — the process catalog the picker and the
/// General settings row read. `defaultPreset` is validated against the
/// advertised options so the settings row's "current default" lookup cannot
/// throw on a hand-edited document.
///
/// The vendored presentation layer localizes `danger-full-access` by value
/// when it receives the English default label; `agent`/`plan`/`coordinator`
/// have no built-in dictionary entry, so the bridge supplies their
/// CodeSmith-specific labels. `locale` is the user's explicit language
/// preference; an unsupported language keeps the English labels. A language
/// switch reaches the labels on the next catalog fetch (a reload).
pub fn catalog(default_preset: &str, locale: Option<&str>) -> Value {
    let default = if PRESETS.contains(&default_preset) {
        default_preset
    } else {
        DEFAULT_PRESET
    };
    let options: Vec<Value> = PRESETS
        .iter()
        .map(|preset| {
            json!({
                "value": preset,
                "name": display_name(preset, locale),
            })
        })
        .collect();
    json!({
        "options": options,
        "defaultOptions": options,
        "defaultPreset": default,
    })
}

/// The host-supplied label. The English names match the vendored full-access
/// dictionary's default so that one row still localizes itself.
fn display_name(preset: &str, locale: Option<&str>) -> &'static str {
    if locale.is_some_and(|locale| locale.to_ascii_lowercase().starts_with("zh")) {
        return match preset {
            "agent" => "智能体",
            "plan" => "计划",
            FULL_ACCESS_PRESET => "完全权限",
            "coordinator" => "协调者",
            _ => "智能体",
        };
    }
    match preset {
        "agent" => "Agent",
        "plan" => "Plan",
        FULL_ACCESS_PRESET => "Full access",
        "coordinator" => "Coordinator",
        _ => "Agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_and_preset_round_trip() {
        assert_eq!(preset_of_mode("agent"), "agent");
        assert_eq!(preset_of_mode("plan"), "plan");
        assert_eq!(preset_of_mode("yolo"), FULL_ACCESS_PRESET);
        assert_eq!(preset_of_mode("coordinator"), "coordinator");
        assert_eq!(preset_of_mode("unknown"), DEFAULT_PRESET);
        for preset in PRESETS {
            let mode = mode_of_preset(preset).expect("catalog value maps to a mode");
            assert_eq!(preset_of_mode(mode), preset);
        }
        assert_eq!(mode_of_preset("yolo"), Some("yolo"));
        assert_eq!(mode_of_preset("read-only"), None);
    }

    #[test]
    fn catalog_default_is_always_advertised() {
        for default in ["agent", "plan", FULL_ACCESS_PRESET, "coordinator", "stale"] {
            let value = catalog(default, None);
            let options = value["defaultOptions"].as_array().unwrap();
            let advertised = options
                .iter()
                .any(|option| option["value"].as_str() == value["defaultPreset"].as_str());
            assert!(advertised, "default {default:?} must be advertised");
            assert_eq!(value["options"], value["defaultOptions"]);
        }
        assert_eq!(catalog("stale", None)["defaultPreset"], DEFAULT_PRESET);
        // The picker's full-access row keys its risk confirmation and
        // localized label on this value.
        let full = catalog("agent", None)["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|option| option["value"] == FULL_ACCESS_PRESET)
            .cloned()
            .expect("full-access option");
        assert_eq!(full["name"], "Full access");
        // The language preference localizes the CodeSmith-specific rows; an
        // unsupported language keeps the English labels.
        let zh = catalog("agent", Some("zh-CN"));
        assert_eq!(zh["options"][0]["name"], "智能体");
        assert_eq!(zh["options"][2]["name"], "完全权限");
        assert_eq!(catalog("agent", Some("fr"))["options"][0]["name"], "Agent");
    }
}
