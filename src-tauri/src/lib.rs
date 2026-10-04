//! Chaff — the Tauri shell.
//!
//! This crate is deliberately thin. All engine logic lives in [`chaff_core`], which has
//! no Tauri dependency and therefore builds and tests on a machine with no webview and
//! no GTK/WebKit development packages.
//!
//! Keep it that way: anything added here becomes unavailable to the fast test path.

pub mod commands;

use tauri::Manager;

/// Re-exported so the app and its integration tests share one engine instance.
pub use chaff_core;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // The catalog and the thumbnail cache live under the app data directory, never
            // inside the user's library. Nothing Chaff writes is written beside their
            // photographs unless they explicitly ask for a sidecar.
            let state = commands::initialise(app.handle())
                .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_library,
            commands::list_photos,
            commands::photo_thumbnail,
            commands::photo_explanation,
            commands::trim_thumbnail_cache,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
