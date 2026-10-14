//! `/extension …` — the in-REPL extension manager (the §F port).
//!
//! Readouts use the same sources the engine's populate pass uses
//! (compiled-in inventory + `~/.codesmith/extensions` /
//! `<workspace>/.codesmith/extensions` dylibs + `extensions_state.toml`);
//! `reload` funnels through [`crate::mod_ops::reload_mods`], the single
//! reload chokepoint shared with the `manage_mods` tool. Install/uninstall
//! drive the `codesmith_extensions` installer library (fetch → build →
//! place → sha256 sidecar). Known limitation: discovery-side trust gating
//! stays at load time — install is trust-agnostic and only *warns* for
//! project scope into an untrusted workspace.

use std::path::PathBuf;
use std::sync::Arc;

use colored::Colorize;

use super::ReplState;

/// `/extension [list|info|enable|disable|status|reload|install|uninstall]`
pub async fn extension_command(state: &ReplState, arg: &str) {
    let mut parts = arg.split_whitespace();
    let sub = parts.next().unwrap_or("list").to_ascii_lowercase();
    let rest: Vec<&str> = parts.collect();
    match sub.as_str() {
        "" | "list" | "ls" => list(state).await,
        "status" => status(state),
        "info" => match rest.first() {
            Some(id) => info(state, id).await,
            None => println!("{} usage: /extension info <id>", "✗".red()),
        },
        "enable" | "disable" => match rest.first() {
            Some(id) => set_enabled(id, sub == "enable"),
            None => println!("{} usage: /extension {sub} <id>", "✗".red()),
        },
        "reload" => match super::mods_cmds::reload_note(state).await {
            Ok(text) => println!("{text}"),
            Err(err) => println!("{} {err}", "✗".red()),
        },
        "install" => install(state, rest.join(" ").trim()).await,
        "uninstall" => match rest.first() {
            Some(id) => uninstall(state, id).await,
            None => println!("{} usage: /extension uninstall <id>", "✗".red()),
        },
        other => println!(
            "{} unknown subcommand `{other}` — list | info | enable | disable | status | reload | install | uninstall",
            "✗".red()
        ),
    }
}

/// Global (`~/.codesmith/extensions`) and project
/// (`<workspace>/.codesmith/extensions`) dylib roots — mirrors the engine's
/// discovery paths (core/engine.rs).
fn extension_roots(workspace: &std::path::Path) -> (Vec<PathBuf>, PathBuf) {
    let global =
        crate::config::effective_home_dir().map(|home| home.join(".codesmith").join("extensions"));
    (
        global.into_iter().collect(),
        workspace.join(".codesmith").join("extensions"),
    )
}

async fn list(state: &ReplState) {
    let workspace = state.workspace.clone();
    let listing = tokio::task::spawn_blocking(move || list_sync(&workspace))
        .await
        .unwrap_or_else(|err| Err(format!("list worker failed: {err}")));
    match listing {
        Ok(text) => println!("{text}"),
        Err(err) => println!("{} {err}", "✗".red()),
    }
}

fn list_sync(workspace: &std::path::Path) -> Result<String, String> {
    let store = crate::extension_state::ExtensionStateStore::load_default()
        .map_err(|err| format!("extension state unreadable: {err}"))?;
    let (global_roots, project_root) = extension_roots(workspace);
    let dylibs = codesmith_extensions::discover_dylib(&global_roots, &[project_root]);

    let mut out = String::from("Extensions:\n");
    for reg in codesmith_extensions::discover_static() {
        out.push_str(&format!(
            "  {} (v{}) [compiled-in] — {}\n",
            reg.metadata.id,
            reg.metadata.version,
            enabled_label(&store, &reg.metadata.id)
        ));
    }
    for d in &dylibs {
        let provenance = store
            .provenance_for(&d.id)
            .map(|p| format!(" (installed from {p})"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {} (v{}) [{}] — {}{provenance}\n",
            d.id,
            d.version,
            if d.global { "global" } else { "project" },
            enabled_label(&store, &d.id)
        ));
    }
    out.push_str(
        "\nDylib roots: ~/.codesmith/extensions, <workspace>/.codesmith/extensions. \
         enable/disable take effect on the next /extension reload.",
    );
    Ok(out)
}

