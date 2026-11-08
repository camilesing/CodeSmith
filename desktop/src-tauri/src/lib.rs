//! CodeSmith Desktop — Tauri shell.
//!
//! Owns the backend child process (`codesmith serve --http`) and the client
//! RPC bridge on 127.0.0.1:1421 that the vendored client UI talks to.
//!
//! Known limitations (see docs/DESKTOP.md):
//! the bundled UI is a vendored, mechanically rebranded copy of the upstream
//! client; it is synced by desktop/scripts/vendor_client.py, not tracked
//! live.

mod host;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::Manager;

use host::{serve_bridge, HostState};

/// Bridge port; overridable for parallel dev instances.
const BRIDGE_PORT: u16 = 1421;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let codesmith_path = std::env::var_os("CODESMITH_DESKTOP_CODESMITH").map(PathBuf::from);
            let port = std::env::var("CODESMITH_DESKTOP_BRIDGE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(BRIDGE_PORT);
            // The UI reads this global to reach the bridge, so an env-pinned
            // port reaches the webview instead of being silently ignored.
            let inject = format!("window.__CODESMITH_BRIDGE_PORT__ = {port};");
            if let Some(webview) = app.get_webview_window("main") {
                let _ = webview.eval(&inject);
            }
            tauri::async_runtime::spawn(async move {
                match host::codesmith::CodesmithProcess::spawn(codesmith_path.as_ref()).await {
                    Ok(backend) => {
                        let host = Arc::new(HostState {
                            backend,
                            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                            event_sinks: Default::default(),
                            live_sinks: Default::default(),
                            pending_inputs: Default::default(),
                            projection_clock: Default::default(),
                            model_choices: Default::default(),
                            waterfall_watchers: Default::default(),
                            uploads: Default::default(),
                        });
                        if let Err(err) = serve_bridge(host, port).await {
                            eprintln!("codesmith desktop: bridge failed: {err:#}");
                        }
                    }
                    Err(err) => {
                        eprintln!("codesmith desktop: backend spawn failed: {err:#}");
                        // Fail loud in the window too — a UI without its
                        // backend is a demo, not the product.
                        eprintln!("codesmith desktop: exiting (backend unavailable)");
                        std::process::exit(1);
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
