//! Capability composition point — the model-visible tool catalog snapshot.
//!
//! `HostServices::build_turn_dispatcher` compiles the per-turn catalog from
//! several selection sources (mode gate, capability-manifest disabled set,
//! `tools_always_load`, plugin overrides, turn-scoped
//! `allowed_tools`/`blocked_tools`) that otherwise meet only implicitly.
//! This module makes the compiled result a first-class artifact: the
//! snapshot records what is visible, where each tool came from, and what
//! the capability manifest disabled; the diff against the previous
//! main-turn baseline drives the `tools-change` extension event, and
//! `/tools` renders it.
//!
//! Known limitations: there is no script-side API to read the current
//! catalog (the event carries the diff only); the first dispatcher build
//! establishes the baseline silently (no event); sub-agent toolsets build
//! on a separate path (`spawn_subagent`) and never diff or emit.

use std::sync::{Arc, Mutex};

/// Where a catalog tool came from. Classification precedence mirrors the
/// registry's name-collision rule: plugin > extension > mcp > builtin (a
/// later registration under a builtin's name replaces it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolOrigin {
    Builtin,
    Plugin,
    Extension,
    Mcp,
}

impl ToolOrigin {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ToolOrigin::Builtin => "builtin",
            ToolOrigin::Plugin => "plugin",
            ToolOrigin::Extension => "extension",
            ToolOrigin::Mcp => "mcp",
        }
    }
}

/// The compiled per-turn catalog: `(name, origin)` in catalog order, plus
/// the capability manifest's disabled names (as configured — they are
/// absent from `entries` by construction; unknown names are recorded
/// anyway, they are documented no-ops).
#[derive(Debug, Clone, Default)]
pub struct ToolCatalogSnapshot {
    pub entries: Vec<(String, ToolOrigin)>,
    pub disabled: Vec<String>,
}

impl ToolCatalogSnapshot {
    /// Classify the final post-selection catalog `names`. The three foreign
    /// sets are the same-name sources the dispatcher built from; anything
    /// unclaimed is builtin. `disabled` is the manifest's configured set
    /// (sorted for a stable render), recorded for the `/tools` readout.
    #[must_use]
    pub fn capture(
        names: &[String],
        plugin: &std::collections::HashSet<String>,
        extension: &std::collections::HashSet<String>,
        mcp: &std::collections::HashSet<String>,
        disabled: &std::collections::HashSet<String>,
    ) -> Self {
        // The struct doc promises disabled names are absent from `entries`;
        // enforce it here rather than trusting every composition site — an
        // overlap would render a tool as both available and disabled in the
        // model-visible `/tools` readout.
        let entries = names
            .iter()
            .filter(|name| {
                let overlap = disabled.contains(*name);
                if overlap {
                    tracing::warn!(
                        tool = %name,
                        "tool is both in the catalog and capability-disabled; dropping it from the catalog"
                    );
                }
                !overlap
            })
            .map(|name| {
                debug_assert!(
                    [plugin, extension, mcp]
                        .iter()
                        .filter(|s| s.contains(name))
                        .count()
                        <= 1,
                    "ambiguous origin for {name}: catalog precedence may diverge from \
                     the registry's later-registration-replaces rule"
                );
                let origin = if plugin.contains(name) {
                    ToolOrigin::Plugin
                } else if extension.contains(name) {
                    ToolOrigin::Extension
                } else if mcp.contains(name) {
                    ToolOrigin::Mcp
                } else {
                    ToolOrigin::Builtin
                };
                (name.clone(), origin)
            })
            .collect();
        let mut disabled = disabled.iter().cloned().collect::<Vec<_>>();
        disabled.sort();
        Self { entries, disabled }
    }

