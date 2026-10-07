//! Shared Mods operations — the single implementation behind both the
//! `/mods` slash commands and the model-visible `manage_mods` tool
//! (§F script mod layer Phase B8/C9/C10).
//!
//! Two invariants live here:
//!
//! - **First-activation consent** (plan §五): `activate` is the only path
//!   that records consent; discovery-side gating happens in
//!   `populate_extension_runtime`. This layer never loads a mod without it.
//! - **File-backed state as source of truth**: mutating ops load a fresh
//!   [`ModStateStore`] from disk, mutate, persist — so the slash-command
//!   writer (App-owned store) and the tool writer (disk-fresh store) can
//!   never diverge. [`reload_mods`] likewise re-loads both stores from disk.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use codesmith_extensions::{
    DiscoveredMod, ExtensionRunner, ModKvStore, RhaiMod, apply_mod_trust_gate, discover_mods,
};

use crate::mod_state::ModStateStore;

/// Mods roots for a workspace: global `~/.codesmith/mods` + project
/// `<workspace>/.codesmith/mods`.
pub struct ModPaths {
    pub global_root: Option<PathBuf>,
    pub project_root: PathBuf,
}

pub fn mod_paths(workspace: &Path) -> ModPaths {
    ModPaths {
        global_root: crate::config::effective_home_dir()
            .map(|home| home.join(".codesmith").join("mods")),
        project_root: workspace.join(".codesmith").join("mods"),
    }
}

/// Trust-gated discovery for a workspace (project mods dropped when the
/// workspace is untrusted — same semantics as dylib discovery).
pub fn discover_workspace_mods(workspace: &Path) -> Vec<DiscoveredMod> {
    let paths = mod_paths(workspace);
    let global_roots: Vec<PathBuf> = paths.global_root.into_iter().collect();
    let project_roots = vec![paths.project_root];
    let trusted = crate::config::is_workspace_trusted(workspace);
    apply_mod_trust_gate(discover_mods(&global_roots, &project_roots), !trusted)
}

/// A discovered-but-not-activated mod — the passive "pending" notice.
/// The full manifest identity rides along so UI surfaces (and future
/// interactive approval dialogs) can show more than the id without
/// re-reading `mod.toml`.
#[derive(Debug, Clone)]
#[allow(dead_code)] // name/description: report surface for future UI; id/version/source in use
pub struct PendingModInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    /// `"global"` / `"project"` — which root it came from.
    pub source: &'static str,
    /// Why the mod is pending: `None` = awaits first activation;
    /// `Some(reason)` = previously activated but the entry file's content
    /// changed since the recorded consent (re-activation required).
    pub reason: Option<&'static str>,
}

impl PendingModInfo {
    pub fn from_discovered(m: &DiscoveredMod) -> Self {
        Self {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            description: m.description.clone(),
            source: if m.global { "global" } else { "project" },
            reason: None,
        }
    }

    /// A previously-activated mod whose entry-file content no longer matches
    /// the recorded activation hash — consent does not cover the new content.
    pub fn changed_content(m: &DiscoveredMod) -> Self {
        Self {
            reason: Some("entry file changed since activation — /mods activate to re-consent"),
            ..Self::from_discovered(m)
        }
    }
}

/// Reload context captured at engine build for the `manage_mods` tool
/// (runner + workspace + the engine's shared cancel token — everything
/// [`reload_mods`] needs). `None` for embeds/tests that skip the mod layer.
#[derive(Clone)]
pub struct ModReloadCtx {
    pub runner: Arc<ExtensionRunner>,
    pub workspace: PathBuf,
    pub shared_cancel_token: Arc<StdMutex<CancellationToken>>,
    /// Captured at engine build (the same flag that gates registration) —
    /// a tool-triggered or watcher reload must not resurrect the mod layer
    /// against `[mods] enabled = false`.
    pub mods_enabled: bool,
}

impl std::fmt::Debug for ModReloadCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModReloadCtx")
            .field("workspace", &self.workspace)
            .field("runner_generation", &self.runner.generation())
            .finish()
    }
}

// === Read-only listings =====================================================

