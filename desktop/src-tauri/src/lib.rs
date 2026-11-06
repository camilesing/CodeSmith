//! CodeSmith Desktop — Tauri shell.
//!
//! Known limitations (spec docs/superpowers/specs/2026-11-06-codesmith-desktop-design.md):
//! the bundled UI is a vendored copy of the DSH client; it is synced with
//! upstream by desktop/scripts/vendor_dsh_client.py, not tracked live.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