    /// Human rendering for `/tools`: grouped by origin, catalog order kept,
    /// plus the manifest-disabled names (absent from the catalog by
    /// construction).
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("Tool catalog ({} tools)\n", self.entries.len());
        for origin in [
            ToolOrigin::Builtin,
            ToolOrigin::Plugin,
            ToolOrigin::Extension,
            ToolOrigin::Mcp,
        ] {
            let names: Vec<&str> = self
                .entries
                .iter()
                .filter(|(_, o)| *o == origin)
                .map(|(n, _)| n.as_str())
                .collect();
            if names.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "\n── {} ({})\n  {}\n",
                origin.as_str(),
                names.len(),
                names.join(", ")
            ));
        }
        if !self.disabled.is_empty() {
            out.push_str(&format!(
                "\n── disabled via capabilities.toml ({})\n  {}\n",
                self.disabled.len(),
                self.disabled.join(", ")
            ));
        }
        out
    }
}

/// The baseline the dispatcher diffs against. Written once per main turn at
/// dispatch build; read by `/tools`. Shared as an `Arc` across
/// `EngineHost`/`EngineHandle`/`App` (the `extension_runner` pattern).
#[derive(Debug, Default)]
pub struct ToolCatalogState {
    inner: Mutex<Option<ToolCatalogSnapshot>>,
}

pub type SharedToolCatalog = Arc<ToolCatalogState>;

impl ToolCatalogState {
    /// Swap in `snapshot`, returning the name diff against the previous
    /// baseline (`added`, `removed` — sorted) when it changed. A missing
    /// baseline (first build) only records: no diff, no event.
    ///
    /// The returned diff is the sole trigger for the `tools-change`
    /// extension event, so ignoring it silently drops the event — hence
    /// `#[must_use]` (matching `current`).
    #[must_use]
    pub fn update(&self, snapshot: ToolCatalogSnapshot) -> Option<(Vec<String>, Vec<String>)> {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let diff = guard.as_ref().and_then(|prev| diff_names(prev, &snapshot));
        *guard = Some(snapshot);
        diff
    }

