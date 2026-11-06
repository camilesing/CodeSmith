//! Launching the CodeSmith desktop UI in place of the interactive REPL.
//!
//! When the resolved preset is `all` and the command was about to enter the
//! interactive REPL, the CLI starts the desktop app (a Tauri shell that owns
//! its own `codesmith serve --http` backend) and exits; the terminal REPL is
//! reachable by any other preset or `[desktop] enabled = false`.
//!
//! Known limitations: the decision reads the *resolved* preset name, so an
//! `all` selection whose explicit keys deviate (reported as `diy`) still
//! opens the desktop; the app is spawned detached and its lifecycle is not
//! monitored (the app fails loud on its own missing backend); app discovery
//! checks file existence only, not that the file is the right binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
}
