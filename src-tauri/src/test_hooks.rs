//! Smoke-test hooks, compiled only with `--features test-hooks` in a debug
//! build. `SERMON_CUT_TEST_HOOK=<scenario>` drives the running app through
//! the real quit, restart and settings paths and appends what it saw to
//! `$HOME/hook.log`. Nothing here talks to YouTube or Modal.

use crate::library::runtime::Library;
use crate::state::AppState;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;

fn home() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_default()
}

fn log(line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(home().join("hook.log")) {
        let _ = writeln!(f, "[{}] {}", std::process::id(), line);
    }
}

fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window("main")
}

pub fn spawn(app: AppHandle, library: Arc<Library>, state: Arc<Mutex<AppState>>) {
    let Ok(mut scenario) = std::env::var("SERMON_CUT_TEST_HOOK") else {
        return;
    };
    // A relaunch inherits the environment; the relaunched copy just quits.
    let marker = home().join("hook.restarted");
    if scenario == "restart" && marker.exists() {
        scenario = "clean_quit".into();
    }
    tauri::async_runtime::spawn(async move {
        // Let the first scan and the webview finish loading (new files need two scans 5 s apart).
        tokio::time::sleep(Duration::from_secs(15)).await;
        log(&format!("scenario {} started", scenario));
        match scenario.as_str() {
            "clean_quit" => {
                log(&format!("work_in_progress={:?}", library.work_in_progress()));
                // What the library screen (and the review screen, for
                // SERMON_CUT_HOOK_VIDEO) would get, through the real IPC.
                use tauri::Listener;
                app.listen("hook-result", |e| log(&format!("ipc result: {}", e.payload())));
                let video = serde_json::to_string(&std::env::var("SERMON_CUT_HOOK_VIDEO").unwrap_or_default()).unwrap_or_default();
                if let Some(w) = main_window(&app) {
                    let _ = w.eval(format!(
                        "(async () => {{ const i = window.__TAURI_INTERNALS__.invoke; const id = {video}; \
                         const call = async (c, a) => {{ try {{ return {{ ok: await i(c, a) }}; }} catch (e) {{ return {{ err: String(e) }}; }} }}; \
                         const out = {{ library: await call('get_library', {{}}), video: id ? await call('get_video', {{ videoId: id }}) : null }}; \
                         await i('plugin:event|emit', {{ event: 'hook-result', payload: out }}); }})()"
                    ));
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                if let Some(w) = main_window(&app) {
                    let _ = w.close();
                }
                log("closed window");
            }
            "busy_close" => {
                library.hook_fake_upload("hook-fake-upload");
                log(&format!("work_in_progress={:?}", library.work_in_progress()));
                log(&format!("begin_update={:?}", library.begin_update()));
                if let Some(w) = main_window(&app) {
                    let _ = w.close();
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                log(&format!(
                    "after close: window_exists={} asking={}",
                    main_window(&app).is_some(),
                    crate::ASKING.load(Ordering::SeqCst)
                ));
                // A second quit while the dialog is up: waits for it.
                let again = crate::ask_before_quitting(&app, &library);
                log(&format!("second ask returned {} asking={}", again, crate::ASKING.load(Ordering::SeqCst)));
                if let Some(w) = main_window(&app) {
                    let _ = w.close();
                }
                app.exit(0);
                tokio::time::sleep(Duration::from_secs(2)).await;
                log(&format!(
                    "after second close and exit(0): still running, window_exists={} asking={}",
                    main_window(&app).is_some(),
                    crate::ASKING.load(Ordering::SeqCst)
                ));
            }
            "restart" => {
                let _ = std::fs::write(&marker, b"");
                library.hook_fake_upload("hook-fake-upload");
                log(&format!("work_in_progress={:?}", library.work_in_progress()));
                // The updater's relaunch, through the webview like the real one.
                if let Some(w) = main_window(&app) {
                    let r = w.eval("window.__TAURI_INTERNALS__.invoke('plugin:process|restart')");
                    log(&format!("restart invoked: {:?}", r));
                }
            }
            "settings" => {
                // The Settings screen's save, through the real IPC command.
                use tauri::Listener;
                app.listen("hook-result", |e| log(&format!("save_settings result: {}", e.payload())));
                if let Some(w) = main_window(&app) {
                    let r = w.eval(
                        "(async () => { const i = window.__TAURI_INTERNALS__.invoke; \
                         const save = async (patch, seq) => { try { return { ok: await i('save_settings', { patch, seq }) }; } catch (e) { return { err: String(e) }; } }; \
                         const out = { clobber: await save({ youtube_auth: null }, 1000), \
                                       edit: await save({ download_quality: '720p', modal_api_key: '' }, 1001) }; \
                         await i('plugin:event|emit', { event: 'hook-result', payload: out }); })()",
                    );
                    log(&format!("save_settings invoked: {:?}", r));
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                {
                    let s = state.lock().await;
                    log(&format!(
                        "in memory: download_quality={} refresh_token={:?}",
                        s.settings.download_quality,
                        s.settings.youtube_auth.as_ref().and_then(|a| a.refresh_token.clone())
                    ));
                }
                if let Some(w) = main_window(&app) {
                    let _ = w.close();
                }
            }
            other => log(&format!("unknown scenario {}", other)),
        }
    });
}
