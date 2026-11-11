//! The host `open-in-app` catalog and launch route.
//!
//! The vendored client's application control (`client-ui-open-in-app`) asks
//! the host origin for the applications installed on this machine
//! (`GET /open-in-app/apps`) and launches one on a workspace directory
//! (`POST /open-in-app/open`). The catalog is a compile-time table of the
//! application ids the vendored control knows how to label (`applications.ts`
//! `APP_LABEL_KEY`): the platform file manager plus editors, IDEs, Git GUIs
//! and terminals installed in their conventional locations. Each entry
//! resolves to a launcher this host actually holds — an existing macOS
//! `.app` bundle, a PATH executable, or a fixed OS command — never a bare
//! install guess.
//!
//! The workspace directory is appended to the launcher's argv unless an
//! argument carries the `{path}` token (the `--cd={path}` /
//! `--working-directory={path}` forms). Launches run detached with a
//! secret-looking environment variable scrubbed, and the route watches the
//! first moments of the child so a launcher that fails immediately is an
//! error response instead of a false success; a long-running GUI is left
//! running.
//!
//! Launch validates the target first: an absolute existing directory inside
//! the bridge cwd, a registered workspace, or an imported thread workspace.
//! The client's controller opens workspace directories, never arbitrary
//! paths, but the route is reachable by any page origin the bridge serves, so
//! it keeps the same fence as the session open/reveal route.
//!
//! Known limitations: discovery checks the conventional install locations
//! (the two `Applications` directories on macOS, PATH on Linux/Windows) — an
//! application renamed or moved elsewhere is not offered. Icons are not
//! served: the vendored control addresses a page-relative `/open-in-app/icon`
//! URL, which this shell's webview would resolve against its own asset
//! server, so the control renders its built-in fallback glyph.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::SharedHost;

/// Launch-argument token carrying the workspace directory.
const PATH_TOKEN: &str = "{path}";

/// One macOS application: the catalog id plus the bundle names that hold it.
struct MacApp {
    id: &'static str,
    bundles: &'static [&'static str],
}

/// The macOS catalog in menu order. Finder and Terminal are handled
/// separately (fixed OS locations); a bundle renamed or moved outside the
/// conventional roots is not detected.
const MAC_APPS: &[MacApp] = &[
    MacApp {
        id: "cursor",
        bundles: &["Cursor.app"],
    },
    MacApp {
        id: "vscode",
        bundles: &["Visual Studio Code.app"],
    },
    MacApp {
        id: "vscodeinsiders",
        bundles: &["Visual Studio Code - Insiders.app"],
    },
    MacApp {
        id: "windsurf",
        bundles: &["Windsurf.app"],
    },
    MacApp {
        id: "zed",
        bundles: &["Zed.app", "Zed Preview.app"],
    },
    MacApp {
        id: "sublimetext",
        bundles: &["Sublime Text.app"],
    },
    MacApp {
        id: "androidstudio",
        bundles: &["Android Studio.app"],
    },
    MacApp {
        id: "intellij",
        bundles: &[
            "IntelliJ IDEA.app",
            "IntelliJ IDEA Ultimate.app",
            "IntelliJ IDEA CE.app",
        ],
    },
    MacApp {
        id: "pycharm",
        bundles: &[
            "PyCharm.app",
            "PyCharm Professional.app",
            "PyCharm CE.app",
            "PyCharm Community.app",
        ],
    },
    MacApp {
        id: "webstorm",
        bundles: &["WebStorm.app"],
    },
    MacApp {
        id: "phpstorm",
        bundles: &["PhpStorm.app"],
    },
    MacApp {
        id: "goland",
        bundles: &["GoLand.app"],
    },
    MacApp {
        id: "rider",
        bundles: &["Rider.app", "JetBrains Rider.app"],
    },
    MacApp {
        id: "rustrover",
        bundles: &["RustRover.app"],
    },
    MacApp {
        id: "fork",
        bundles: &["Fork.app"],
    },
    MacApp {
        id: "sourcetree",
        bundles: &["Sourcetree.app"],
    },
    MacApp {
        id: "github",
        bundles: &["GitHub Desktop.app"],
    },
    MacApp {
        id: "tower",
        bundles: &["Tower.app"],
    },
    MacApp {
        id: "gitkraken",
        bundles: &["GitKraken.app"],
    },
    MacApp {
        id: "smartgit",
        bundles: &["SmartGit.app"],
    },
    MacApp {
        id: "sublimemerge",
        bundles: &["Sublime Merge.app"],
    },
    MacApp {
        id: "ghostty",
        bundles: &["Ghostty.app"],
    },
    MacApp {
        id: "warp",
        bundles: &["Warp.app"],
    },
    MacApp {
        id: "iterm",
        bundles: &["iTerm.app"],
    },
    MacApp {
        id: "kitty",
        bundles: &["kitty.app"],
    },
];

