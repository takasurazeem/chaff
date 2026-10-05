//! Chaff — the Tauri shell.
//!
//! This crate is deliberately thin. All engine logic lives in [`chaff_core`], which has
//! no Tauri dependency and therefore builds and tests on a machine with no webview and
//! no GTK/WebKit development packages.
//!
//! Keep it that way: anything added here becomes unavailable to the fast test path.

pub mod commands;
pub mod faces;

use tauri::Manager;

/// Re-exported so the app and its integration tests share one engine instance.
pub use chaff_core;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // **The folder picker.** The dependency and the JS package were both installed and
        // this line was missing, so `openFolderDialog()` invoked a command that did not
        // exist. The promise rejected, the rejection was outside a try/catch, and the
        // button did nothing at all — which is exactly how it behaved.
        //
        // Every plugin the frontend calls has to be registered here. A plugin that is
        // installed but not registered fails at runtime and only at runtime.
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // **Logging, to a file.**
            //
            // The engine reports what it did — how many photographs, how they resolved,
            // how many were scored — and until this existed those numbers were a struct
            // nobody read.
            //
            // To a **file**, not stderr. A windowed application launched from Finder has no
            // terminal, so stderr goes nowhere a person can look, and the first version of
            // this wrote to stderr and was invisible. The log lives beside the catalog,
            // where `cat` retrieves it and where it survives the window closing.
            //
            // `RUST_LOG` still filters it; the default is `info`, which is the level the
            // index report is written at.
            if let Ok(dir) = app.path().app_data_dir() {
                let _ = std::fs::create_dir_all(&dir);
                if let Ok(file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("chaff.log"))
                {
                    env_logger::Builder::from_env(
                        env_logger::Env::default().default_filter_or("info"),
                    )
                    .format_timestamp_millis()
                    .target(env_logger::Target::Pipe(Box::new(file)))
                    .init();
                }
            }

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
            commands::list_directories,
            commands::run_face_pass,
            commands::list_people,
            commands::person_photos,
            commands::face_counts,
            commands::photo_detail,
            commands::get_settings,
            commands::set_setting,
            commands::photo_thumbnail,
            commands::photo_explanation,
            commands::plan_delete,
            commands::commit_delete,
            commands::list_trash,
            commands::restore_trash,
            commands::purge_trash,
            commands::build_info,
            commands::capabilities,
            commands::set_decision,
            commands::decision_count,
            commands::trim_thumbnail_cache,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
