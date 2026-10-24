#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod youtube_dl;
mod ffmpeg;
mod library;
mod logging;
mod modal;
mod notify;
mod state;
#[cfg(all(feature = "test-hooks", debug_assertions))]
mod test_hooks;
mod thumbnail;
mod tools;
mod upload;
mod youtube;

use library::runtime::{HeartbeatOutcome, Library};
use state::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::sync::Mutex;

/// Set once the user chose "Quit Anyway", so the exit that follows isn't asked about again.
static QUIT_CONFIRMED: AtomicBool = AtomicBool::new(false);
/// The quit dialog is showing: another ⌘Q or close waits for it instead of
/// opening a second one.
static ASKING: AtomicBool = AtomicBool::new(false);

/// The webview has saved its pending edits (or was given up on) for the
/// close or quit under way; cleared if the user then keeps working.
static FLUSHED: AtomicBool = AtomicBool::new(false);
/// Waiting for the webview's "flush-done"; another close or quit leaves it
/// to the one already waiting.
static FLUSHING: AtomicBool = AtomicBool::new(false);
/// How long a close or quit waits for the webview to save.
const FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Before closing or quitting, ask the webview to save pending edits
/// ("flush-before-exit") and run `then` once it answers ("flush-done") or
/// FLUSH_TIMEOUT passes. Returns true if `then` will run later, so the
/// caller should hold the close or quit back; false if the flush is done.
/// Nothing blocks the event loop and no lock is held while waiting.
fn flush_first(app: &AppHandle, then: impl FnOnce(&AppHandle) + Send + 'static) -> bool {
    if FLUSHED.load(Ordering::SeqCst) {
        return false;
    }
    if FLUSHING.swap(true, Ordering::SeqCst) {
        return true;
    }
    let (done, answered) = tokio::sync::oneshot::channel::<()>();
    let listener = tauri::Listener::once(app, "flush-done", move |_| {
        let _ = done.send(());
    });
    if let Err(e) = tauri::Emitter::emit(app, "flush-before-exit", ()) {
        log::warn!("Couldn't ask the window to save: {}", e);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if tokio::time::timeout(FLUSH_TIMEOUT, answered).await.is_err() {
            log::warn!("The window didn't confirm saving in time; going ahead");
        }
        tauri::Listener::unlisten(&app, listener);
        FLUSHED.store(true, Ordering::SeqCst);
        FLUSHING.store(false, Ordering::SeqCst);
        then(&app);
    });
    true
}

/// Quit, unless there's work to ask about first.
fn quit(app: &AppHandle, library: &Library) {
    if !ask_before_quitting(app, library) {
        app.exit(0);
    }
}

/// Before quitting mid-upload (or mid-processing/download), ask first.
/// Returns true if quitting should wait for the user's answer.
fn ask_before_quitting(app: &AppHandle, library: &Library) -> bool {
    if QUIT_CONFIRMED.load(Ordering::SeqCst) {
        return false;
    }
    let Some(work) = library.work_in_progress() else {
        return false;
    };
    if ASKING.swap(true, Ordering::SeqCst) {
        // Already asking; that answer decides.
        return true;
    }
    log::info!("Quit requested while working on {}; asking first", work);
    let app = app.clone();
    app.dialog()
        .message(format!(
            "Sermon Cut is still working on {}. Quitting now stops it. Uploads continue where they left off next time; processing and downloads start over.",
            work
        ))
        .title("Quit Sermon Cut?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Quit Anyway".into(), "Keep Working".into()))
        .show({
            let app = app.clone();
            move |quit| {
                ASKING.store(false, Ordering::SeqCst);
                log::info!("{}", if quit { "Quitting anyway" } else { "Kept working instead of quitting" });
                if !quit {
                    // Kept working: the next close or quit saves again.
                    FLUSHED.store(false, Ordering::SeqCst);
                }
                if quit {
                    QUIT_CONFIRMED.store(true, Ordering::SeqCst);
                    app.exit(0);
                }
            }
        });
    true
}