/// `/mods list` — every discovered mod with its activation state.
pub fn list_mods(workspace: &Path, state: &ModStateStore) -> String {
    let mods = discover_workspace_mods(workspace);
    if mods.is_empty() {
        return "No script mods discovered. Install one under ~/.codesmith/mods/<id>/ or <workspace>/.codesmith/mods/<id>/ (mod.toml + mod.rhai), or ask the agent to write one via manage_mods.".to_string();
    }
    let mut out = String::from("Script mods:\n");
    for m in &mods {
        let status = if state.is_disabled(&m.id) {
            "disabled"
        } else if state.is_activated(&m.id) {
            "activated"
        } else {
            "pending activation"
        };
        out.push_str(&format!(
            "  {} (v{}) [{}] — {}\n",
            m.id,
            m.version,
            if m.global { "global" } else { "project" },
            status
        ));
    }
    out.push_str("\nActivation is one-time per id (/mods activate <id> or approve manage_mods action=activate).");
    out
}

/// `/mods status` — runner generation + activated/pending/disabled split.
pub fn mod_status(workspace: &Path, state: &ModStateStore) -> String {
    let mods = discover_workspace_mods(workspace);
    let activated: Vec<&str> = mods
        .iter()
        .filter(|m| state.should_load(&m.id))
        .map(|m| m.id.as_str())
        .collect();
    let pending: Vec<&str> = mods
        .iter()
        .filter(|m| !state.is_activated(&m.id))
        .map(|m| m.id.as_str())
        .collect();
    let disabled = state.disabled();
    let activated_ids = state.activated();
    format!(
        "Mod state: {} discovered, {} loaded, {} pending, {} disabled.\nloaded: {}\npending: {}\ndisabled: {}\nactivated ids on record: {}",
        mods.len(),
        activated.len(),
        pending.len(),
        disabled.len(),
        if activated.is_empty() {
            "(none)"
        } else {
            &activated.join(", ")
        },
        if pending.is_empty() {
            "(none)"
        } else {
            &pending.join(", ")
        },
        if disabled.is_empty() {
            "(none)"
        } else {
            &disabled.join(", ")
        },
        if activated_ids.is_empty() {
            "(none)"
        } else {
            &activated_ids.join(", ")
        },
    )
}

/// `/mods info <id>` — manifest detail for one mod.
pub fn mod_info(workspace: &Path, state: &ModStateStore, id: &str) -> Option<String> {
    let mods = discover_workspace_mods(workspace);
    let m = mods.into_iter().find(|m| m.id == id)?;
    let status = if state.is_disabled(&m.id) {
        "disabled"
    } else if state.is_activated(&m.id) {
        "activated"
    } else {
        "pending activation"
    };
    Some(format!(
        "id: {}\nname: {}\nversion: {}\nsource: {}\ndir: {}\nentry: {}\ndescription: {}\nstate: {}",
        m.id,
        m.name,
        m.version,
        if m.global { "global" } else { "project" },
        m.dir.display(),
        m.entry_path.display(),
        m.description.as_deref().unwrap_or("(none)"),
        status,
    ))
}

// === Mutating operations ====================================================

