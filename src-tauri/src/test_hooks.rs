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

/// Run `body` (the inside of an async JS function, with `i` = invoke and
/// `call(cmd, args, opts)` = invoke returning {ok} or {err}) in the webview
/// and return what it returned, as JSON.
async fn ipc(app: &AppHandle, body: &str) -> serde_json::Value {
    use std::sync::atomic::AtomicU32;
    use tauri::Listener;
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let event = format!("hook-ipc-{}", SEQ.fetch_add(1, Ordering::SeqCst));
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    app.once(event.clone(), move |e| {
        let _ = tx.send(e.payload().to_string());
    });
    let js = format!(
        "(async () => {{ const i = window.__TAURI_INTERNALS__.invoke; \
         const call = async (c, a, o) => {{ try {{ return {{ ok: await i(c, a, o) }}; }} catch (e) {{ return {{ err: String(e) }}; }} }}; \
         let out; try {{ out = await (async () => {{ {body} }})(); }} catch (e) {{ out = {{ thrown: String(e) }}; }} \
         await i('plugin:event|emit', {{ event: '{event}', payload: out === undefined ? null : out }}); }})()"
    );
    let Some(w) = main_window(app) else { return serde_json::json!({ "hook": "no window" }) };
    if let Err(e) = w.eval(js) {
        return serde_json::json!({ "hook": format!("eval failed: {}", e) });
    }
    match tokio::time::timeout(Duration::from_secs(60), rx).await {
        Ok(Ok(s)) => serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s)),
        _ => serde_json::json!({ "hook": "no answer" }),
    }
}

fn js(v: impl serde::Serialize) -> String {
    serde_json::to_string(&v).unwrap_or_default()
}

/// The render key the backend expects for `source` given this review.
fn key_for(dir: &std::path::Path, source: &str, title: &str, preacher: &str, logo: &str) -> String {
    let version = std::fs::metadata(dir.join(source))
        .map(|m| crate::library::fsutil::mtime_ms(&m).to_string())
        .unwrap_or_default();
    crate::library::manifest::render_key(TEMPLATE_VERSION, title, preacher, false, source, &version, logo)
}

/// src/templates/sccy-default.ts
const TEMPLATE_VERSION: u32 = 4;

/// Every recorded render's file exists, every render file on disk is
/// recorded, and no temp render is left. Returns the problems.
fn render_problems(dir: &std::path::Path) -> Vec<String> {
    let data = crate::library::manifest::load_video_data(dir);
    let mut problems = Vec::new();
    for r in &data.renders {
        if !dir.join(&r.file).is_file() {
            problems.push(format!("recorded render {} is missing", r.file));
        }
        if !data.thumbnail_options.contains(&r.source) {
            problems.push(format!("render {} is for a source that isn't an option", r.file));
        }
    }
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if crate::library::manifest::is_render_file(&name) && !data.renders.iter().any(|r| r.file == name) {
            problems.push(format!("render file {} isn't recorded", name));
        }
        if name.starts_with(crate::library::manifest::RENDER_TMP_PREFIX) {
            problems.push(format!("temp render {} left", name));
        }
    }
    problems
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
            "render_race" => render_race(&app, &library).await,
            "regain" => regain(&app, &library).await,
            "logo" => logo(&app, &library, &state).await,
            "close_flush" => close_flush(&app, &library).await,
            "logs" => logs(&app).await,
            other => log(&format!("unknown scenario {}", other)),
        }
    });
}

/// The webview's console.error reaches the log (masked), Show Logs opens
/// the folder and the diagnostics come back.
async fn logs(app: &AppHandle) {
    let out = ipc(
        app,
        "console.error('smoke: a webview error', 'ya29.smoke-token-0123456789'); \
         console.warn('smoke: a webview warning'); \
         await new Promise((r) => setTimeout(r, 500)); \
         return { show: await call('open_log_folder'), diagnostics: await call('diagnostics') };",
    )
    .await;
    log(&format!("open_log_folder: {}", out["show"]));
    let text = out["diagnostics"]["ok"].as_str().unwrap_or_default();
    log(&format!("diagnostics: {} lines; err={}", text.lines().count(), out["diagnostics"]["err"]));
    for line in text.lines().take(14) {
        log(&format!("  | {}", line));
    }
    log(&format!("diagnostics has the webview error: {}", text.contains("smoke: a webview error")));
    if let Some(w) = main_window(app) {
        let _ = w.close();
    }
}

