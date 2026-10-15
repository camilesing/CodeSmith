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

use tokio_util::sync::CancellationToken;

use codesmith_extensions::{
    DiscoveredMod, ExtensionRunner, ModKvStore, RhaiMod, apply_mod_trust_gate, discover_mods,
};

use crate::mod_state::{ActivationHashState, ModStateStore};

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

/// Per-mod listing status, derived exactly the way the engine's populate
/// gate derives it (core/engine.rs): a hash-drifted or unreadable entry
/// file is pending re-consent, not loaded — the listings must not report
/// it as loaded precisely when the user needs to re-approve.
pub(crate) enum ModListStatus {
    Disabled,
    /// Discovered, never activated — awaits first consent.
    PendingActivation,
    /// Activated, consent still valid — what the runner loads.
    Loaded,
    /// Activated, but the entry file changed since the recorded consent
    /// (or is unreadable) — the populate gate pends it instead of loading.
    PendingReconsent {
        reason: String,
    },
}

/// Classify one discovered mod against the state store, mirroring the
/// engine's populate gate (disabled → pending → hash check → load).
pub(crate) fn classify_mod(state: &ModStateStore, m: &DiscoveredMod) -> ModListStatus {
    if !state.should_load(&m.id) {
        // `should_load` = activated && !disabled; disabled outranks pending
        // in the display split (a disabled-but-unactivated mod shows as
        // disabled only).
        return if state.is_disabled(&m.id) {
            ModListStatus::Disabled
        } else {
            ModListStatus::PendingActivation
        };
    }
    match entry_file_hash(&m.entry_path) {
        Ok(hash) => match state.activation_hash_state(&m.id, &hash) {
            ActivationHashState::Changed => ModListStatus::PendingReconsent {
                reason: "entry file changed since activation".to_string(),
            },
            // `Matches` and legacy `NoRecord` both load (the gate backfills
            // the hash on first load).
            _ => ModListStatus::Loaded,
        },
        // Unreadable entry: the populate gate audits Failed and does not
        // load it — the listing must not claim it loaded either.
        Err(e) => ModListStatus::PendingReconsent {
            reason: format!("entry file unreadable ({e})"),
        },
    }
}

/// `/mods list` — every discovered mod with its activation state.
pub fn list_mods(workspace: &Path, state: &ModStateStore) -> String {
    let mods = discover_workspace_mods(workspace);
    if mods.is_empty() {
        return "No script mods discovered. Install one under ~/.codesmith/mods/<id>/ or <workspace>/.codesmith/mods/<id>/ (mod.toml + mod.rhai), or ask the agent to write one via manage_mods.".to_string();
    }
    let mut out = String::from("Script mods:\n");
    for m in &mods {
        let status = match classify_mod(state, m) {
            ModListStatus::Disabled => "disabled".to_string(),
            ModListStatus::PendingActivation => "pending activation".to_string(),
            ModListStatus::Loaded => "activated".to_string(),
            ModListStatus::PendingReconsent { reason } => {
                format!("pending re-activation ({reason})")
            }
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
/// Both entry points (`/mods enable|disable` and the manage_mods tool)
/// reload immediately after this, so enablement takes effect right away.
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
        format!("Enabled mod '{id}'.")
    } else {
        format!("Disabled mod '{id}'.")
    })
}