/// Serializes mod-state load→mutate→persist across every writer in the
/// process: the `/mods` command path, the `manage_mods` tool path, and the
/// activation-hash backfill during reload. Each persist rewrites the whole
/// file, so two unlocked racing mutations are last-writer-wins — one record
/// silently disappears. Take this guard BEFORE `ModStateStore::load_default`
/// and hold it across mutate + persist.
///
/// Constraint: the reload path (`reload_mods` → `reload_extension_runtime` →
/// `populate_extension_runtime`) must NOT hold this guard across populate —
/// the backfill acquires it, and `std::sync::Mutex` is not reentrant. The
/// reload path's own store load therefore stays lock-free; the backfill
/// re-loads fresh state under the guard before recording, so that lock-free
/// snapshot is never persisted back over a concurrent writer.
pub fn mod_state_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    match LOCK.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// SHA-256 hex of a mod's entry file — the content the activation consent
/// is bound to. Known limitation: companion files beside the entry are not
/// hashed (the entry script is the executable unit mods load).
pub(crate) fn entry_file_hash(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Activate a mod (first-activation consent, bound to the entry file's
/// current content) — persisted, then the mod layer reloads so the mod's
/// tools/commands/hooks are live immediately.
pub fn activate(workspace: &Path, state: &mut ModStateStore, id: &str) -> Result<String, String> {
    let mods = discover_workspace_mods(workspace);
    let m = mods
        .into_iter()
        .find(|m| m.id == id)
        .ok_or_else(|| format!("No mod with id '{id}'. Run /mods list."))?;
    let entry_hash = entry_file_hash(&m.entry_path)?;
    state
        .activate(id, &entry_hash)
        .map_err(|e| format!("persist activation: {e}"))?;
    Ok(format!(
        "Activated mod '{}' (v{}, {}). One-time consent recorded for this \
         content — same-id reloads need no further approval, but a changed \
         entry file asks for activation again.",
        m.id,
        m.version,
        m.dir.display()
    ))
}

/// Enable/disable an already-known mod id (no fresh consent either way).
pub fn set_enabled(state: &mut ModStateStore, id: &str, enabled: bool) -> Result<String, String> {
    if !state.is_activated(id) {
        return Err(format!(
            "Mod '{id}' is not activated; activate it first (/mods activate {id})."
        ));
    }
    state
        .set_enabled(id, enabled)
        .map_err(|e| format!("persist enablement: {e}"))?;
    Ok(if enabled {
        format!("Enabled mod '{id}' (takes effect on the next mods reload).")
    } else {
        format!("Disabled mod '{id}' (takes effect on the next mods reload).")
    })
}

/// Remove a mod: delete its directory + KV file + all state records.
pub fn remove(workspace: &Path, state: &mut ModStateStore, id: &str) -> Result<String, String> {
    let mods = discover_workspace_mods(workspace);
    let m = mods
        .into_iter()
        .find(|m| m.id == id)
        .ok_or_else(|| format!("No mod with id '{id}' on disk."))?;
    std::fs::remove_dir_all(&m.dir).map_err(|e| format!("remove {}: {e}", m.dir.display()))?;
    if let Some(kv_dir) = state.kv_dir() {
        let scope = if m.global { "global" } else { "project" };
        let _ = std::fs::remove_file(kv_dir.join(format!("{scope}-{}.json", m.id)));
    }
    state
        .deactivate(id)
        .map_err(|e| format!("clear state: {e}"))?;
    Ok(format!(
        "Removed mod '{id}' ({}). Bindings clear on the next mods reload.",
        m.dir.display()
    ))
}

/// Sanitize a mod-author-supplied relative path: reject absolute paths and
/// any `..` component (same guard class as mod.toml `entry`). Rooted paths
/// (`/abs`) and drive-prefix paths (`C:x`) are rejected too — on Windows
/// neither `is_absolute` alone catches them.
pub fn sanitize_rel_path(rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if p.has_root()
        || p.components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
        || p.components().count() == 0
    {
        return Err(format!(
            "path {rel:?} must be relative and stay inside the mod directory (no absolute paths, no '..')"
        ));
    }
    Ok(p.to_path_buf())
}

/// Write mod files (`mod.toml` + `mod.rhai` + any companions) into
/// `<mods-root>/<id>/`. Creates the directory. Does NOT activate — the
/// approval-gated `activate` step stays separate by design.
pub fn write_mod(
    workspace: &Path,
    id: &str,
    files: &[(String, String)],
    global: bool,
) -> Result<PathBuf, String> {
    // Canonical helper (also rejects leading/trailing dots: ".git"-style
    // hidden names, and Windows trailing-dot aliasing) — same guard class
    // the automation/background-task ids use.
    if !codesmith_agent_runtime::utils::is_safe_path_component(id) {
        return Err(format!(
            "mod id {id:?} must be [a-zA-Z0-9._-], non-empty, and must not start or end with '.'"
        ));
    }
    if files.is_empty() {
        return Err("write requires a non-empty `files` list".to_string());
    }
    if !files.iter().any(|(p, _)| p == "mod.toml") {
        return Err("write requires a mod.toml entry in `files`".to_string());
    }
    let paths = mod_paths(workspace);
    let root = match (global, &paths.global_root) {
        (true, Some(g)) => g.clone(),
        (true, None) => {
            return Err("no home directory available for the global mods root".to_string());
        }
        (false, _) => paths.project_root,
    };
    // Trust gate: an untrusted workspace must not persist mod code. A
    // project write would create a discoverable-but-shadowed mod; a global
    // write would put executable code into `~/.codesmith/mods`, which loads
    // in EVERY workspace — including other untrusted ones
    // (`apply_mod_trust_gate` retains globals). Refuse both so the trust
    // signal cannot be bypassed via the `global` flag; per-call approval
    // and first-activation consent remain additional gates, not
    // replacements for it.
    // Trust gate: an untrusted workspace must not persist mod code. A
    // project write would create a discoverable-but-shadowed mod; a global
    // write would put executable code into `~/.codesmith/mods`, which loads
    // in EVERY workspace — including other untrusted ones
    // (`apply_mod_trust_gate` retains globals). Refuse both so the trust
    // signal cannot be bypassed via the `global` flag; per-call approval
    // and first-activation consent remain additional gates, not
    // replacements for it.
    if !crate::config::is_workspace_trusted(workspace) {
        return Err(
            "workspace is not trusted; project and global mod writes are refused".to_string(),
        );
    }
    let mod_dir = root.join(id);
    for (rel, _content) in files {
        sanitize_rel_path(rel)?;
    }
    std::fs::create_dir_all(&mod_dir).map_err(|e| format!("create {}: {e}", mod_dir.display()))?;
    // `sanitize_rel_path` is purely lexical, and `create_dir_all` /
    // `fs::write` follow symlinks. Anchor containment on the canonicalized
    // mods ROOT — anchoring on `mod_dir.canonicalize()` alone is the hole:
    // a pre-planted symlink at `<mods-root>/<id>` leaves `create_dir_all`
    // successful while the canonicalized mod dir already resolves to the
    // attacker-chosen target, so every check anchored there passes while
    // the approved write lands outside the root. Known limitation: the
    // check-then-write gap is still a TOCTOU window (a symlink swapped in
    // between check and write wins); closing that needs O_NOFOLLOW-style
    // opens, which std does not expose.
    // `sanitize_rel_path` is purely lexical, and `create_dir_all` /
    // `fs::write` follow symlinks. Anchor containment on the canonicalized
    // mods ROOT — anchoring on `mod_dir.canonicalize()` alone is the hole:
    // a pre-planted symlink at `<mods-root>/<id>` leaves `create_dir_all`
    // successful while the canonicalized mod dir already resolves to the
    // attacker-chosen target, so every check anchored there passes while
    // the approved write lands outside the root. Known limitation: the
    // check-then-write gap is still a TOCTOU window (a symlink swapped in
    // between check and write wins); closing that needs O_NOFOLLOW-style
    // opens, which std does not expose.
    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("resolve {}: {e}", root.display()))?;
    let mod_dir_canon = mod_dir
        .canonicalize()
        .map_err(|e| format!("resolve {}: {e}", mod_dir.display()))?;
    if !mod_dir_canon.starts_with(&root_canon) {
        return Err(format!(
            "mod dir {} resolves outside the mods root (symlink?) — refusing to write",
            mod_dir.display()
        ));
    }
    for (rel, content) in files {
        let target = mod_dir.join(sanitize_rel_path(rel)?);
        // Final-component symlinks are invisible to the parent check below
        // (`fs::write` follows them) — reject outright.
        if std::fs::symlink_metadata(&target)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(format!(
                "path {rel:?} is a symlink — refusing to write through it"
            ));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let parent_canon = target
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .ok_or_else(|| format!("path {rel:?} escapes the mod directory (symlink?)"))?;
        if !parent_canon.starts_with(&mod_dir_canon) {
            return Err(format!(
                "path {rel:?} escapes the mod directory (symlink?) — refusing to write"
            ));
        }
        std::fs::write(&target, content).map_err(|e| format!("write {}: {e}", target.display()))?;
    }
    Ok(mod_dir)
}

// === Reload =================================================================

/// Reload the whole extension layer (compiled-in + dylib + script mods) from
/// disk-fresh state. Safe from any thread; idempotent
/// (clear → invalidate → populate).
#[allow(clippy::too_many_arguments)]
pub fn reload_mods(
    runner: &Arc<ExtensionRunner>,
    workspace: &Path,
    shared_cancel_token: Arc<StdMutex<CancellationToken>>,
    mods_enabled: bool,
) -> String {
    // Disk-fresh stores: the tool path may have mutated them since the App
    // copies were loaded. An unreadable store must NOT silently become an
    // empty one — `reload_extension_runtime` clears every binding and
    // repopulates from these stores, so an empty read would unload every
    // activated mod AND re-enable user-disabled extensions until the next
    // successful reload. Abort instead; current bindings stay live.
    let ext_state = match crate::extension_state::ExtensionStateStore::load_default() {
        Ok(s) => s,
        Err(e) => {
            return format!(
                "Reload aborted: extension state unreadable ({e}); current bindings kept."
            );
        }
    };
    let mod_state = match ModStateStore::load_default() {
        Ok(s) => s,
        Err(e) => {
            return format!("Reload aborted: mod state unreadable ({e}); current bindings kept.");
        }
    };
    let gen_before = runner.generation();
    let report = crate::core::engine::reload_extension_runtime(
        runner,
        workspace,
        &ext_state,
        &mod_state,
        mods_enabled,
        shared_cancel_token,
    );
    let mut msg = format!(
        "Extension layer reloaded (generation {gen_before} → {}). {} mod(s) loaded.",
        runner.generation(),
        report.loaded_mods
    );
    if !report.pending_mods.is_empty() {
        let ids: Vec<String> = report
            .pending_mods
            .iter()
            .map(|p| match p.reason {
                None => p.id.clone(),
                Some(reason) => format!("{} ({reason})", p.id),
            })
            .collect();
        msg.push_str(&format!(
            " Pending activation (one-time consent): {} — /mods activate <id>.",
            ids.join(", ")
        ));
    }
    // Discipline 5 — surface failed loads in the reload message too (the
    // audit keeps the original errors; this is the first-line digest).
    let failed = crate::core::engine::failed_audit_lines(&report.audit);
    if !failed.is_empty() {
        msg.push_str(&format!(" Failed: {}.", failed.join("; ")));
    }
    msg
}

// === Watcher (Phase B7) =====================================================

/// Debounce window after the last relevant file event before reloading.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(500);
/// Minimum spacing between two reloads (races with a manual reload resolve
/// as two idempotent reloads).
const WATCH_COOLDOWN: Duration = Duration::from_secs(1);

fn event_is_relevant(ev: &notify::Event) -> bool {
    ev.paths.iter().any(|p| {
        matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("rhai") | Some("toml")
        )
    })
}

