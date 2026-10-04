//! Chaff — local-first, cross-platform photo culling workstation.
//!
//! This crate is the Tauri shell plus the application core. The core lives in
//! [`core`] and is deliberately Tauri-free so it can be unit-tested with a plain
//! `cargo test`, with no display server and no webview.

pub mod core;

/// Placeholder command retained from the scaffold so the IPC surface is wired and
/// testable end to end. Replaced by real commands as Phase 1 lands.
#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![app_version])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