fn video_dir(library: &Library, id: &str) -> std::path::PathBuf {
    library.root().unwrap_or_default().join(crate::library::DATA_DIR).join("videos").join(id)
}

/// A title edit racing a render: each round saves title A, then sends A's
/// render and a save of title B at once (and, in some rounds, B then A
/// again, or the render alone before the edit). Afterwards video.json and
/// the render files must agree.
async fn render_race(app: &AppHandle, library: &Library) {
    let id = std::env::var("SERMON_CUT_HOOK_VIDEO").unwrap_or_default();
    let dir = video_dir(library, &id);
    let data = crate::library::manifest::load_video_data(&dir);
    let Some(source) = data.thumbnail_options.first().cloned() else {
        log("CHECK render_race FAIL: no thumbnail options");
        return;
    };
    let preacher = "Pastor Hook";
    let image = std::fs::read(dir.join(&source)).unwrap_or_default();
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&image);
    let mut failures = Vec::new();
    let (mut accepted, mut refused) = (0, 0);
    for round in 0..20 {
        let a = format!("Race A {}", round);
        let b = format!("Race B {}", round);
        let key = key_for(&dir, &source, &a, preacher, "");
        let mode = round % 4;
        // Mode 3 sends the edit a little after the render starts, so it
        // lands while the image is being written.
        let delay = 10 * (round / 4 + 1) * (round / 4 + 1);
        let body = format!(
            "const id = {id}; const bytes = Uint8Array.from(atob({b64}), (c) => c.charCodeAt(0)); \
             const review = (title) => call('save_review', {{ videoId: id, title, description: 'd', preacher: {preacher}, singleLine: false, selectedSource: {source} }}); \
             const render = () => call('save_rendered_thumbnail', bytes, {{ headers: {{ 'x-video-id': encodeURIComponent(id), 'x-source': encodeURIComponent({source}), 'x-render-key': encodeURIComponent({key}), 'x-template-version': '{TEMPLATE_VERSION}' }} }}); \
             const first = await review({a}); \
             if ({mode} === 0) {{ const [r, s] = await Promise.all([render(), review({b})]); return {{ first, render: r, edit: s }}; }} \
             if ({mode} === 1) {{ const [r, s] = await Promise.all([render(), (async () => {{ const x = await review({b}); const y = await review({a}); return [x, y]; }})()]); return {{ first, render: r, edit: s }}; }} \
             if ({mode} === 3) {{ const r = render(); await new Promise((res) => setTimeout(res, {delay})); const s = await review({b}); return {{ first, render: await r, edit: s }}; }} \
             const r = await render(); const s = await review({b}); return {{ first, render: r, edit: s }};",
            id = js(&id),
            b64 = js(&b64),
            preacher = js(preacher),
            source = js(&source),
            key = js(&key),
            a = js(&a),
            b = js(&b),
        );
        let out = ipc(app, &body).await;
        let ok = out.get("render").and_then(|r| r.get("ok")).is_some();
        if ok { accepted += 1 } else { refused += 1 }
        log(&format!("round {} mode {}: render {}", round, mode, out.get("render").map(|r| r.to_string()).unwrap_or_else(|| out.to_string())));
        let problems = render_problems(&dir);
        let data = crate::library::manifest::load_video_data(&dir);
        let want = if mode == 1 { &a } else { &b };
        if data.review.title.as_deref() != Some(want.as_str()) {
            failures.push(format!("round {}: title on disk {:?}, expected {}", round, data.review.title, want));
        }
        // A render that was accepted stays recorded (the overlay is kept
        // until it's drawn again), even after the title changed.
        if ok && data.render_for(&source).is_none() {
            failures.push(format!("round {}: accepted render isn't recorded", round));
        }
        failures.extend(problems.into_iter().map(|p| format!("round {}: {}", round, p)));
    }
    let view = ipc(app, &format!("return await call('get_video', {{ videoId: {} }});", js(&id))).await;
    log(&format!("get_video ok={} title={}", view.get("ok").is_some(), view.pointer("/ok/review/title").map(|v| v.to_string()).unwrap_or_default()));
    let data = crate::library::manifest::load_video_data(&dir);
    log(&format!("video.json renders={:?} selected_thumbnail={:?}", data.renders.iter().map(|r| (&r.file, &r.render_key)).collect::<Vec<_>>(), data.selected_thumbnail));
    log(&format!("accepted={} refused={}", accepted, refused));
    if accepted == 0 || refused == 0 {
        failures.push("expected both accepted and refused renders".into());
    }
    if failures.is_empty() {
        log("CHECK render_race PASS");
    } else {
        log(&format!("CHECK render_race FAIL: {:?}", failures));
    }
    if let Some(w) = main_window(app) {
        let _ = w.close();
    }
}