/// Watch both mods roots (`.rhai`/`.toml`, recursive) and reload the
/// extension layer on changes — debounce 500ms + cooldown 1s. Runs until
/// the process exits; a failing watcher logs once and disables itself
/// (hot-reload is best-effort, never fatal).
///
/// Must be called from a tokio runtime context (spawns the watch task).
/// `mods_enabled` is captured at spawn (the same flag that gates the
/// watcher itself) so a reload can never resurrect the mod layer against
/// the user's `[mods] enabled = false`.
pub fn spawn_mods_watcher(
    runner: Arc<ExtensionRunner>,
    workspace: PathBuf,
    shared_cancel_token: Arc<StdMutex<CancellationToken>>,
    mods_enabled: bool,
) {
    let paths = mod_paths(&workspace);
    let roots: Vec<PathBuf> = paths
        .global_root
        .into_iter()
        .chain([paths.project_root])
        .collect();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<notify::Result<notify::Event>>(256);
    let watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = tx.blocking_send(res);
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(target: "codesmith_mods", "mods watcher unavailable: {e}");
            return;
        }
    };
    use notify::Watcher as _;
    let mut watcher = watcher;
    let mut watched_any = false;
    for root in &roots {
        // Create the root so `watch` succeeds for first-run machines; a
        // missing parent (no home dir) just skips that root.
        let _ = std::fs::create_dir_all(root);
        match watcher.watch(root, notify::RecursiveMode::Recursive) {
            Ok(()) => watched_any = true,
            Err(e) => {
                tracing::warn!(target: "codesmith_mods", "mods watcher skip {}: {e}", root.display())
            }
        }
    }
    if !watched_any {
        return;
    }

    tokio::spawn(async move {
        // Keep the watcher alive for the task's lifetime.
        let _watcher = watcher;
        let mut last_fire = Instant::now()
            .checked_sub(WATCH_COOLDOWN)
            .unwrap_or_else(Instant::now);
        while let Some(ev) = rx.recv().await {
            let Ok(ev) = ev else { continue };
            if !event_is_relevant(&ev) {
                continue;
            }
            // Debounce: wait out the quiet window, discarding further events.
            let deadline = tokio::time::Instant::now() + WATCH_DEBOUNCE;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    ev2 = rx.recv() => {
                        match ev2 {
                            Some(Ok(e2)) if event_is_relevant(&e2) => {}
                            Some(_) => {}
                            None => return,
                        }
                    }
                }
            }
            // Cooldown vs the last fire (manual reload included, best-effort).
            let elapsed = last_fire.elapsed();
            if elapsed < WATCH_COOLDOWN {
                tokio::time::sleep(WATCH_COOLDOWN - elapsed).await;
            }
            last_fire = Instant::now();
            // reload_mods is synchronous disk work (recursive scans, TOML
            // parsing, Rhai compilation on the load thread) — keep it off
            // the tokio worker so a reload doesn't stall other tasks.
            let msg = {
                let runner = Arc::clone(&runner);
                let workspace = workspace.clone();
                let token = shared_cancel_token.clone();
                tokio::task::spawn_blocking(move || {
                    reload_mods(&runner, &workspace, token, mods_enabled)
                })
                .await
                .unwrap_or_else(|e| format!("mods watcher reload failed: {e}"))
            };
            tracing::info!(target: "codesmith_mods", "watcher reload: {msg}");
        }
    });
}

