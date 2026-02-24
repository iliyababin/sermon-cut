#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod youtube_dl;
mod ffmpeg;
mod modal;
mod state;
mod thumbnail;
mod youtube;

use state::AppState;
use std::sync::Arc;
use tokio::sync::Mutex;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(Arc::new(Mutex::new(AppState::load())))
        .invoke_handler(tauri::generate_handler![
            commands::download_video,
            commands::get_video_info,
            commands::extract_audio,
            commands::transcribe_audio,
            commands::trim_video,
            commands::get_videos,
            commands::delete_video,
            commands::get_download_progress,
            commands::cancel_download,
            commands::get_settings,
            commands::save_settings,
            commands::reset_state,
            commands::get_app_data_dir,
            commands::add_local_video,
            commands::generate_thumbnail,
            commands::generate_thumbnail_options,
            commands::read_image_base64,
            commands::open_file,
            commands::open_folder,
            commands::youtube_get_auth_status,
            commands::youtube_sign_in,
            commands::youtube_sign_out,
            commands::youtube_list_playlists,
            commands::youtube_upload_video,
            commands::process_video_full,
            commands::update_video_metadata,
            commands::retrim_video,
            commands::regenerate_thumbnails,
            commands::process_custom_thumbnail,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
