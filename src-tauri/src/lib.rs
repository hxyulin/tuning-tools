mod commands;

use std::sync::Arc;

use tauri::{Emitter, Manager};

/// Recording and stream state changes, as `studio_app::AppEvent`
const APP_EVENT: &str = "studio-app-event";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    studio_carriers::probe::init_hid_on_main_thread();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage::<commands::App>(Arc::new(studio_app::StudioApp::new()))
        .setup(|app| {
            let studio = app.state::<commands::App>().inner().clone();
            if let Ok(dir) = app.path().app_data_dir() {
                studio.set_recordings_dir(dir.join("recordings"));
            }
            let handle = app.handle().clone();
            studio.set_event_sink(Arc::new(move |event| {
                let _ = handle.emit(APP_EVENT, event);
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_elf,
            commands::symbol_children,
            commands::startup_elf_path,
            commands::list_probes,
            commands::search_chips,
            commands::session_connect,
            commands::session_disconnect,
            commands::session_set_watches,
            commands::session_set_rate,
            commands::session_request,
            commands::session_discard,
            commands::session_save,
            commands::list_serial_ports,
            commands::watchable_leaves,
            commands::session_task_states,
            commands::session_task_trace,
            commands::inspect_children,
            commands::node_metadata,
            commands::session_read_values,
            commands::recording_start,
            commands::recording_stop,
            commands::export_csv,
            commands::stream_start,
            commands::stream_stop,
            commands::app_state,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Close a recording's file and the stream's port before exiting
            if let tauri::RunEvent::Exit = event {
                let studio = app.state::<commands::App>();
                studio.disconnect();
                studio.stop_stream();
            }
        });
}