/// Load one discovered mod — used by `populate_extension_runtime` and tests.
/// `kv_dir` is the mods-state directory (from [`ModStateStore::kv_dir`]);
/// `hub` is the runner's message-projection hub (baked into the mod's
/// `projection_state` reads).
pub fn load_rhai_mod(
    m: &DiscoveredMod,
    kv_dir: Option<&Path>,
    hub: codesmith_agent::extension::MessageProjectionHubArc,
) -> Result<RhaiMod, String> {
    let kv = match kv_dir {
        Some(dir) => {
            let scope = if m.global { "global" } else { "project" };
            ModKvStore::new(dir.join(format!("{scope}-{}.json", m.id)))
        }
        // No state directory (path-less store): in-memory only — never
        // persist mod data into the shared temp dir.
        None => ModKvStore::ephemeral(),
    };
    RhaiMod::load(m, kv, hub).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// HOME-scoped env guard (the engine-tests `ScopedHome` pattern): the
    /// global mods root, the trust config, and the mods-state dir all resolve
    /// inside a tempdir, and the workspace is marked trusted so project-root
    /// discovery passes the real trust gate. Serialized with `lock_test_env`
    /// like every other env-mutating test in the crate.
    struct ScopedHome {
        previous: Option<std::ffi::OsString>,
    }

    impl ScopedHome {
        fn set_trusted(tmp: &TempDir, workspace: &Path) -> Self {
            let guard = Self::set(tmp);
            // Backslashes must be doubled inside a TOML basic-string key;
            // a raw Windows path (`D:\a\…`) would make `\a` an invalid
            // escape and the whole config file fails to parse, so the
            // workspace never comes back trusted.
            let key = workspace.display().to_string().replace('\\', "\\\\");
            let config = r#"[projects."{path}"]
trust_level = "trusted"
"#
            .replace("{path}", &key);
            std::fs::create_dir_all(tmp.path().join(".codesmith")).unwrap();
            std::fs::write(tmp.path().join(".codesmith").join("config.toml"), config).unwrap();
            guard
        }

        fn set(tmp: &TempDir) -> Self {
            let previous = std::env::var_os("HOME");
            // Safety: tests using this helper serialize with lock_test_env()
            // and restore the original value in Drop.
            unsafe {
                std::env::set_var("HOME", tmp.path());
            }
            Self { previous }
        }
    }

    impl Drop for ScopedHome {
        fn drop(&mut self) {
            // Safety: serialized with lock_test_env().
            unsafe {
                if let Some(previous) = self.previous.take() {
                    std::env::set_var("HOME", previous);
                } else {
                    std::env::remove_var("HOME");
                }
            }
        }
    }

    fn write_fixture_mod(root: &Path, id: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mod.toml"),
            format!("id = \"{id}\"\nversion = \"0.1.0\"\ndescription = \"test mod\"\n"),
        )
        .unwrap();
        std::fs::write(dir.join("mod.rhai"), "mod_log(\"loaded\");").unwrap();
        dir
    }

    /// A store whose kv_dir sits inside a tempdir (hermetic).
    fn store_in(dir: &TempDir) -> ModStateStore {
        ModStateStore::load_from(dir.path().join("mods_state.toml")).unwrap()
    }

    /// A workspace whose PROJECT mods root sits inside a tempdir. Discovery
    /// still scans the real global root; tests assert only on their own ids.
    fn workspace_in(dir: &TempDir) -> PathBuf {
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join(".codesmith").join("mods")).unwrap();
        ws
    }

    #[test]
    fn watcher_event_relevance_filters_extensions() {
        let mk = |paths: Vec<&str>| notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        };
        assert!(event_is_relevant(&mk(vec!["/m/mod.rhai"])));
        assert!(event_is_relevant(&mk(vec!["/m/mod.toml"])));
        assert!(event_is_relevant(&mk(vec!["/m/other.txt", "/m/x.rhai"])));
        assert!(!event_is_relevant(&mk(vec!["/m/README.md"])));
        assert!(!event_is_relevant(&mk(vec![])));
    }

    #[test]
    fn sanitize_rel_path_rejects_escape_attempts() {
        assert!(sanitize_rel_path("../escape").is_err());
        assert!(sanitize_rel_path("/abs").is_err());
        assert!(sanitize_rel_path("a/../../b").is_err());
        assert!(sanitize_rel_path("").is_err());
        assert!(sanitize_rel_path("mod.rhai").is_ok());
        assert!(sanitize_rel_path("src/lib.rhai").is_ok());
    }

    #[test]
    fn write_mod_creates_files_and_requires_manifest() {
        let _env = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let _home = ScopedHome::set_trusted(&dir, &dir.path().join("ws"));
        let ws = workspace_in(&dir);
        let files = vec![
            (
                "mod.toml".to_string(),
                "id = \"w\"\nversion = \"0.1.0\"\n".to_string(),
            ),
            ("mod.rhai".to_string(), "mod_log(\"x\");".to_string()),
        ];
        let target = write_mod(&ws, "w", &files, false).unwrap();
        assert!(target.join("mod.toml").is_file());
        assert!(target.join("mod.rhai").is_file());

        let err = write_mod(&ws, "w2", &[("mod.rhai".into(), "x".into())], false);
        assert!(err.is_err());
        let msg = err.unwrap_err();
        assert!(msg.contains("mod.toml"), "{msg}");

        let err = write_mod(&ws, "bad id", &files, false);
        assert!(err.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn write_mod_refuses_symlinked_mod_dir() {
        // Review round 5: anchoring containment on the canonicalized MOD
        // DIR let a pre-planted `<mods-root>/<id>` symlink carry the
        // approved write outside the root — `create_dir_all` succeeds on
        // the symlinked dir and every per-file check anchored at the
        // symlink target passed. The anchor must be the mods root.
        let _env = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let _home = ScopedHome::set_trusted(&dir, &dir.path().join("ws"));
        let ws = workspace_in(&dir);
        let mods_root = ws.join(".codesmith").join("mods");
        std::fs::create_dir_all(&mods_root).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, mods_root.join("pwn")).unwrap();
        let files = vec![(
            "mod.toml".to_string(),
            "id = \"pwn\"\nversion = \"0.1.0\"\n".to_string(),
        )];
        let err = write_mod(&ws, "pwn", &files, false).unwrap_err();
        assert!(err.contains("outside the mods root"), "{err}");
        assert!(
            !outside.join("mod.toml").exists(),
            "nothing may land in the symlink target"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_mod_refuses_final_component_symlink() {
        // A symlinked FILE inside an honest mod dir: `fs::write` follows
        // it, and the parent-containment check cannot see it.
        let _env = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let _home = ScopedHome::set_trusted(&dir, &dir.path().join("ws"));
        let ws = workspace_in(&dir);
        let mods_root = ws.join(".codesmith").join("mods");
        let mod_dir = write_fixture_mod(&mods_root, "hooked");
        let victim = dir.path().join("victim.toml");
        std::fs::write(&victim, "original").unwrap();
        std::fs::remove_file(mod_dir.join("mod.toml")).unwrap();
        std::os::unix::fs::symlink(&victim, mod_dir.join("mod.toml")).unwrap();
        let files = vec![(
            "mod.toml".to_string(),
            "id = \"hooked\"\nversion = \"0.1.0\"\n".to_string(),
        )];
        let err = write_mod(&ws, "hooked", &files, false).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "original",
            "the write must not follow the file symlink"
        );
    }

    #[test]
    fn write_mod_refuses_global_writes_from_untrusted_workspace() {
        // Review round 5: the trust gate covered only project writes; a
        // `global=true` write persisted executable mod code into
        // ~/.codesmith/mods, which every workspace (including other
        // untrusted ones) discovers and loads once activated.
        let _env = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        // No trust config written → the workspace is untrusted.
        let _home = ScopedHome::set(&dir);
        let ws = workspace_in(&dir);
        let files = vec![(
            "mod.toml".to_string(),
            "id = \"g\"\nversion = \"0.1.0\"\n".to_string(),
        )];
        let err = write_mod(&ws, "g", &files, true).unwrap_err();
        assert!(err.contains("not trusted"), "{err}");
        let err = write_mod(&ws, "g", &files, false).unwrap_err();
        assert!(err.contains("not trusted"), "{err}");
    }

    #[test]
    fn activate_persists_and_reports() {
        let _env = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let _home = ScopedHome::set_trusted(&dir, &dir.path().join("ws"));
        let ws = workspace_in(&dir);
        write_fixture_mod(&ws.join(".codesmith").join("mods"), "act-me");
        let mut state = store_in(&dir);
        let msg = activate(&ws, &mut state, "act-me").unwrap();
        assert!(msg.contains("act-me"));
        assert!(state.should_load("act-me"));
        // Persisted.
        let reloaded = ModStateStore::load_from(dir.path().join("mods_state.toml")).unwrap();
        assert!(reloaded.should_load("act-me"));
    }

    #[test]
    fn activate_unknown_id_is_error() {
        let dir = TempDir::new().unwrap();
        let ws = workspace_in(&dir);
        let mut state = store_in(&dir);
        let err = activate(&ws, &mut state, "ghost").unwrap_err();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn set_enabled_requires_activation() {
        let dir = TempDir::new().unwrap();
        let mut state = store_in(&dir);
        let err = set_enabled(&mut state, "stranger", false).unwrap_err();
        assert!(err.contains("not activated"), "{err}");
    }

    #[test]
    fn remove_deletes_dir_state_and_kv() {
        let _env = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let _home = ScopedHome::set_trusted(&dir, &dir.path().join("ws"));
        let ws = workspace_in(&dir);
        let mods_root = ws.join(".codesmith").join("mods");
        write_fixture_mod(&mods_root, "bye");
        let mut state = store_in(&dir);
        state.activate("bye", "irrelevant-to-remove").unwrap();
        // Simulate a kv file left by a previous load.
        std::fs::create_dir_all(dir.path().join("mods-state")).unwrap();
        std::fs::write(dir.path().join("mods-state").join("project-bye.json"), "{}").unwrap();

        remove(&ws, &mut state, "bye").unwrap();
        assert!(!mods_root.join("bye").exists());
        assert!(!state.is_activated("bye"));
        assert!(
            !dir.path()
                .join("mods-state")
                .join("project-bye.json")
                .exists()
        );
    }

    #[test]
    fn list_and_status_reflect_activation() {
        let _env = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let _home = ScopedHome::set_trusted(&dir, &dir.path().join("ws"));
        let ws = workspace_in(&dir);
        write_fixture_mod(&ws.join(".codesmith").join("mods"), "listed");
        let mut state = store_in(&dir);
        let listing = list_mods(&ws, &state);
        assert!(listing.contains("listed"), "{listing}");
        assert!(listing.contains("pending activation"), "{listing}");
        let status = mod_status(&ws, &state);
        assert!(status.contains("pending: listed"), "{status}");
        state.activate("listed", "irrelevant-to-listing").unwrap();
        let listing = list_mods(&ws, &state);
        assert!(listing.contains("activated"), "{listing}");
        let info = mod_info(&ws, &state, "listed").unwrap();
        assert!(info.contains("test mod"), "{info}");
    }

    #[test]
    fn load_rhai_mod_builds_mod_from_discovery() {
        let _env = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let _home = ScopedHome::set_trusted(&dir, &dir.path().join("ws"));
        let ws = workspace_in(&dir);
        let mods_root = ws.join(".codesmith").join("mods");
        let mod_dir = write_fixture_mod(&mods_root, "loadable");
        std::fs::write(
            mod_dir.join("mod.rhai"),
            "register_command(\"hi\", \"say hi\", |args| { message(\"hi \" + args) });",
        )
        .unwrap();
        let state = store_in(&dir);
        let found = discover_workspace_mods(&ws);
        let m = found.iter().find(|m| m.id == "loadable").unwrap();
        let rhai = load_rhai_mod(
            m,
            state.kv_dir().as_deref(),
            codesmith_agent::extension::MessageProjectionHubArc::new(
                codesmith_agent::extension::MessageProjectionHub::new(),
            ),
        )
        .unwrap();
        use codesmith_agent::extension::Extension as _;
        assert_eq!(rhai.metadata().id, "loadable");
        assert_eq!(rhai.registrations().len(), 1);
    }
}
