//! The permission-preset surface — the vendored `client-ui-permission-presets`
//! plugin's host half.
//!
//! Upstream (DSH) models permissions as one sandbox + approval bundle chosen
//! per session; plan is a separate collaboration state and coordinator is not
//! a permission at all. CodeSmith still carries a single `thread.mode` ring
//! (`agent` / `plan` / `yolo` / `coordinator`), so the bridge projects that
//! mode onto the upstream vocabulary instead of inventing a second permission
//! system:
//!
//! * selectable presets are `workspace-write` (agent: workspace-write sandbox,
//!   approval ask) and `danger-full-access` (yolo: no sandbox, no approvals);
//! * plan is a separate collaboration state (its own composer chip). CodeSmith
//!   enforces read-only planning, so while plan is active the effective
//!   permission reads as the derived `read-only` value — never a selectable
//!   preset;
//! * coordinator is an experimental orchestrator-only tool surface. Its
//!   read-only + auto-approve knobs match no preset, so it reads as the
//!   derived `custom` value and stays out of the catalog.
//!
//! The vendored presentation layer localizes the three conventional values
//! (`read-only`, `workspace-write`, `danger-full-access`) by value when the
//! host passes the value as its name, so the bridge supplies no labels of its
//! own. The new-session default lives in the desktop settings document
//! (`permission.defaultPreset`); legacy values (`agent`, `yolo`, `plan`,
//! `coordinator`) normalize on read so a stored document cannot break the
//! settings row.

use serde_json::{json, Value};

/// The default preset: workspace-write sandbox + approval ask.
pub const DEFAULT_PRESET: &str = "workspace-write";

/// The catalog value that maps onto CodeSmith's `yolo` mode. The vendored
/// picker keys its localized label and explicit risk gate on this exact
/// string.
pub const FULL_ACCESS_PRESET: &str = "danger-full-access";

/// The derived read-only value shown while plan mode is active. It is not a
/// selectable preset: switching to it would mean entering plan mode, which is
/// the plan chip's job.
pub const READ_ONLY_PRESET: &str = "read-only";

/// The derived value for a session whose sandbox/approval knobs match no
/// configured preset (`coordinator`: read-only + auto-approve).
pub const CUSTOM_PRESET: &str = "custom";

/// Every selectable catalog option, in display order.
pub const PRESETS: [&str; 2] = [DEFAULT_PRESET, FULL_ACCESS_PRESET];

/// Normalize a stored or requested preset name. Legacy CodeSmith spellings
/// (`agent`, `yolo`) and the retired mode-shaped values (`plan`,
/// `coordinator`) collapse onto a selectable preset so the settings row always
/// advertises its current default.
pub fn normalize_preset(preset: &str) -> &'static str {
    match preset.trim().to_ascii_lowercase().as_str() {
        "danger-full-access" | "yolo" => FULL_ACCESS_PRESET,
        _ => DEFAULT_PRESET,
    }
}

/// The catalog value for one thread mode. `plan`/`coordinator` are derived
/// values outside the catalog; unknown modes read as the default, keeping
/// `permissions.currentValue` a value the client can render.
pub fn preset_of_mode(mode: &str) -> &'static str {
    match mode {
        "plan" => READ_ONLY_PRESET,
        "yolo" => FULL_ACCESS_PRESET,
        "coordinator" => CUSTOM_PRESET,
        _ => DEFAULT_PRESET,
    }
}

/// The thread mode for one selectable catalog value. Legacy `agent`/`yolo`
/// spellings are accepted too, so a hand-typed `/permission yolo` keeps
/// working. `None` for anything else, including the derived values.
pub fn mode_of_preset(preset: &str) -> Option<&'static str> {
    match preset.trim().to_ascii_lowercase().as_str() {
        "workspace-write" | "agent" => Some("agent"),
        "danger-full-access" | "yolo" => Some("yolo"),
        _ => None,
    }
}

/// `permissionPresets/catalog` — the process catalog the picker and the
/// General settings row read. `defaultPreset` is normalized so the settings
/// row's "current default" lookup cannot throw on a hand-edited document.
pub fn catalog(default_preset: &str) -> Value {
    let default = normalize_preset(default_preset);
    let options: Vec<Value> = PRESETS
        .iter()
        .map(|preset| {
            json!({
                "value": preset,
                // The value doubles as the name so the vendored locale
                // dictionaries localize the three conventional rows.
                "name": preset,
            })
        })
        .collect();
    json!({
        "options": options.clone(),
        "defaultOptions": options,
        "defaultPreset": default,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_and_preset_round_trip() {
        assert_eq!(preset_of_mode("agent"), DEFAULT_PRESET);
        assert_eq!(preset_of_mode("yolo"), FULL_ACCESS_PRESET);
        assert_eq!(preset_of_mode("plan"), READ_ONLY_PRESET);
        assert_eq!(preset_of_mode("coordinator"), CUSTOM_PRESET);
        assert_eq!(preset_of_mode("unknown"), DEFAULT_PRESET);
        for preset in PRESETS {
            let mode = mode_of_preset(preset).expect("catalog value maps to a mode");
            assert_eq!(preset_of_mode(mode), preset);
        }
        // The retired mode-shaped spellings are not permissions.
        assert_eq!(mode_of_preset("plan"), None);
        assert_eq!(mode_of_preset("coordinator"), None);
        assert_eq!(mode_of_preset("read-only"), None);
        // Legacy spellings still switch, so an old muscle memory keeps working.
        assert_eq!(mode_of_preset("agent"), Some("agent"));
        assert_eq!(mode_of_preset("yolo"), Some("yolo"));
    }

    #[test]
    fn legacy_defaults_normalize() {
        assert_eq!(normalize_preset("agent"), DEFAULT_PRESET);
        assert_eq!(normalize_preset("yolo"), FULL_ACCESS_PRESET);
        assert_eq!(normalize_preset("plan"), DEFAULT_PRESET);
        assert_eq!(normalize_preset("coordinator"), DEFAULT_PRESET);
        assert_eq!(normalize_preset("danger-full-access"), FULL_ACCESS_PRESET);
        assert_eq!(normalize_preset("stale"), DEFAULT_PRESET);
    }

    #[test]
    fn catalog_default_is_always_advertised() {
        for default in [
            "workspace-write",
            FULL_ACCESS_PRESET,
            "agent",
            "plan",
            "stale",
        ] {
            let value = catalog(default);
            let options = value["defaultOptions"].as_array().unwrap();
            let advertised = options
                .iter()
                .any(|option| option["value"].as_str() == value["defaultPreset"].as_str());
            assert!(advertised, "default {default:?} must be advertised");
            assert_eq!(value["options"], value["defaultOptions"]);
        }
        assert_eq!(catalog("agent")["defaultPreset"], DEFAULT_PRESET);
        assert_eq!(catalog("yolo")["defaultPreset"], FULL_ACCESS_PRESET);
        let catalog_value = catalog("workspace-write");
        let values: Vec<&str> = catalog_value["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|option| option["value"].as_str().unwrap())
            .collect();
        assert_eq!(values, vec!["workspace-write", "danger-full-access"]);
        // The vendored client localizes `name == value` from its own
        // dictionaries; the host must not ship its own labels.
        let names: Vec<&str> = catalog_value["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|option| option["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, values);
    }
}