/// One PATH-resolved application (Linux/Windows) with its launch args.
struct CliApp {
    id: &'static str,
    program: &'static str,
    args: &'static [&'static str],
}

/// The Linux catalog in menu order; `filemanager` is the OS open verb.
const LINUX_APPS: &[CliApp] = &[
    CliApp {
        id: "filemanager",
        program: "xdg-open",
        args: &[],
    },
    CliApp {
        id: "cursor",
        program: "cursor",
        args: &[],
    },
    CliApp {
        id: "vscode",
        program: "code",
        args: &[],
    },
    CliApp {
        id: "vscodeinsiders",
        program: "code-insiders",
        args: &[],
    },
    CliApp {
        id: "windsurf",
        program: "windsurf",
        args: &[],
    },
    CliApp {
        id: "zed",
        program: "zed",
        args: &[],
    },
    CliApp {
        id: "sublimetext",
        program: "subl",
        args: &[],
    },
    CliApp {
        id: "androidstudio",
        program: "studio",
        args: &[],
    },
    CliApp {
        id: "intellij",
        program: "idea",
        args: &[],
    },
    CliApp {
        id: "pycharm",
        program: "pycharm",
        args: &[],
    },
    CliApp {
        id: "webstorm",
        program: "webstorm",
        args: &[],
    },
    CliApp {
        id: "phpstorm",
        program: "phpstorm",
        args: &[],
    },
    CliApp {
        id: "goland",
        program: "goland",
        args: &[],
    },
    CliApp {
        id: "rider",
        program: "rider",
        args: &[],
    },
    CliApp {
        id: "rustrover",
        program: "rustrover",
        args: &[],
    },
    CliApp {
        id: "smartgit",
        program: "smartgit",
        args: &[],
    },
    CliApp {
        id: "sublimemerge",
        program: "smerge",
        args: &[],
    },
    CliApp {
        id: "ghostty",
        program: "ghostty",
        args: &["--working-directory={path}"],
    },
    CliApp {
        id: "kitty",
        program: "kitty",
        args: &["--directory", "{path}"],
    },
    CliApp {
        id: "gnometerminal",
        program: "gnome-terminal",
        args: &["--working-directory={path}"],
    },
    CliApp {
        id: "konsole",
        program: "konsole",
        args: &["--workdir", "{path}"],
    },
];

/// The Windows catalog: Explorer plus PATH-resolved editors/terminals.
const WINDOWS_APPS: &[CliApp] = &[
    CliApp {
        id: "explorer",
        program: "explorer.exe",
        args: &[],
    },
    CliApp {
        id: "cursor",
        program: "cursor.exe",
        args: &[],
    },
    CliApp {
        id: "vscode",
        program: "code.exe",
        args: &[],
    },
    CliApp {
        id: "vscodeinsiders",
        program: "code-insiders.exe",
        args: &[],
    },
    CliApp {
        id: "windsurf",
        program: "windsurf.exe",
        args: &[],
    },
    CliApp {
        id: "zed",
        program: "zed.exe",
        args: &[],
    },
    CliApp {
        id: "sublimetext",
        program: "sublime_text.exe",
        args: &[],
    },
    CliApp {
        id: "smartgit",
        program: "smartgit.exe",
        args: &[],
    },
    CliApp {
        id: "sublimemerge",
        program: "sublime_merge.exe",
        args: &[],
    },
    CliApp {
        id: "windowsterminal",
        program: "wt.exe",
        args: &["-d", "{path}"],
    },
];

/// One verified launcher for a catalog entry.
#[derive(Debug, PartialEq, Eq)]
enum Launch {
    /// Append the directory to an OS open command (`open <dir>`).
    OpenPath(&'static str),
    /// macOS: `open -a <bundle> <dir>`.
    MacBundle(PathBuf),
    /// A resolved executable with its argv template.
    Argv(PathBuf, Vec<String>),
}

/// The conventional macOS bundle roots, in lookup order.
fn mac_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join("Applications"));
    }
    roots.push(PathBuf::from("/System/Applications"));
    roots.push(PathBuf::from("/System/Applications/Utilities"));
    roots
}

/// First existing bundle among `names` under `roots`.
fn mac_bundle_in(roots: &[PathBuf], names: &[&str]) -> Option<PathBuf> {
    roots.iter().find_map(|root| {
        names
            .iter()
            .map(|name| root.join(name))
            .find(|bundle| bundle.is_dir())
    })
}