fn library_summary(v: &serde_json::Value) -> String {
    let videos = v.pointer("/ok/videos").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let list: Vec<String> = videos
        .iter()
        .map(|x| format!("{}={}/{}", x["file_name"].as_str().unwrap_or("?"), x["status"].as_str().unwrap_or("?"), x["queue_position"]))
        .collect();
    format!("writable={} locked_by={} videos={:?}", v.pointer("/ok/writable").cloned().unwrap_or_default(), v.pointer("/ok/locked_by").cloned().unwrap_or_default(), list)
}

/// Started read-only (another Mac's fresh lock.json was planted); the other
/// Mac lets go, the heartbeat regains the lock, and the backlog must not be
/// queued. Then a file that really is new arrives and is.
async fn regain(app: &AppHandle, library: &Library) {
    let root = library.root().unwrap_or_default();
    let before = ipc(app, "return await call('get_library', {});").await;
    log(&format!("before: {}", library_summary(&before)));
    let read_only = before.pointer("/ok/writable") == Some(&serde_json::Value::Bool(false));
    let _ = std::fs::remove_file(root.join(crate::library::DATA_DIR).join("lock.json"));
    log("removed the other Mac's lock.json");
    let mut regained = false;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let v = ipc(app, "return await call('get_library', {});").await;
        if v.pointer("/ok/writable") == Some(&serde_json::Value::Bool(true)) {
            regained = true;
            log(&format!("regained: {}", library_summary(&v)));
            break;
        }
    }
    // Two auto-process scans (every 30 s).
    tokio::time::sleep(Duration::from_secs(70)).await;
    let after = ipc(app, "return await call('get_library', {});").await;
    log(&format!("after scans: {}", library_summary(&after)));
    let queued: Vec<String> = after
        .pointer("/ok/videos")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter(|x| !x["queue_position"].is_null() || !x["processing_stage"].is_null() || x["status"] == "error")
        .map(|x| x["file_name"].as_str().unwrap_or("?").to_string())
        .collect();
    let manifest = std::fs::read_to_string(root.join(crate::library::DATA_DIR).join("library.json")).unwrap_or_default();
    let m: serde_json::Value = serde_json::from_str(&manifest).unwrap_or_default();
    log(&format!("library.json queue={} auto_queued={:?}", m["queue"], m["entries"].as_array().into_iter().flatten().filter(|e| e["auto_queued"] == true).map(|e| e["file_name"].clone()).collect::<Vec<_>>()));
    let auto_queued = m["entries"].as_array().into_iter().flatten().any(|e| e["auto_queued"] == true);
    if read_only && regained && queued.is_empty() && !auto_queued && m["queue"].as_array().is_none_or(|q| q.is_empty()) {
        log("CHECK regain PASS (nothing queued)");
    } else {
        log(&format!("CHECK regain FAIL: read_only={} regained={} queued={:?}", read_only, regained, queued));
    }
    // Control: a file that arrives now is new, and is queued (processing
    // then fails at once: Modal is 127.0.0.1:9).
    if let Ok(src) = std::env::var("SERMON_CUT_HOOK_NEW_FILE") {
        let _ = std::fs::copy(&src, root.join("Arrived Now.mp4"));
        log("copied Arrived Now.mp4");
        tokio::time::sleep(Duration::from_secs(75)).await;
        let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join(crate::library::DATA_DIR).join("library.json")).unwrap_or_default()).unwrap_or_default();
        let arrived: Vec<_> = m["entries"].as_array().into_iter().flatten().filter(|e| e["file_name"] == "Arrived Now.mp4").map(|e| (e["status"].clone(), e["auto_queued"].clone())).collect();
        let others: Vec<_> = m["entries"].as_array().into_iter().flatten().filter(|e| e["file_name"] != "Arrived Now.mp4").map(|e| (e["file_name"].clone(), e["status"].clone(), e["auto_queued"].clone())).collect();
        log(&format!("control: arrived={:?} others={:?}", arrived, others));
    }
    if let Some(w) = main_window(app) {
        let _ = w.close();
    }
}

