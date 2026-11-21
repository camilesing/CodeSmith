//! Launching the CodeSmith desktop UI in place of the interactive REPL.
//!
//! When the resolved preset is `all` and the command was about to enter the
//! interactive REPL, the CLI starts the desktop app (a Tauri shell that owns
//! its own `codesmith serve --http` backend) and exits; the terminal REPL is
//! reachable by any other preset or `[desktop] enabled = false`.
//!
//! `codesmith ui` (the `Ui` subcommand, `codes ui` through the alias) is the
//! developer entry: from inside a checkout it starts the dev stack — vite on
//! :1420 (spawned unless something already serves there) plus the Tauri
//! shell in the foreground — so one command replaces the two-terminal dance.
//!
//! Known limitations: the decision reads the *resolved* preset name, so an
//! `all` selection whose explicit keys deviate (reported as `diy`) still
//! opens the desktop; the app is spawned detached and its lifecycle is not
//! monitored (the app fails loud on its own missing backend); app discovery
//! checks file existence only, not that the file is the right binary. The
//! `ui` subcommand is checkout-scoped — an installed app still opens via
//! preset `all` — and its :1420 readiness check is a bare TCP probe, so
//! another listener on the port is reused as if it were vite.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::Config;

/// macOS app-bundle binary path for an installed desktop app.
#[cfg(target_os = "macos")]
const INSTALLED_APP: &str = "/Applications/CodeSmith.app/Contents/MacOS/CodeSmith";
#[cfg(not(target_os = "macos"))]
const INSTALLED_APP: &str = "/usr/local/bin/codesmith-desktop";

/// What the CLI should do instead of starting the terminal REPL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopLaunch {
    /// Start this app, then exit the CLI.
    Run { app: PathBuf },
    /// Keep the terminal REPL.
    Skip(&'static str),
    /// Refuse to continue (fail-loud with the build instructions).
    Fail(String),
}