    /// The last recorded baseline (`None` before the first turn dispatch).
    #[must_use]
    pub fn current(&self) -> Option<ToolCatalogSnapshot> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn diff_names(
    prev: &ToolCatalogSnapshot,
    next: &ToolCatalogSnapshot,
) -> Option<(Vec<String>, Vec<String>)> {
    // Names only: the `tools-change` payload carries no origin, so an
    // origin-only relabel (plugin drops `x`, an MCP server picks `x` up)
    // must not surface as removed+added under the same name — consumers
    // would invalidate caches for what is a no-op in the visible tool set.
    let prev_set: std::collections::HashSet<&str> =
        prev.entries.iter().map(|(n, _)| n.as_str()).collect();
    let next_set: std::collections::HashSet<&str> =
        next.entries.iter().map(|(n, _)| n.as_str()).collect();
    let mut added: Vec<String> = next_set
        .difference(&prev_set)
        .map(|n| (*n).to_string())
        .collect();
    let mut removed: Vec<String> = prev_set
        .difference(&next_set)
        .map(|n| (*n).to_string())
        .collect();
    if added.is_empty() && removed.is_empty() {
        return None;
    }
    added.sort();
    removed.sort();
    Some((added, removed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(names: &[&str]) -> ToolCatalogSnapshot {
        let (plugin, extension, mcp, disabled) = (
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
        );
        ToolCatalogSnapshot::capture(
            &names.iter().map(|n| n.to_string()).collect::<Vec<_>>(),
            &plugin,
            &extension,
            &mcp,
            &disabled,
        )
    }

    #[test]
    fn capture_classifies_by_origin_precedence() {
        let names = ["read_file", "myplug", "call_count", "mcp_foo"]
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>();
        let plugin: std::collections::HashSet<String> =
            ["myplug"].iter().map(|n| n.to_string()).collect();
        let extension: std::collections::HashSet<String> =
            ["call_count"].iter().map(|n| n.to_string()).collect();
        let mcp: std::collections::HashSet<String> =
            ["mcp_foo"].iter().map(|n| n.to_string()).collect();
        let s =
            ToolCatalogSnapshot::capture(&names, &plugin, &extension, &mcp, &Default::default());
        assert_eq!(
            s.entries,
            vec![
                ("read_file".into(), ToolOrigin::Builtin),
                ("myplug".into(), ToolOrigin::Plugin),
                ("call_count".into(), ToolOrigin::Extension),
                ("mcp_foo".into(), ToolOrigin::Mcp),
            ]
        );
    }

    #[test]
    fn update_first_build_is_silent_then_diffs_then_stable() {
        let state = ToolCatalogState::default();
        assert!(
            state.update(snap(&["read_file", "edit_file"])).is_none(),
            "first build only establishes the baseline"
        );
        assert_eq!(
            state.update(snap(&["read_file", "call_count"])),
            Some((
                vec!["call_count".to_string()],
                vec!["edit_file".to_string()]
            )),
            "change diffs both sides, sorted"
        );
        assert!(
            state.update(snap(&["read_file", "call_count"])).is_none(),
            "identical catalog emits nothing"
        );
        assert_eq!(state.current().unwrap().entries.len(), 2);
    }

    #[test]
    fn update_ignores_origin_only_relabel() {
        let state = ToolCatalogState::default();
        let names = vec!["mytool".to_string()];
        let empty: std::collections::HashSet<String> = Default::default();
        let plugin: std::collections::HashSet<String> =
            ["mytool"].iter().map(|n| n.to_string()).collect();
        // First build only establishes the baseline; the (absent) diff is
        // intentionally dropped — `#[must_use]` wants that said out loud.
        let _ = state.update(ToolCatalogSnapshot::capture(
            &names, &empty, &empty, &empty, &empty,
        ));
        assert_eq!(
            state.update(ToolCatalogSnapshot::capture(
                &names, &plugin, &empty, &empty, &empty
            )),
            None,
            "an origin-only relabel leaves the visible tool set unchanged"
        );
    }

    #[test]
    fn capture_drops_names_that_are_also_disabled() {
        let names = vec!["kept".to_string(), "gone".to_string()];
        let empty: std::collections::HashSet<String> = Default::default();
        let disabled: std::collections::HashSet<String> =
            ["gone"].iter().map(|n| n.to_string()).collect();
        let s = ToolCatalogSnapshot::capture(&names, &empty, &empty, &empty, &disabled);
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].0, "kept");
        assert_eq!(s.disabled, vec!["gone".to_string()]);
    }

    #[test]
    fn render_groups_by_origin_and_skips_empty() {
        let names = ["read_file", "call_count", "mcp_foo"]
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>();
        let extension: std::collections::HashSet<String> =
            ["call_count"].iter().map(|n| n.to_string()).collect();
        let mcp: std::collections::HashSet<String> =
            ["mcp_foo"].iter().map(|n| n.to_string()).collect();
        let disabled: std::collections::HashSet<String> = ["edit_file", "write_file"]
            .iter()
            .map(|n| n.to_string())
            .collect();
        let s =
            ToolCatalogSnapshot::capture(&names, &Default::default(), &extension, &mcp, &disabled);
        let rendered = s.render();
        assert!(rendered.contains("Tool catalog (3 tools)"));
        assert!(rendered.contains("── builtin (1)\n  read_file"));
        assert!(rendered.contains("── extension (1)\n  call_count"));
        assert!(rendered.contains("── mcp (1)\n  mcp_foo"));
        assert!(!rendered.contains("plugin"), "empty groups are skipped");
        assert!(
            rendered.contains("── disabled via capabilities.toml (2)\n  edit_file, write_file"),
            "disabled names render sorted in their own group:\n{rendered}"
        );
        assert_eq!(s.disabled, vec!["edit_file", "write_file"]);
    }
}