/// Remove a mod: delete its directory + KV file + all state records.
pub fn remove(workspace: &Path, state: &mut ModStateStore, id: &str) -> Result<String, String> {
    let mods = discover_workspace_mods(workspace);
    let m = mods
        .into_iter()
        .find(|m| m.id == id)
        .ok_or_else(|| format!("No mod with id '{id}' on disk."))?;
    // Clear state FIRST: once the directory is gone, discovery can no
    // longer resolve the id, so a persist failure after a successful
    // directory removal would leave a stale activated record that no
    // retry can clear. Deactivating first keeps every failure retryable —
    // a failed dir deletion leaves a discoverable-but-unactivated mod.
    state
        .deactivate(id)
        .map_err(|e| format!("clear state: {e}"))?;
    std::fs::remove_dir_all(&m.dir).map_err(|e| format!("remove {}: {e}", m.dir.display()))?;
    if let Some(kv_dir) = state.kv_dir() {
        let scope = if m.global { "global" } else { "project" };
        let _ = std::fs::remove_file(kv_dir.join(format!("{scope}-{}.json", m.id)));
    }
    Ok(format!(
        "Removed mod '{id}' ({}). Its bindings unload immediately after this.",
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

// === Per-mod lifecycle (single-mod load/unload, manual boundary) ============

/// Drive an async runner transition from sync (spawn_blocking) context —
/// the same thread + current-thread-runtime pattern
/// `populate_extension_runtime` uses (building a tokio runtime on a runtime
/// worker thread panics; see the comment there). The future must be owned
/// (clone the `Arc<ExtensionRunner>` into it).
fn drive_transition<F>(fut: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    std::thread::scope(|s| {
        let handle = s.spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("mod transition runtime")
                .block_on(fut)
        });
        handle.join().expect("mod transition thread panicked")
    })
}

/// Load one activated mod live — the single-mod counterpart of
/// [`reload_mods`]. The populate gate (activated + enabled + entry-hash
/// consent) is re-checked for this id, then compile + bind + retain.
/// Survivors are untouched: no recompile, no whole-layer reload. Fails
/// loud in-band (the returned message says what is wrong; nothing loads).
pub fn load_mod(
    runner: &Arc<ExtensionRunner>,
    workspace: &Path,
    id: &str,
    mods_enabled: bool,
) -> String {
    if !mods_enabled {
        return format!(
            "Mod layer disabled ([mods] enabled = false) — '{id}' stays on disk; it loads at the next session with the layer enabled."
        );
    }
    let Some(m) = discover_workspace_mods(workspace)
        .into_iter()
        .find(|m| m.id == id)
    else {
        return format!("No mod with id '{id}' on disk — /mods list shows what exists.");
    };
    let Ok(state) = ModStateStore::load_default() else {
        return "Mod state unreadable — activation status unknown; nothing loaded.".to_string();
    };
    if state.is_disabled(id) {
        return format!("Mod '{id}' is disabled — enable it first (/mods enable {id}).");
    }
    if !state.is_activated(id) {
        return format!("Mod '{id}' is not activated — activate it first (/mods activate {id}).");
    }
    let hash = match entry_file_hash(&m.entry_path) {
        Ok(h) => h,
        Err(e) => return format!("Cannot read '{id}' entry file: {e} — nothing loaded."),
    };
    match state.activation_hash_state(id, &hash) {
        ActivationHashState::Matches => {}
        ActivationHashState::NoRecord => {
            // Legacy activation without a recorded hash — the same backfill
            // the populate gate performs, under the state lock.
            let _guard = mod_state_lock();
            match ModStateStore::load_default() {
                Ok(mut fresh) => {
                    if let Err(e) = fresh.record_activation_hash(id, &hash) {
                        tracing::warn!(
                            target: "codesmith_mods",
                            "backfill activation hash for {id}: {e}"
                        );
                    }
                }
                Err(e) => tracing::warn!(
                    target: "codesmith_mods",
                    "backfill activation hash for {id}: state unreadable ({e})"
                ),
            }
        }
        ActivationHashState::Changed => {
            return format!(
                "Mod '{id}' entry file changed since activation — the recorded consent does not cover this content. /mods activate {id} to re-consent."
            );
        }
    }
    let kv_dir = state.kv_dir();
    let hub = runner.message_projection_hub();
    let rhai_mod = match load_rhai_mod(&m, kv_dir.as_deref(), hub) {
        Ok(m) => Arc::new(m),
        Err(e) => return format!("Compile '{id}' failed, nothing loaded: {e}"),
    };
    let gen_before = runner.generation();
    let runner_in = Arc::clone(runner);
    match drive_transition(async move { runner_in.bind_mod(rhai_mod).await }) {
        Ok(rejections) => {
            let mut msg = format!(
                "Loaded mod '{id}' live (generation {gen_before} → {}); takes effect at the next turn boundary.",
                runner.generation()
            );
            for r in rejections {
                msg.push_str(&format!(" Rejected registration: {r}"));
            }
            msg
        }
        Err(e) => {
            format!("Bind '{id}' failed: {e} — the mod stays on disk; retry or /mods reload.")
        }
    }
}

/// Unload one live mod — the single-mod counterpart of [`reload_mods`]:
/// drop its compiled form, re-register the survivors from their retained
/// compiled ASTs (no recompile), bump the generation once. Known
/// limitation, by design: the mod's KV file persists (its opt-in store);
/// the unload reclaims the compiled engine + AST only.
pub fn unload_mod(runner: &Arc<ExtensionRunner>, id: &str) -> String {
    let gen_before = runner.generation();
    let survivors_before = runner.retained_mod_ids();
    let runner_in = Arc::clone(runner);
    let id_in = id.to_string();
    let outcome = drive_transition(async move { runner_in.unload_retained_mod(&id_in).await });
    if !outcome.evicted {
        return format!(
            "Mod '{id}' had no live binding this session (discovered, not loaded); nothing to unload."
        );
    }
    let mut msg = format!(
        "Unloaded mod '{id}' (generation {gen_before} → {}); {} surviving mod(s) re-registered without recompile.",
        runner.generation(),
        survivors_before.iter().filter(|s| s.as_str() != id).count()
    );
    if !outcome.errors.is_empty() {
        msg.push_str(&format!(" Errors: {}.", outcome.errors.join("; ")));
    }
    msg
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

    // === Per-mod lifecycle (single-mod load/unload) ========================

    /// A fixture mod whose entry registers one named tool.
    fn write_tool_fixture_mod(root: &Path, id: &str, tool: &str) -> PathBuf {
        let dir = write_fixture_mod(root, id);
        std::fs::write(
            dir.join("mod.rhai"),
            format!(
                r#"register_tool(#{{ name: "{tool}", description: "test tool", schema: #{{ type: "object", properties: #{{}}, additionalProperties: false }} }}, |input, ctx| {{ ok("{tool}") }});"#
            ),
        )
        .unwrap();
        dir
    }

    /// A runner bound against the real host context (the engine-build
    /// wiring, minus the engine).
    fn bound_runner(workspace: &Path) -> Arc<ExtensionRunner> {
        use codesmith_extensions::HostExtensionContext;
        let runner = Arc::new(ExtensionRunner::new());
        let ctx = HostExtensionContext::new(
            workspace.to_path_buf(),
            codesmith_agent::extension::ExtensionMode::Tui,
            Arc::new(StdMutex::new(true)),
            Arc::new(StdMutex::new(CancellationToken::new())),
            runner.generation_arc(),
        );
        runner.bind_core(Arc::new(ctx));
        runner
    }

    #[test]
    fn load_and_unload_single_mod_keep_survivors() {
        let _env = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let _home = ScopedHome::set_trusted(&dir, &dir.path().join("ws"));
        let ws = workspace_in(&dir);
        let mods_root = ws.join(".codesmith").join("mods");
        write_tool_fixture_mod(&mods_root, "mod-a", "tool-a");
        write_tool_fixture_mod(&mods_root, "mod-b", "tool-b");
        {
            let _guard = mod_state_lock();
            let mut state = ModStateStore::load_default().unwrap();
            activate(&ws, &mut state, "mod-a").unwrap();
            activate(&ws, &mut state, "mod-b").unwrap();
        }

        let runner = bound_runner(&ws);
        let note = load_mod(&runner, &ws, "mod-a", true);
        assert!(note.contains("Loaded mod 'mod-a' live"), "{note}");
        let note = load_mod(&runner, &ws, "mod-b", true);
        assert!(note.contains("Loaded mod 'mod-b' live"), "{note}");

        let tools = |runner: &Arc<ExtensionRunner>| {
            let mut names: Vec<_> = runner
                .bound_tools()
                .into_iter()
                .map(|(name, _)| name)
                .collect();
            names.sort();
            names
        };
        assert_eq!(tools(&runner), vec!["tool-a", "tool-b"]);

        // Unload one: its binding goes, the survivor stays live without a
        // recompile, generation bumps.
        let gen_before = runner.generation();
        let note = unload_mod(&runner, "mod-a");
        assert!(note.contains("Unloaded mod 'mod-a'"), "{note}");
        assert!(runner.generation() > gen_before);
        assert_eq!(tools(&runner), vec!["tool-b"]);

        // Enable-after-disable roundtrip: the same mod loads back live.
        // Slot order is preserved (same-id retain replaces in place).
        let note = load_mod(&runner, &ws, "mod-a", true);
        assert!(note.contains("Loaded mod 'mod-a' live"), "{note}");
        assert_eq!(tools(&runner), vec!["tool-a", "tool-b"]);
    }

    #[test]
    fn load_mod_refuses_hash_drift_and_unloaded_ids() {
        let _env = crate::test_support::lock_test_env();
        let dir = TempDir::new().unwrap();
        let _home = ScopedHome::set_trusted(&dir, &dir.path().join("ws"));
        let ws = workspace_in(&dir);
        let mods_root = ws.join(".codesmith").join("mods");
        let mod_dir = write_tool_fixture_mod(&mods_root, "drift", "tool-drift");
        {
            let _guard = mod_state_lock();
            let mut state = ModStateStore::load_default().unwrap();
            activate(&ws, &mut state, "drift").unwrap();
        }
        let runner = bound_runner(&ws);
        assert!(load_mod(&runner, &ws, "drift", true).contains("Loaded mod 'drift' live"));

        // Content changed since the recorded consent: load must refuse
        // (the one-time consent no longer covers this content).
        std::fs::write(
            mod_dir.join("mod.rhai"),
            r#"register_tool(#{ name: "tool-drift-2", description: "d", schema: #{ type: "object", properties: #{} } }, |i| ok("x"));"#,
        )
        .unwrap();
        let note = load_mod(&runner, &ws, "drift", true);
        assert!(note.contains("changed since activation"), "{note}");

        // Unknown id: loud no-op.
        let note = load_mod(&runner, &ws, "ghost", true);
        assert!(note.contains("No mod with id 'ghost'"), "{note}");
        let note = unload_mod(&runner, "ghost");
        assert!(note.contains("no live binding"), "{note}");
    }
}