fn enabled_label(store: &crate::extension_state::ExtensionStateStore, id: &str) -> &'static str {
    if store.is_enabled(id) {
        "enabled"
    } else {
        "disabled"
    }
}

async fn info(state: &ReplState, id: &str) {
    let workspace = state.workspace.clone();
    let id = id.to_string();
    let info = tokio::task::spawn_blocking(move || info_sync(&workspace, &id))
        .await
        .unwrap_or_else(|err| Err(format!("info worker failed: {err}")));
    match info {
        Ok(text) => println!("{text}"),
        Err(err) => println!("{} {err}", "✗".red()),
    }
}

fn info_sync(workspace: &std::path::Path, id: &str) -> Result<String, String> {
    let store = crate::extension_state::ExtensionStateStore::load_default()
        .map_err(|err| format!("extension state unreadable: {err}"))?;
    for reg in codesmith_extensions::discover_static() {
        if reg.metadata.id == id {
            return Ok(format!(
                "extension {}:\n  name: {}\n  version: {}\n  source: compiled-in\n  enabled: {}",
                reg.metadata.id,
                reg.metadata.name,
                reg.metadata.version,
                store.is_enabled(id)
            ));
        }
    }
    let (global_roots, project_root) = extension_roots(workspace);
    for d in &codesmith_extensions::discover_dylib(&global_roots, &[project_root]) {
        if d.id == id {
            let provenance = store
                .provenance_for(id)
                .unwrap_or_else(|| "(not recorded)".to_string());
            return Ok(format!(
                "extension {}:\n  version: {}\n  source: {}\n  dylib: {}\n  config: {}\n  installed from: {}\n  enabled: {}",
                d.id,
                d.version,
                if d.global { "global" } else { "project" },
                d.dylib_path.display(),
                d.config_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(none)".to_string()),
                provenance,
                store.is_enabled(id)
            ));
        }
    }
    Err(format!(
        "no extension '{id}' — /extension list shows what exists"
    ))
}

fn status(state: &ReplState) {
    match &state.extension_runner {
        Some(runner) => {
            println!("extension runtime: generation {}", runner.generation());
            println!("  bound tools:     {}", runner.bound_tools().len());
            println!("  bound commands:  {}", runner.bound_command_names().len());
            if let Some(engine) = state.engine.as_ref() {
                let failed = crate::core::engine::failed_audit_lines(&engine.mods_audit);
                if !failed.is_empty() {
                    println!("  {} {}", "✗".red(), failed.join("; "));
                }
            }
        }
        None => println!(
            "{}",
            "no extension runner bound in this session (embed or pre-engine start)".dimmed()
        ),
    }
}

fn set_enabled(id: &str, enabled: bool) {
    let result = crate::extension_state::ExtensionStateStore::load_default()
        .and_then(|mut store| store.set_enabled(id, enabled));
    match result {
        Ok(()) => println!(
            "{id} {} — /extension reload applies it",
            if enabled { "enabled" } else { "disabled" }
        ),
        Err(err) => println!("{} {err}", "✗".red()),
    }
}

