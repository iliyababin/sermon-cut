#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod youtube_dl;
mod ffmpeg;
mod library;
mod modal;
mod state;
mod thumbnail;
mod tools;
mod youtube;

use library::runtime::Library;
use state::AppState;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

fn main() {
    tools::extend_path();

    let app_state = AppState::load();
    let settings = app_state.settings.clone();
    let app_state = Arc::new(Mutex::new(app_state));
    let library = Library::new(settings.library_folder());

    let app = tauri::Builder::default()
        // First: a second copy would write the same library.json.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .setup({
            let library = library.clone();
            let app_state = app_state.clone();
            move |app| {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(tools::update_ytdlp_if_stale());
                tauri::async_runtime::spawn({
                    let library = library.clone();
                    let app_state = app_state.clone();
                    let handle = handle.clone();
                    async move {
                        if let Err(e) = library.rescan(&app_state).await {
                            eprintln!("[library] First scan failed: {}", e);
                        }
                        commands::allow_user_paths(&handle, library.root().as_deref(), &settings);
                        let _ = tauri::Emitter::emit(&handle, "library-changed", ());
                    }
                });
                tauri::async_runtime::spawn(library.clone().worker(handle.clone(), app_state.clone()));
                tauri::async_runtime::spawn({
                    let library = library.clone();
                    async move {
                        let mut tick = tokio::time::interval(std::time::Duration::from_secs(library::lock::HEARTBEAT_SECS));
                        loop {
                            tick.tick().await;
                            let lib = library.clone();
                            let reopen = tokio::task::spawn_blocking(move || lib.heartbeat()).await.unwrap_or(false);
                            if reopen {
                                if let Err(e) = library.reopen().await {
                                    eprintln!("[library] Reopen failed: {}", e);
                                }
                                let _ = tauri::Emitter::emit(&handle, "library-changed", ());
                            }
                        }
                    }
                });
                Ok(())
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(app_state)
        .manage(library.clone())
        .invoke_handler(tauri::generate_handler![
            commands::get_video_info,
            commands::get_settings,
            commands::save_settings,
            commands::reset_state,
            commands::get_app_data_dir,
            commands::read_image_base64,
            commands::open_file,
            commands::open_folder,
            commands::youtube_get_auth_status,
            commands::youtube_sign_in,
            commands::youtube_sign_out,
            commands::youtube_list_playlists,
            library::commands::get_library,
            library::commands::rescan_library,
            library::commands::get_video,
            library::commands::process_video,
            library::commands::cancel_processing,
            library::commands::reset_video,
            library::commands::hide_video,
            library::commands::trash_video,
            library::commands::forget_video,
            library::commands::save_review,
            library::commands::regenerate_thumbnails,
            library::commands::process_custom_thumbnail,
            library::commands::save_rendered_thumbnail,
            commands::crop_standalone_thumbnail,
            commands::save_standalone_thumbnail,
            commands::copy_file,
            library::commands::reveal_video,
            library::commands::open_library_folder,
            library::commands::dismiss_notices,
            library::commands::take_over_library,
            library::commands::clear_library_data,
            library::commands::download_video,
            library::commands::cancel_download,
            library::commands::youtube_upload_video,
            library::commands::check_library_folder,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(move |_app, event| {
        if let tauri::RunEvent::Exit = event {
            library.release_lock();
        }
    });
}