/// The logo from state.json (picked in an earlier session): in the asset
/// scope only if it's an image, and loadable by the webview.
async fn logo(app: &AppHandle, library: &Library, state: &Arc<Mutex<AppState>>) {
    let logo = state.lock().await.settings.logo_path.clone().unwrap_or_default();
    let scope = app.asset_protocol_scope();
    let allowed = scope.is_allowed(std::path::Path::new(&logo));
    let out = ipc(
        app,
        &format!(
            "const p = {p}; const url = window.__TAURI_INTERNALS__.convertFileSrc(p, 'asset'); \
             let fetched; try {{ const r = await fetch(url); const b = await r.arrayBuffer(); fetched = {{ status: r.status, bytes: b.byteLength, type: r.headers.get('content-type') }}; }} catch (e) {{ fetched = {{ err: String(e) }}; }} \
             const img = await new Promise((res) => {{ const el = new Image(); el.onload = () => res({{ loaded: el.naturalWidth + 'x' + el.naturalHeight }}); el.onerror = () => res({{ err: 'img error' }}); el.src = url; }}); \
             const read = await call('read_image_base64', {{ path: p }}); \
             return {{ url, img, fetched, read: read.ok ? {{ ok: read.ok.slice(0, 30) }} : read }};",
            p = js(&logo)
        ),
    )
    .await;
    log(&format!("logo={} asset_scope_allowed={} webview={}", logo, allowed, out));
    // With a video: open its review, which draws the selected thumbnail
    // with this logo, and say which render it recorded.
    if let Ok(id) = std::env::var("SERMON_CUT_HOOK_VIDEO").map(|v| v.trim().to_string()).and_then(|v| if v.is_empty() { Err(std::env::VarError::NotPresent) } else { Ok(v) }) {
        let opened = ipc(app, "const b = [...document.querySelectorAll('button')].find((x) => (x.getAttribute('aria-label') || '').startsWith('Review ')); if (!b) return 'no review button'; b.click(); return 'ok';").await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        let data = crate::library::manifest::load_video_data(&video_dir(library, &id));
        log(&format!("review opened: {}; renders now {:?}", opened, data.renders.iter().map(|r| (&r.file, &r.render_key)).collect::<Vec<_>>()));
    }
    if let Some(w) = main_window(app) {
        let _ = w.close();
    }
}

/// An edit typed into the review screen and the window closed before the
/// autosave (1 s) fires: the close must save it first.
async fn close_flush(app: &AppHandle, library: &Library) {
    let id = std::env::var("SERMON_CUT_HOOK_VIDEO").unwrap_or_default();
    let dir = video_dir(library, &id);
    let opened = ipc(
        app,
        "const b = [...document.querySelectorAll('button')].find((x) => (x.getAttribute('aria-label') || '').startsWith('Review ')); \
         if (!b) return 'no review button'; b.click(); return b.getAttribute('aria-label');",
    )
    .await;
    log(&format!("opened review: {}", opened));
    tokio::time::sleep(Duration::from_secs(4)).await;
    let typed = ipc(
        app,
        "const el = document.getElementById('review-title'); if (!el) return 'no title input'; \
         const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; \
         set.call(el, 'Flushed On Close'); el.dispatchEvent(new Event('input', { bubbles: true })); return el.value;",
    )
    .await;
    let typed_at = std::time::Instant::now();
    let on_disk = crate::library::manifest::load_video_data(&dir).review.title;
    log(&format!("typed: {}; title on disk before close: {:?}", typed, on_disk));
    if let Some(w) = main_window(app) {
        let _ = w.close();
    }
    log(&format!("close sent {} ms after typing", typed_at.elapsed().as_millis()));
    // The autosave would fire 1000 ms after typing; the flush saves sooner.
    for _ in 0..200 {
        if crate::library::manifest::load_video_data(&dir).review.title.as_deref() == Some("Flushed On Close") {
            log(&format!("edit on disk {} ms after typing", typed_at.elapsed().as_millis()));
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
