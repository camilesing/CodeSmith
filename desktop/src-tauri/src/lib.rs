//! CodeSmith Desktop — Tauri shell.
//!
//! Owns the backend child process (`codesmith serve --http`) and the dsh
//! RPC bridge on 127.0.0.1:1421 that the vendored DSH client talks to.
//!
//! Known limitations (spec docs/superpowers/specs/2026-11-06-codesmith-desktop-design.md):
//! the bundled UI is a vendored copy of the DSH client; it is synced with
//! upstream by desktop/scripts/vendor_dsh_client.py, not tracked live.

mod host;

use std::path::PathBuf;
use std::sync::Arc;

use host::{HostState, serve_bridge};

/// Bridge port; overridable for parallel dev instances.
const BRIDGE_PORT: u16 = 1421;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|_app| {
            let codesmith_path = std::env::var_os("CODESMITH_DESKTOP_CODESMITH").map(PathBuf::from);
            let port = std::env::var("CODESMITH_DESKTOP_BRIDGE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(BRIDGE_PORT);
            tauri::async_runtime::spawn(async move {
                match host::codesmith::CodesmithProcess::spawn(codesmith_path.as_ref()).await {
                    Ok(backend) => {
                        let host = Arc::new(HostState {
                            backend,
                            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                            client_id: uuid::Uuid::new_v4().to_string(),
                            event_sinks: Default::default(),
                            live_sinks: Default::default(),
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
