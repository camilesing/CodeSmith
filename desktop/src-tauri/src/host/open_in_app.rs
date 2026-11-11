//! The host `open-in-app` catalog and launch route.
//!
//! The vendored client's application control (`client-ui-open-in-app`) asks
//! the host origin for the applications installed on this machine
//! (`GET /open-in-app/apps`) and launches one on a workspace directory
//! (`POST /open-in-app/open`). CodeSmith has no application-specific opener:
//! its only native per-directory handoff is the operating system's file
//! manager, which is also what `session/openWorkspacePath` reveal uses
//! (`host::changes::open_changed_file`). The catalog therefore advertises
//! exactly that one application (`finder` / `explorer` / `filemanager`) and
//! nothing else.
//!
//! Launch validates the target first: an absolute existing directory inside
//! the bridge cwd, a registered workspace, or an imported thread workspace.
//! The client's controller opens workspace directories, never arbitrary
//! paths, but the route is reachable by any page origin the bridge serves, so
//! it keeps the same fence as the session open/reveal route.
//!
//! Known limitations: application discovery is the platform file manager
//! only — editors, IDEs, and terminals would need per-application locators
//! this shell does not maintain; icons are not served (the control falls back
//! to its own glyph when the icon request is absent).

use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::SharedHost;

/// The installed applications this shell can launch, in menu order. Finder,
/// Explorer, and `xdg-open` are the OS file-manager channels; a Linux host
/// without `xdg-open` advertises no application rather than a broken one.
pub fn available_apps() -> Vec<&'static str> {
    match std::env::consts::OS {
        "macos" => vec!["finder"],
        "windows" => vec!["explorer"],
        _ if which::which("xdg-open").is_ok() => vec!["filemanager"],
        _ => Vec::new(),
    }
}

/// `GET /open-in-app/apps` — the probed catalog ids.
pub async fn apps() -> Json<Value> {
    Json(json!({ "apps": available_apps() }))
}

enum LaunchFailure {
    NotFound,
    Outside(String),
    Failed(String),
}

fn failure(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (status, Json(json!({ "code": code, "message": message.into() }))).into_response()
}

/// `POST /open-in-app/open` — hand one workspace directory to the platform
/// file manager. The client only checks the HTTP status; a refusal is never
/// a 2xx.
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
    if !available_apps().contains(&app) {
        return failure(
            StatusCode::BAD_REQUEST,
            "bad-request",
            format!("unknown or unavailable app: {app}"),
        );
    }
    let requested = PathBuf::from(path);
    if !requested.is_absolute() {
        return failure(
            StatusCode::BAD_REQUEST,
            "bad-request",
            "path must be an absolute directory path",
        );
    }
    let roots = super::files::openable_roots(&host).await;
    let launched = tokio::task::spawn_blocking(move || {
        let canonical = std::fs::canonicalize(&requested).map_err(|_| LaunchFailure::NotFound)?;
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
        super::changes::open_changed_file("open", &canonical).map_err(LaunchFailure::Failed)
    })
    .await;
    match launched {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(LaunchFailure::NotFound)) => failure(
            StatusCode::NOT_FOUND,
            "not-found",
            format!("directory does not exist: {path}"),
        ),
        Ok(Err(LaunchFailure::Outside(path))) => failure(
            StatusCode::BAD_REQUEST,
            "bad-request",
            format!("{path} is outside every session workspace"),
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

    #[test]
    fn catalog_is_the_platform_file_manager() {
        let apps = available_apps();
        let expected = match std::env::consts::OS {
            "macos" => "finder",
            "windows" => "explorer",
            _ => "filemanager",
        };
        // Linux without xdg-open legitimately advertises nothing; the other
        // platforms always have their file manager.
        if !apps.is_empty() {
            assert_eq!(apps, vec![expected]);
        }
    }

    #[tokio::test]
    async fn apps_payload_wraps_the_catalog() {
        let value = apps().await.0;
        assert!(
            value["apps"].is_array(),
            "the client reads {{apps: string[]}}: {value}"
        );
    }
}