/// Resolve the desktop launch decision for one REPL entry.
///
/// `preset` is the resolved (applied) preset name; `app_exists` is injected
/// so the decision is unit-testable without files on disk.
pub fn resolve_desktop_launch(
    preset: &str,
    config: &Config,
    app_exists: impl Fn(&Path) -> bool,
) -> DesktopLaunch {
    let desktop = config.desktop.clone().unwrap_or_default();
    if desktop.enabled == Some(false) {
        return DesktopLaunch::Skip("[desktop] enabled = false");
    }
    if preset != "all" {
        return DesktopLaunch::Skip("resolved preset is not 'all'");
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("CODESMITH_DESKTOP_APP") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(path) = desktop.app_path {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(codesmith_home_app());
    candidates.push(PathBuf::from(INSTALLED_APP));
    for candidate in &candidates {
        if app_exists(candidate) {
            return DesktopLaunch::Run {
                app: candidate.clone(),
            };
        }
    }
    let listed = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    DesktopLaunch::Fail(format!(
        "preset 'all' opens the CodeSmith desktop UI, but no app was found \
         (looked in {listed}). Build it with `cargo tauri build` in desktop/ \
         (see docs/DESKTOP.md), set [desktop] app_path, or disable with \
         [desktop] enabled = false"
    ))
}

/// The user-scoped install slot: `$CODESMITH_HOME/desktop/codesmith-desktop`.
fn codesmith_home_app() -> PathBuf {
    let home = std::env::var_os("CODESMITH_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".codesmith")
        });
    home.join("desktop").join("codesmith-desktop")
}

/// Start the desktop app and exit the CLI.
///
/// `codesmith_path` (from `[desktop] codesmith_path`, or this binary) tells
/// the app which backend to spawn; the app also discovers `codesmith` on
/// PATH on its own.
pub fn launch_desktop(app: &Path, codesmith_path: Option<&Path>) -> Result<()> {
    let mut command = Command::new(app);
    if let Some(path) = codesmith_path {
        command.env("CODESMITH_DESKTOP_CODESMITH", path);
    }
    command
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn desktop app {}", app.display()))?;
    Ok(())
}

/// The REPL-entry hook: run the desktop when resolved, otherwise return.
pub fn maybe_launch_desktop(preset: &str, config: &Config) -> Result<()> {
    let decision = resolve_desktop_launch(preset, config, |path| path.is_file());
    match decision {
        DesktopLaunch::Run { app } => {
            // Default the backend to this very binary so the app spawns the
            // codesmith that launched it; an explicit path wins.
            let codesmith_path = config
                .desktop
                .as_ref()
                .and_then(|d| d.codesmith_path.as_deref())
                .map(PathBuf::from)
                .or_else(|| std::env::current_exe().ok());
            launch_desktop(&app, codesmith_path.as_deref())?;
            eprintln!(
                "codesmith: preset 'all' — desktop app started ({}); terminal REPL skipped",
                app.display()
            );
            std::process::exit(0);
        }
        DesktopLaunch::Skip(reason) => {
            tracing::debug!(reason, "desktop UI not launched");
            Ok(())
        }
        DesktopLaunch::Fail(message) => bail!(message),
    }
}

// === `codesmith ui` — the dev-stack launcher ===============================

/// The dev stack's vite port (desktop/ui/vite.config.ts `server.port`; the
/// Tauri shell's `devUrl` points here too).
const DEV_UI_PORT: u16 = 1420;

/// Walk up from `start` to the checkout root owning
/// `desktop/src-tauri/tauri.conf.json`; `None` outside a checkout.
fn find_repo_root(start: PathBuf) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir
            .join("desktop")
            .join("src-tauri")
            .join("tauri.conf.json")
            .is_file()
        {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Whether something already accepts connections on the dev UI port. A bare
/// TCP probe: whatever answers is assumed to be vite (see known limitations).
async fn port_serving(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

/// `codesmith ui` — start the desktop dev stack from the enclosing checkout
/// and stay in the foreground: vite on :1420 (spawned unless something
/// already serves there — the two-terminal habit keeps working), then the
/// Tauri shell, which spawns its own `codesmith serve --http` backend. The
/// backend binary is `[desktop] codesmith_path`, else this very binary (the
/// same precedence the preset-`all` launch uses). A vite process this
/// command started is killed when the shell exits; one it reused is left
/// alone. Ctrl-C in the terminal reaches all three (they share the
/// foreground process group).
pub async fn run_dev_ui(config: &Config) -> Result<()> {
    let cwd = std::env::current_dir().context("read the current directory")?;
    let repo = find_repo_root(cwd).with_context(|| {
        "codesmith ui runs the desktop dev stack and needs a CodeSmith checkout \
         (it looks for desktop/src-tauri walking up from the current directory). \
         Run it from inside the repository, or see docs/DESKTOP.md; an \
         installed app opens with preset `all` instead."
    })?;
    let ui_dir = repo.join("desktop").join("ui");

    let mut vite = if port_serving(DEV_UI_PORT).await {
        println!("codesmith ui: :{DEV_UI_PORT} already serving — reusing it");
        None
    } else {
        Some(start_vite(&ui_dir).await?)
    };

    let codesmith_path = config
        .desktop
        .as_ref()
        .and_then(|d| d.codesmith_path.as_deref())
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .context("resolve the codesmith binary for the desktop backend")?;
    let tauri_manifest = repo.join("desktop").join("src-tauri").join("Cargo.toml");
    println!(
        "codesmith ui: starting the desktop shell (backend {})",
        codesmith_path.display()
    );
    // Foreground with inherited stdio: the shell's and backend's logs belong
    // in this terminal, and the exit code is the command's.
    let status = tokio::process::Command::new("cargo")
        .arg("run")
        .arg("--manifest-path")
        .arg(&tauri_manifest)
        .env("CODESMITH_DESKTOP_CODESMITH", &codesmith_path)
        .status()
        .await
        .with_context(|| format!("cargo run {}", tauri_manifest.display()))?;

    if let Some(child) = vite.as_mut() {
        let _ = child.start_kill();
    }
    if !status.success() {
        bail!("desktop shell exited with {status}");
    }
    Ok(())
}

/// Spawn `pnpm --dir <ui> dev` (installing first when node_modules is
/// absent) and wait until :1420 answers or the child dies.
async fn start_vite(ui_dir: &Path) -> Result<tokio::process::Child> {
    if !ui_dir
        .join("node_modules")
        .join(".bin")
        .join("vite")
        .exists()
    {
        println!("codesmith ui: desktop/ui has no node_modules — pnpm install first");
        let install = tokio::process::Command::new("pnpm")
            .arg("--dir")
            .arg(ui_dir)
            .arg("install")
            .stdin(Stdio::null())
            .status()
            .await
            .with_context(|| format!("pnpm install in {}", ui_dir.display()))?;
        if !install.success() {
            bail!("pnpm install failed with {install} — see docs/DESKTOP.md");
        }
    }
    // kill_on_drop is the secondary cleanup for unusual exit paths; the
    // normal one is the explicit start_kill after the shell exits.
    let mut child = tokio::process::Command::new("pnpm")
        .arg("--dir")
        .arg(ui_dir)
        .arg("dev")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn pnpm dev in {}", ui_dir.display()))?;
    println!("codesmith ui: vite starting on :{DEV_UI_PORT}");
    for _ in 0..60 {
        if port_serving(DEV_UI_PORT).await {
            return Ok(child);
        }
        if let Some(status) = child.try_wait()? {
            bail!("vite exited before serving ({status})");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = child.start_kill();
    bail!("vite did not serve on :{DEV_UI_PORT} within 30s");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DesktopConfig;

    fn config_with(desktop: Option<DesktopConfig>) -> Config {
        Config {
            desktop,
            ..Config::default()
        }
    }

    #[test]
    fn launch_runs_when_preset_all_and_app_exists() {
        let exists = |p: &Path| p == Path::new("/apps/codesmith-desktop");
        let config = config_with(Some(DesktopConfig {
            app_path: Some("/apps/codesmith-desktop".into()),
            ..DesktopConfig::default()
        }));
        assert_eq!(
            resolve_desktop_launch("all", &config, exists),
            DesktopLaunch::Run {
                app: PathBuf::from("/apps/codesmith-desktop")
            }
        );
    }

    #[test]
    fn launch_skips_when_disabled() {
        let config = config_with(Some(DesktopConfig {
            enabled: Some(false),
            app_path: Some("/apps/codesmith-desktop".into()),
            ..DesktopConfig::default()
        }));
        assert_eq!(
            resolve_desktop_launch("all", &config, |_| true),
            DesktopLaunch::Skip("[desktop] enabled = false")
        );
    }

    #[test]
    fn launch_skips_when_preset_is_not_all() {
        let config = config_with(None);
        assert_eq!(
            resolve_desktop_launch("middle", &config, |_| true),
            DesktopLaunch::Skip("resolved preset is not 'all'")
        );
    }

    #[test]
    fn launch_fails_loud_when_app_missing() {
        let config = config_with(None);
        let decision = resolve_desktop_launch("all", &config, |_| false);
        match decision {
            DesktopLaunch::Fail(message) => {
                assert!(message.contains("preset 'all' opens the CodeSmith desktop UI"));
                assert!(message.contains("cargo tauri build"));
                assert!(message.contains("[desktop] enabled = false"));
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn find_repo_root_walks_up_to_the_checkout() {
        let root = std::env::temp_dir().join(format!("csm-repo-{}", std::process::id()));
        let marker = root
            .join("desktop")
            .join("src-tauri")
            .join("tauri.conf.json");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "{}").unwrap();
        assert_eq!(find_repo_root(root.clone()), Some(root.clone()));
        assert_eq!(
            find_repo_root(root.join("crates").join("cli").join("src")),
            Some(root.clone())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_repo_root_finds_nothing_outside_a_checkout() {
        let elsewhere = std::env::temp_dir().join(format!("csm-empty-{}", std::process::id()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert_eq!(find_repo_root(elsewhere.clone()), None);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }
}