/// Resolve one catalog id to a verified launcher on this host.
fn resolve_app(id: &str) -> Option<Launch> {
    match std::env::consts::OS {
        "macos" => resolve_mac(id),
        "windows" => resolve_cli(WINDOWS_APPS, id),
        _ => resolve_cli(LINUX_APPS, id),
    }
}

fn resolve_mac(id: &str) -> Option<Launch> {
    match id {
        "finder" => return Some(Launch::OpenPath("open")),
        // Terminal ships at a fixed path outside the two Applications roots.
        "terminal" => {
            let bundle = PathBuf::from("/System/Applications/Utilities/Terminal.app");
            return bundle.is_dir().then_some(Launch::MacBundle(bundle));
        }
        "xcode" => {
            let output = Command::new("xcode-select").arg("-p").output().ok()?;
            if !output.status.success() {
                return None;
            }
            let developer = String::from_utf8_lossy(&output.stdout);
            // /Applications/Xcode.app/Contents/Developer -> the bundle.
            let bundle = Path::new(developer.trim())
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)?;
            return (bundle.is_dir() && bundle.extension().is_some_and(|ext| ext == "app"))
                .then_some(Launch::MacBundle(bundle));
        }
        _ => {}
    }
    let app = MAC_APPS.iter().find(|app| app.id == id)?;
    mac_bundle_in(&mac_roots(), app.bundles).map(Launch::MacBundle)
}

fn resolve_cli(catalog: &[CliApp], id: &str) -> Option<Launch> {
    let app = catalog.iter().find(|app| app.id == id)?;
    let program = which::which(app.program).ok()?;
    Some(Launch::Argv(
        program,
        app.args.iter().map(|arg| (*arg).to_string()).collect(),
    ))
}

/// The installed applications this shell can launch, in menu order. A Linux
/// host without `xdg-open` or a CLI editor advertises only what resolves.
pub fn available_apps() -> Vec<&'static str> {
    let catalog: Vec<&'static str> = match std::env::consts::OS {
        "macos" => {
            let mut ids = vec!["finder"];
            ids.extend(MAC_APPS.iter().map(|app| app.id));
            ids.push("terminal");
            ids
        }
        "windows" => WINDOWS_APPS.iter().map(|app| app.id).collect(),
        _ => LINUX_APPS.iter().map(|app| app.id).collect(),
    };
    catalog
        .into_iter()
        .filter(|id| resolve_app(id).is_some())
        .collect()
}

/// `GET /open-in-app/apps` — the probed catalog ids.
pub async fn apps() -> Json<Value> {
    // Resolution stats bundles and may run `xcode-select`; keep it off the
    // async worker threads.
    let ids = tokio::task::spawn_blocking(available_apps)
        .await
        .unwrap_or_default();
    Json(json!({ "apps": ids }))
}

#[derive(Debug)]
enum LaunchFailure {
    NotFound,
    Outside(String),
    UnknownApp(String),
    Failed(String),
}

fn failure(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({ "code": code, "message": message.into() })),
    )
        .into_response()
}

/// Canonicalize the requested directory and require it to lie inside one of
/// the session-workspace roots. The roots are canonicalized too: a symlinked
/// workspace must fence its own target, not the link path.
fn fenced_directory(roots: &[PathBuf], requested: &Path) -> Result<PathBuf, LaunchFailure> {
    let canonical = std::fs::canonicalize(requested).map_err(|_| LaunchFailure::NotFound)?;
    if !canonical.is_dir() {
        return Err(LaunchFailure::NotFound);
    }
    let allowed = roots.iter().any(|root| {
        std::fs::canonicalize(root)
            .map(|root| canonical.starts_with(root))
            .unwrap_or(false)
    });
    if !allowed {
        return Err(LaunchFailure::Outside(
            canonical.to_string_lossy().into_owned(),
        ));
    }
    Ok(canonical)
}

/// Substitute the directory token into one argv, appending the directory
/// when no argument carries it.
fn launch_args(args: &[String], path: &str) -> Vec<String> {
    if args.iter().any(|arg| arg.contains(PATH_TOKEN)) {
        args.iter()
            .map(|arg| arg.replace(PATH_TOKEN, path))
            .collect()
    } else {
        let mut args = args.to_vec();
        args.push(path.to_string());
        args
    }
}

/// Environment names that smell like credentials. The launched application
/// is a desktop tool, not the agent; it has no use for the harness's keys.
fn is_secret_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.ends_with("_KEY")
        || upper.contains("_API_KEY")
        || upper.contains("SECRET")
        || upper.ends_with("_TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("CREDENTIAL")
}

