//! Chaff — the Tauri shell.
//!
//! This crate is deliberately thin. All engine logic lives in [`chaff_core`], which has
//! no Tauri dependency and therefore builds and tests on a machine with no webview and
//! no GTK/WebKit development packages.
//!
//! Keep it that way: anything added here becomes unavailable to the fast test path.

/// Re-exported so the app and its integration tests share one engine instance.
pub use chaff_core;

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
