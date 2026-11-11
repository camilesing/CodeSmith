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
        // The native directory picker (`directoryPicker/pick`) uses the
        // dialog plugin's folder chooser; the bridge holds the AppHandle.
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let codesmith_path = std::env::var_os("CODESMITH_DESKTOP_CODESMITH").map(PathBuf::from);
            let port = std::env::var("CODESMITH_DESKTOP_BRIDGE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(BRIDGE_PORT);
            // The UI reads this global to reach the bridge, so an env-pinned
            // port reaches the webview instead of being silently ignored.
            let inject = format!("window.__CODESMITH_BRIDGE_PORT__ = {port};");
            // The staging the drag-drop handler fills and the bridge's
            // /api/host/dropBytes route serves (see `host::host_drop`); an
            // Arc from here because the window handler outlives the spawn
            // that builds HostState.
            let drop_bytes: host::host_drop::DropStaging = Default::default();
            if let Some(webview) = app.get_webview_window("main") {
                let _ = webview.eval(&inject);
                // With the native drag-drop enabled (the default), the DOM
                // never sees a file drop — wry claims it. The window event
                // carries the absolute paths; the page bridge turns them into
                // the composer's own path-citing intake.
                let drop_webview = webview.clone();
                let drop_staging = std::sync::Arc::clone(&drop_bytes);
                webview.on_window_event(move |event| {
                    if let tauri::WindowEvent::DragDrop(event) = event {
                        match event {
                            // The hover state drives the page's synthetic
                            // dragenter/dragleave (the vendored drop overlay's
                            // own signal — the DOM never sees native drags).
                            tauri::DragDropEvent::Enter { .. } => {
                                let _ = drop_webview.eval(host::host_drop::hover_script(true));
                            }
                            tauri::DragDropEvent::Leave => {
                                let _ = drop_webview.eval(host::host_drop::hover_script(false));
                            }
                            tauri::DragDropEvent::Drop { paths, .. } => {
                                let paths = paths.clone();
                                let webview = drop_webview.clone();
                                let staging = std::sync::Arc::clone(&drop_staging);
                                tauri::async_runtime::spawn(async move {
                                    host::host_drop::deliver(&webview, &staging, paths).await;
                                });
                            }
                            _ => {}
                        }
                    }
                });
            }
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match host::codesmith::CodesmithProcess::spawn(codesmith_path.as_ref()).await {
                    Ok(backend) => {
                        let host = Arc::new(HostState {
                            backend,
                            app_handle: Some(app_handle),
                            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                            event_sinks: Default::default(),
                            live_sinks: Default::default(),
                            pending_inputs: Default::default(),
                            projection_clock: Default::default(),
                            model_choices: Default::default(),
                            waterfall_watchers: Default::default(),
                            uploads: Default::default(),
                            drop_bytes,
                            reference_index: Default::default(),
                            plan_handoffs: Default::default(),
                            plan_return_modes: Default::default(),
                            workspace_lock: Default::default(),
                            goal_overlays: Default::default(),
                            goal_overlays_loaded: Default::default(),
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