fn main() {
    logging::install_panic_hook();
    tools::extend_path();

    let app_state = AppState::load();
    let settings = app_state.settings.clone();
    let app_state = Arc::new(Mutex::new(app_state));
    let library = Library::new(settings.library_folder_or_default());

    let app = tauri::Builder::default()
        // First: a second copy would write the same library.json.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        // Then the log, before anything that might write to it. After the
        // single-instance check, so a second copy never opens (or rotates)
        // the log file the first one is writing.
        .plugin(logging::plugin())
        .plugin(logging::ready_plugin(settings.library_folder_or_default()))
        .setup({
            let library = library.clone();
            let app_state = app_state.clone();
            move |app| {
                let handle = app.handle().clone();
                #[cfg(target_os = "macos")]
                {
                    use tauri::menu::{Menu, MenuItem, MenuItemKind};
                    let menu = Menu::default(&handle)?;
                    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
                        // The default app menu ends with the built-in Quit.
                        let count = app_menu.items()?.len();
                        if count > 0 {
                            app_menu.remove_at(count - 1)?;
                        }
                        app_menu.append(&MenuItem::with_id(&handle, "quit", "Quit Sermon Cut", true, Some("CmdOrCtrl+Q"))?)?;
                    }
                    app.set_menu(menu)?;
                }
                tauri::async_runtime::spawn(tools::update_ytdlp_if_stale());
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                test_hooks::spawn(handle.clone(), library.clone(), app_state.clone());
                tauri::async_runtime::spawn({
                    let library = library.clone();
                    let app_state = app_state.clone();
                    let handle = handle.clone();
                    async move {
                        if let Err(e) = library.rescan(&app_state).await {
                            log::error!("First scan failed: {}", e);
                        }
                        commands::allow_user_paths(&handle, library.root().as_deref(), &settings);
                        let _ = tauri::Emitter::emit(&handle, "library-changed", ());
                        notify::refresh_badge(&handle);
                    }
                });
                tauri::async_runtime::spawn(library.clone().worker(handle.clone(), app_state.clone()));
                // With auto-process on, new videos are picked up even while
                // the library screen isn't showing (it scans on its own then).
                tauri::async_runtime::spawn({
                    let library = library.clone();
                    let app_state = app_state.clone();
                    let handle = handle.clone();
                    async move {
                        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
                        let mut failures = logging::FailureTracker::new(module_path!(), "The auto-process scan");
                        loop {
                            tick.tick().await;
                            if !app_state.lock().await.settings.auto_process {
                                continue;
                            }
                            match library.rescan(&app_state).await {
                                Ok(()) => failures.log_recovered(),
                                Err(e) => failures.log_failure(e),
                            }
                            let _ = tauri::Emitter::emit(&handle, "library-changed", ());
                            notify::refresh_badge(&handle);
                        }
                    }
                });
                tauri::async_runtime::spawn({
                    let library = library.clone();
                    async move {
                        let mut tick = tokio::time::interval(std::time::Duration::from_secs(library::lock::HEARTBEAT_SECS));
                        loop {
                            tick.tick().await;
                            match library.heartbeat().await {
                                HeartbeatOutcome::Unchanged => continue,
                                HeartbeatOutcome::LostTo(host) => {
                                    log::warn!("{} took over the library; read-only now", host);
                                }
                                HeartbeatOutcome::Regained => {
                                    log::info!("The other computer let go of the library; reopening it");
                                    if let Err(e) = library.reopen().await {
                                        log::error!("Reopen failed: {}", e);
                                    }
                                }
                            }
                            let _ = tauri::Emitter::emit(&handle, "library-changed", ());
                        }
                    }
                });
                Ok(())
            }
        })
        // ⌘Q: macOS's built-in Quit item can't be stopped once chosen (tao only
        // hears about it after the fact), so the app menu gets its own Quit
        // item that asks first. Closing the window is handled below.
        .on_menu_event({
            let library = library.clone();
            move |app, event| {
                if event.id() == "quit" {
                    let later = library.clone();
                    if !flush_first(app, move |app| quit(app, &later)) {
                        quit(app, &library);
                    }
                }
            }
        })
        .on_window_event({
            let library = library.clone();
            move |window, event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    // Save first, then close again (which comes back here).
                    let again = window.clone();
                    if flush_first(window.app_handle(), move |_| {
                        let _ = again.close();
                    }) || ask_before_quitting(window.app_handle(), &library)
                    {
                        api.prevent_close();
                    }
                }
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .manage(app_state)
        .manage(youtube::SignInSlot::default())
        .manage(library.clone())
        .invoke_handler(tauri::generate_handler![
            commands::get_video_info,
            commands::get_settings,
            commands::save_settings,
            commands::reset_state,
            commands::get_app_folders,
            commands::read_image_base64,
            commands::open_file,
            commands::open_folder,
            commands::youtube_get_auth_status,
            commands::youtube_sign_in,
            commands::youtube_sign_out,
            commands::youtube_list_playlists,
            library::commands::get_library,
            library::commands::rescan_library,
            library::commands::get_storage,
            library::commands::get_video,
            library::commands::process_video,
            library::commands::reprocess_video,
            library::commands::cancel_processing,
            library::commands::reset_video,
            library::commands::hide_video,
            library::commands::trash_video,
            library::commands::forget_video,
            library::commands::save_review,
            library::commands::regenerate_thumbnails,
            library::commands::process_custom_thumbnail,
            library::commands::save_rendered_thumbnail,
            library::commands::set_trim,
            library::commands::frame_still,
            commands::crop_standalone_thumbnail,
            commands::save_standalone_thumbnail,
            commands::copy_file,
            library::commands::reveal_video,
            library::commands::reveal_commit_backup,
            library::commands::open_library_folder,
            library::commands::dismiss_notices,
            library::commands::take_over_library,
            library::commands::clear_library_data,
            library::commands::download_video,
            library::commands::cancel_download,
            library::commands::begin_update,
            library::commands::end_update,
            library::commands::youtube_upload_video,
            library::commands::cancel_upload,
            library::commands::forget_upload_session,
            library::commands::youtube_update_metadata,
            library::commands::retry_thumbnail,
            library::commands::check_library_folder,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(move |app, event| match event {
        // Anything else that asks the app to exit (e.g. closing the last window).
        // A relaunch (after installing an update) goes through: no work can
        // be running, since begin_update refused to start otherwise.
        tauri::RunEvent::ExitRequested { code, .. } if code == Some(tauri::RESTART_EXIT_CODE) => {}
        tauri::RunEvent::ExitRequested { api, .. } => {
            let then = {
                let library = library.clone();
                move |app: &AppHandle| quit(app, &library)
            };
            if flush_first(app, then) || ask_before_quitting(app, &library) {
                api.prevent_exit();
            }
        }
        tauri::RunEvent::Exit => {
            log::info!("Quitting");
            library.release_lock();
        }
        _ => {}
    });
}