async fn install(state: &ReplState, arg: &str) {
    if arg.is_empty() {
        println!(
            "{} usage: /extension install <source>  (git:<url>[@<ref>] | path:<dir> | crate:<name>[@<version>] | prebuilt:<https-url>; flags: --global, --checksum <sha256>)",
            "✗".red()
        );
        return;
    }
    let spec = match codesmith_extensions::SourceSpec::parse(arg) {
        Ok(spec) => spec,
        Err(err) => {
            println!("{} {err}", "✗".red());
            return;
        }
    };
    let (global_roots, project_root) = extension_roots(&state.workspace);
    let global_scope = matches!(&spec.scope, codesmith_extensions::InstallScope::Global);
    let root = match &spec.scope {
        codesmith_extensions::InstallScope::Global => match global_roots.first() {
            Some(root) => root.clone(),
            None => {
                println!(
                    "{} no home directory — cannot resolve the global extensions root",
                    "✗".red()
                );
                return;
            }
        },
        codesmith_extensions::InstallScope::Project => project_root,
    };
    let untrusted_project = !global_scope && !crate::config::is_workspace_trusted(&state.workspace);
    let result = tokio::task::spawn_blocking(move || run_install(spec, root)).await;
    match result {
        Ok(Ok(report)) => {
            println!(
                "installed {} (v{}) → {}",
                report.id,
                report.version,
                report.path.display()
            );
            println!("{}", format!("provenance: {}", report.provenance).dimmed());
            if let Err(err) = crate::extension_state::ExtensionStateStore::load_default()
                .and_then(|mut store| store.add_installed(&report.id, &report.provenance))
            {
                println!(
                    "{}",
                    format!("warning: could not record install provenance: {err}").yellow()
                );
            }
            if untrusted_project {
                println!(
                    "{}",
                    "warning: this workspace is not trusted — project-scope extensions are gated at load; add the path with /trust add to trust it".yellow()
                );
            }
            println!("{}", "run /extension reload to load it".dimmed());
        }
        Ok(Err(err)) => println!("{} install failed: {err}", "✗".red()),
        Err(err) => println!("{} install worker failed: {err}", "✗".red()),
    }
}

/// Cargo target dir for source-building installs (git/path/crate).
fn build_dir() -> PathBuf {
    std::env::temp_dir().join("codesmith-extension-builds")
}

fn run_install(
    spec: codesmith_extensions::SourceSpec,
    root: PathBuf,
) -> Result<codesmith_extensions::InstallReport, codesmith_extensions::ExtensionError> {
    use codesmith_extensions::{
        CargoBuilder, CratesIoSource, CurlHttpFetcher, GitSource, IdentityBuilder, LocalPathSource,
        PrebuiltDylibSource, SourceKind,
    };

    let source: Box<dyn codesmith_extensions::ExtensionSource> = match spec.kind {
        SourceKind::Git => Box::new(GitSource::new(spec.body.clone(), spec.ref_.clone())),
        SourceKind::Path => Box::new(LocalPathSource::new(spec.body.clone())),
        SourceKind::CratesIo => Box::new(CratesIoSource::new(
            spec.body.clone(),
            spec.ref_.clone(),
            Arc::new(CurlHttpFetcher::new()),
        )),
        SourceKind::Prebuilt => Box::new(PrebuiltDylibSource::new(
            spec.body.clone(),
            spec.checksum.clone(),
            Arc::new(CurlHttpFetcher::new()),
        )),
    };
    let builder: Box<dyn codesmith_extensions::ExtensionBuilder> = match spec.kind {
        SourceKind::Prebuilt => Box::new(IdentityBuilder),
        _ => Box::new(CargoBuilder::new(build_dir())),
    };
    codesmith_extensions::Installer::new(source.as_ref(), builder.as_ref(), root).install(&spec)
}

async fn uninstall(state: &ReplState, id: &str) {
    let (mut roots, project_root) = extension_roots(&state.workspace);
    roots.push(project_root);
    let id = id.to_string();
    let result = tokio::task::spawn_blocking(move || {
        codesmith_extensions::Installer::uninstall_files(&id, &roots)
    })
    .await;
    match result {
        Ok(Ok(report)) => {
            if report.removed {
                let _ = crate::extension_state::ExtensionStateStore::load_default()
                    .and_then(|mut store| store.remove_installed(&report.id));
                println!(
                    "uninstalled {} — bindings clear on the next /extension reload",
                    report.id
                );
            } else {
                println!(
                    "{}",
                    format!(
                        "no installed files for {} under the extensions roots",
                        report.id
                    )
                    .yellow()
                );
            }
        }
        Ok(Err(err)) => println!("{} uninstall failed: {err}", "✗".red()),
        Err(err) => println!("{} uninstall worker failed: {err}", "✗".red()),
    }
}