/// One detached launch: spawn with the scrubbed environment, watch the first
/// moments for an immediate failure, and leave a still-running application
/// alone. The reaper thread waits on the child so a launcher that later exits
/// does not stay a zombie for the bridge's lifetime.
fn launch(app: &Launch, path: &Path, path_text: &str) -> Result<(), String> {
    let mut command = match app {
        Launch::OpenPath(program) => {
            let mut command = Command::new(program);
            command.arg(path);
            command
        }
        Launch::MacBundle(bundle) => {
            let mut command = Command::new("open");
            command.arg("-a").arg(bundle).arg(path);
            command
        }
        Launch::Argv(program, args) => {
            let mut command = Command::new(program);
            command.args(launch_args(args, path_text));
            command
        }
    };
    command.env_clear();
    for (name, value) in std::env::vars_os() {
        if !is_secret_env(&name.to_string_lossy()) {
            command.env(name, value);
        }
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| err.to_string())?;
    let deadline = Instant::now() + Duration::from_millis(400);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("launcher exited with {status}")),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            // A GUI that outlives the watch window is launched and owned by
            // the desktop session; reap it when it exits.
            Ok(None) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(err) => return Err(err.to_string()),
        }
    }
}

/// `POST /open-in-app/open` — hand one workspace directory to an installed
/// application. The client only checks the HTTP status; a refusal is never a
/// 2xx.
pub async fn open(State(host): State<SharedHost>, Json(body): Json<Value>) -> Response {
    let Some(app) = body.get("app").and_then(Value::as_str) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "bad-request",
            "request body must be JSON with string \"app\" and \"path\"",
        );
    };
    let Some(path) = body.get("path").and_then(Value::as_str) else {
        return failure(
            StatusCode::BAD_REQUEST,
            "bad-request",
            "request body must be JSON with string \"app\" and \"path\"",
        );
    };
    let requested = PathBuf::from(path);
    if !requested.is_absolute() {
        return failure(
            StatusCode::BAD_REQUEST,
            "bad-request",
            "path must be an absolute directory path",
        );
    }
    let roots = super::files::openable_roots(&host).await;
    let app = app.to_string();
    let app_for_launch = app.clone();
    let display = path.to_string();
    let launched = tokio::task::spawn_blocking(move || {
        let canonical = fenced_directory(&roots, &requested)?;
        let resolved = resolve_app(&app_for_launch)
            .ok_or_else(|| LaunchFailure::UnknownApp(app_for_launch.clone()))?;
        // The launched process receives the canonical directory: the fence
        // admitted it, and the application should not reopen a symlink path.
        launch(&resolved, &canonical, &canonical.to_string_lossy()).map_err(LaunchFailure::Failed)
    })
    .await;
    match launched {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(LaunchFailure::NotFound)) => failure(
            StatusCode::NOT_FOUND,
            "not-found",
            format!("directory does not exist: {display}"),
        ),
        Ok(Err(LaunchFailure::Outside(path))) => failure(
            StatusCode::BAD_REQUEST,
            "bad-request",
            format!("{path} is outside every session workspace"),
        ),
        Ok(Err(LaunchFailure::UnknownApp(app))) => failure(
            StatusCode::BAD_REQUEST,
            "bad-request",
            format!("unknown or unavailable app: {app}"),
        ),
        Ok(Err(LaunchFailure::Failed(err))) => {
            eprintln!("codesmith desktop: open-in-app launch failed: {err}");
            failure(
                StatusCode::BAD_GATEWAY,
                "launch-failed",
                format!("failed to launch {app}"),
            )
        }
        Err(err) => failure(
            StatusCode::INTERNAL_SERVER_ERROR,
            "bad-request",
            format!("open-in-app join failed: {err}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vendored control's label table (`client-ui-open-in-app`
    /// `APPLICATIONS`) — an id outside it is filtered out of the menu, so the
    /// host must never advertise one.
    const CLIENT_APP_IDS: &[&str] = &[
        "finder",
        "explorer",
        "filemanager",
        "cursor",
        "vscode",
        "vscodeinsiders",
        "windsurf",
        "zed",
        "sublimetext",
        "xcode",
        "androidstudio",
        "intellij",
        "pycharm",
        "webstorm",
        "phpstorm",
        "goland",
        "rider",
        "rustrover",
        "fork",
        "sourcetree",
        "github",
        "tower",
        "gitkraken",
        "smartgit",
        "sublimemerge",
        "ghostty",
        "warp",
        "iterm",
        "kitty",
        "terminal",
        "windowsterminal",
        "gitbash",
        "gnometerminal",
        "konsole",
    ];

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cds-open-in-app-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn catalog_ids_are_a_subset_of_the_client_label_table() {
        let mut ids: Vec<&str> = vec!["finder", "explorer", "filemanager", "terminal"];
        ids.extend(MAC_APPS.iter().map(|app| app.id));
        ids.extend(LINUX_APPS.iter().map(|app| app.id));
        ids.extend(WINDOWS_APPS.iter().map(|app| app.id));
        for id in ids {
            assert!(
                CLIENT_APP_IDS.contains(&id),
                "{id} is not in the vendored control's label table and would be filtered out"
            );
        }
        // A catalog id must not repeat within one platform's menu.
        for catalog in [
            LINUX_APPS.iter().map(|a| a.id).collect::<Vec<_>>(),
            WINDOWS_APPS.iter().map(|a| a.id).collect::<Vec<_>>(),
        ] {
            let mut seen = std::collections::HashSet::new();
            for id in catalog {
                assert!(seen.insert(id), "duplicate catalog id {id}");
            }
        }
    }

    #[test]
    fn mac_bundle_lookup_prefers_the_first_existing_root_and_spelling() {
        let root = temp_dir("mac");
        let other = temp_dir("mac-other");
        std::fs::create_dir_all(other.join("Zed.app")).unwrap();
        assert_eq!(
            mac_bundle_in(
                &[root.clone(), other.clone()],
                &["Zed.app", "Zed Preview.app"]
            ),
            Some(other.join("Zed.app"))
        );
        assert_eq!(
            mac_bundle_in(std::slice::from_ref(&root), &["Zed.app"]),
            None,
            "a missing bundle is not advertised"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn launch_args_substitute_the_token_or_append_the_directory() {
        let args = vec!["--cd={path}".to_string()];
        assert_eq!(launch_args(&args, "/w/x"), vec!["--cd=/w/x"]);
        let args = vec!["--workdir".to_string(), "{path}".to_string()];
        assert_eq!(launch_args(&args, "/w/x"), vec!["--workdir", "/w/x"]);
        let args = vec!["--new-window".to_string()];
        assert_eq!(launch_args(&args, "/w/x"), vec!["--new-window", "/w/x"]);
    }

    #[test]
    fn secret_env_names_are_scrubbed() {
        for name in [
            "OPENAI_API_KEY",
            "CODESMITH_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "DB_PASSWORD",
            "GOOGLE_APPLICATION_CREDENTIALS",
        ] {
            assert!(is_secret_env(name), "{name} must be scrubbed");
        }
        for name in [
            "PATH",
            "HOME",
            "SystemRoot",
            "CODESMITH_DESKTOP_CODESMITH",
            "TERM",
        ] {
            assert!(!is_secret_env(name), "{name} must survive");
        }
    }

    #[test]
    fn fenced_directory_admits_inside_paths_and_refuses_escapes() {
        let root = temp_dir("fence-root");
        let inside = root.join("inside");
        let outside = temp_dir("fence-outside");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::write(root.join("file.txt"), b"x").unwrap();

        let canonical_inside = std::fs::canonicalize(&inside).unwrap();
        assert_eq!(
            fenced_directory(std::slice::from_ref(&root), &inside).unwrap(),
            canonical_inside
        );
        assert!(matches!(
            fenced_directory(std::slice::from_ref(&root), &root.join("file.txt")),
            Err(LaunchFailure::NotFound)
        ));
        assert!(matches!(
            fenced_directory(std::slice::from_ref(&root), &outside),
            Err(LaunchFailure::Outside(_))
        ));
        assert!(matches!(
            fenced_directory(std::slice::from_ref(&root), &root.join("missing")),
            Err(LaunchFailure::NotFound)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_catalog_always_offers_finder_and_a_terminal() {
        let apps = available_apps();
        assert!(apps.contains(&"finder"), "{apps:?}");
        assert!(
            apps.contains(&"terminal"),
            "Terminal ships with macOS: {apps:?}"
        );
        // Menu order: the file manager leads.
        assert_eq!(apps.first(), Some(&"finder"));
    }

    #[tokio::test]
    async fn apps_payload_wraps_the_catalog() {
        let value = apps().await.0;
        assert!(
            value["apps"].is_array(),
            "the client reads {{apps: string[]}}: {value}"
        );
        for id in value["apps"].as_array().unwrap() {
            assert!(
                id.as_str().is_some_and(|id| CLIENT_APP_IDS.contains(&id)),
                "the client filters unknown ids out of the menu: {id}"
            );
        }
    }
}
