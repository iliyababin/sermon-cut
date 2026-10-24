//! The app's log file, and what must never be in it.
//!
//! Lines go to the platform's log folder (on macOS
//! ~/Library/Logs/com.youthscc.sermon-cut/sermon-cut.log), rotated at 5 MB
//! with four older files kept. Not the library's folder: that one is shared
//! between computers and may be unplugged.
//!
//! Every line passes through `redact` on its way out: secrets the app holds
//! (registered with `register_secret`) and anything shaped like a Google
//! token, auth code or upload id are replaced. That's a backstop: call
//! sites still never log response bodies, transcripts, tokens or session
//! URIs (docs/INVARIANTS.md).

use regex::Regex;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{Manager, Runtime};

/// The log file's name, without ".log".
pub const FILE_NAME: &str = "sermon-cut";
/// Rotate the active file when it would pass this size.
const MAX_FILE_SIZE: u128 = 5 * 1024 * 1024;
/// Older files kept next to the active one.
const KEEP_FILES: usize = 4;
/// Crates that are chatty at info; only their warnings are kept.
const NOISY: [&str; 7] = ["tao", "wry", "hyper", "hyper_util", "reqwest", "rustls", "h2"];

const REDACTED: &str = "<redacted>";

/// Set once the logger exists (in the plugin's setup). Before that, the
/// panic hook and `early` write to stderr only.
static READY: OnceLock<()> = OnceLock::new();

/// The log plugin, with our format and redaction. Register it before any
/// plugin that might log.
pub fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    use tauri_plugin_log::{Target, TargetKind};
    let mut builder = builder(Target::new(TargetKind::LogDir { file_name: Some(FILE_NAME.into()) }), MAX_FILE_SIZE, KEEP_FILES)
        .level(if cfg!(debug_assertions) { log::LevelFilter::Debug } else { log::LevelFilter::Info });
    for name in NOISY {
        builder = builder.level_for(name, log::LevelFilter::Warn);
    }
    if cfg!(debug_assertions) {
        builder = builder.target(Target::new(TargetKind::Stdout));
    }
    builder.build()
}

/// The plugin's builder writing to `file`, rotated at `max_size` keeping
/// `keep` older files. Apart from `plugin` so tests can rotate small files.
fn builder(file: tauri_plugin_log::Target, max_size: u128, keep: usize) -> tauri_plugin_log::Builder {
    use tauri_plugin_log::{RotationStrategy, TimezoneStrategy};
    tauri_plugin_log::Builder::new()
        .clear_targets()
        .target(file)
        .max_file_size(max_size)
        .rotation_strategy(RotationStrategy::KeepSome(keep))
        // Names the rotated files; set before `format`, which it replaces.
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .format(|out, message, record| {
            out.finish(format_args!(
                "{}",
                format_line(&chrono::Local::now(), record.level(), record.target(), &message.to_string())
            ))
        })
}

/// Runs right after the log plugin's setup: marks the logger ready and
/// writes the start line and anything logged before it existed.
pub fn ready_plugin<R: Runtime>(library_folder: String) -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("app-log")
        .setup(move |app, _api| {
            let _ = READY.set(());
            log::info!("{}", start_line(app.package_info().version.to_string(), &library_folder));
            for (level, message) in std::mem::take(&mut *EARLY.lock().unwrap_or_else(|p| p.into_inner())) {
                log::log!(level, "{}", message);
            }
            Ok(())
        })
        .build()
}

fn start_line(version: String, library_folder: &str) -> String {
    let os = os_info::get();
    let folder = Path::new(library_folder);
    let state = match std::fs::metadata(folder) {
        Ok(m) if m.is_dir() && m.permissions().readonly() => "read-only",
        Ok(m) if m.is_dir() => "found",
        Ok(_) => "not a folder",
        Err(_) => "missing",
    };
    format!(
        "Sermon Cut {} started ({} build) on {} {} ({}); library folder {} ({})",
        version,
        if cfg!(debug_assertions) { "debug" } else { "release" },
        os.os_type(),
        os.version(),
        os.architecture().unwrap_or("unknown"),
        library_folder,
        state
    )
}

/// Messages from before the logger existed (loading state.json happens
/// before the app is built), written once it does.
static EARLY: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());

/// Log `message`, or keep it for the log if the logger isn't there yet.
/// Always also written to stderr then, in case the app never gets that far.
pub fn early(level: log::Level, message: String) {
    if READY.get().is_some() {
        log::log!(level, "{}", message);
    } else {
        eprintln!("{}", redact(&message));
        EARLY.lock().unwrap_or_else(|p| p.into_inner()).push((level, message));
    }
}

/// Panics go to the log once it exists, and always to stderr. Install
/// first thing in `main`.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if READY.get().is_some() {
            let thread = std::thread::current();
            log::error!(
                "Panic in thread {}: {}\n{}",
                thread.name().unwrap_or("unnamed"),
                info,
                std::backtrace::Backtrace::force_capture()
            );
        }
        default(info);
    }));
}

/// One log line: local time with its UTC offset (so a fallback to UTC
/// shows), level, module and the redacted message. Lines after the first
/// of a message are indented.
pub fn format_line(now: &chrono::DateTime<chrono::Local>, level: log::Level, target: &str, message: &str) -> String {
    let target = match target {
        "sermon_cut" => "app",
        t => t.strip_prefix("sermon_cut::").unwrap_or(t),
    };
    let message = redact(message);
    format!(
        "{} {:<5} {}: {}",
        now.format("%Y-%m-%d %H:%M:%S%:z"),
        level,
        target,
        message.trim_end().replace('\n', "\n    ")
    )
}

// ---------------------------------------------------------------------------
// Redaction

/// Stands in for a secret in a hand-written `Debug`: types that hold
/// secrets never derive it (docs/INVARIANTS.md).
pub struct Hidden;

impl std::fmt::Debug for Hidden {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

/// `Some(<redacted>)` or `None`, for an optional secret.
pub fn hidden<T>(secret: &Option<T>) -> Option<Hidden> {
    secret.as_ref().map(|_| Hidden)
}

/// Secrets the app holds right now, replaced wherever they appear. Newest
/// last; the oldest go past MAX_KNOWN.
static KNOWN: Mutex<Vec<String>> = Mutex::new(Vec::new());
const MAX_KNOWN: usize = 64;
/// Shorter values aren't secrets worth the false matches.
const MIN_SECRET_LEN: usize = 8;

/// Mask `secret` in every line from now on: the Modal key, the client
/// secret, YouTube tokens, upload session URIs.
pub fn register_secret(secret: &str) {
    let secret = secret.trim();
    if secret.len() < MIN_SECRET_LEN {
        return;
    }
    let mut known = KNOWN.lock().unwrap_or_else(|p| p.into_inner());
    known.retain(|k| k != secret);
    known.push(secret.to_string());
    if known.len() > MAX_KNOWN {
        let excess = known.len() - MAX_KNOWN;
        known.drain(..excess);
    }
}

/// The secrets in the settings: the Modal key and the YouTube tokens.
pub fn register_settings(settings: &crate::state::Settings) {
    if let Some(key) = &settings.modal_api_key {
        register_secret(key);
    }
    if let Some(auth) = &settings.youtube_auth {
        for token in [&auth.access_token, &auth.refresh_token].into_iter().flatten() {
            register_secret(token);
        }
    }
}

/// Shapes of secrets, for those the app doesn't know (an old token in an
/// error, an auth code in a URL).
static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r#""(access_token|refresh_token|id_token|client_secret|code)"\s*:\s*"[^"]*""#, r#""$1":"<redacted>""#),
        (r#"(?i)\bBearer\s+[^\s"',;]+"#, "Bearer <redacted>"),
        (r"ya29\.[0-9A-Za-z_\-.]+", REDACTED),
        (r"\b1//[0-9A-Za-z_\-]{8,}", REDACTED),
        (r"\b4/0[0-9A-Za-z_\-]{8,}", REDACTED),
        (r"GOCSPX-[0-9A-Za-z_\-]+", REDACTED),
        (r#"\b(upload_id|code)=[^&\s"'<>]+"#, "${1}=<redacted>"),
        (r#"\b((?:[A-Za-z]+_)?(?:key|token))=[^&\s"'<>]+"#, "${1}=<redacted>"),
    ]
    .into_iter()
    .map(|(pattern, with)| (Regex::new(pattern).expect("valid pattern"), with))
    .collect()
});

/// `line` with every known secret and anything shaped like one replaced.
pub fn redact(line: &str) -> Cow<'_, str> {
    let known = KNOWN.lock().unwrap_or_else(|p| p.into_inner()).clone();
    redact_with(&known, line)
}

/// `redact` with the known secrets given.
pub fn redact_with<'a>(known: &[String], line: &'a str) -> Cow<'a, str> {
    let mut out = Cow::Borrowed(line);
    for secret in known {
        if out.contains(secret.as_str()) {
            out = Cow::Owned(out.replace(secret.as_str(), REDACTED));
        }
    }
    for (pattern, with) in PATTERNS.iter() {
        if let Cow::Owned(replaced) = pattern.replace_all(&out, *with) {
            out = Cow::Owned(replaced);
        }
    }
    out
}

/// URLs in `text` cut to their scheme and host: yt-dlp prints the video's
/// URL, signed media URLs and the like.
pub fn hosts_only(text: &str) -> Cow<'_, str> {
    static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(https?://[^/\s?#'\x22()<>]+)[^\s'\x22()<>]*").expect("valid pattern"));
    URL.replace_all(text, "$1/…")
}

/// The host of `url`, for logging which site a download came from.
pub fn url_host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "(not a URL)".into())
}

/// The last `n` lines of a tool's stderr, URLs cut to their host, ready to
/// log (the redactor still runs over it).
pub fn output_tail(stderr: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(n)..].join("\n");
    hosts_only(&tail).into_owned()
}

// ---------------------------------------------------------------------------
// Failures that repeat

/// How often a failure that goes on is mentioned again.
const STILL_FAILING_EVERY: Duration = Duration::from_secs(5 * 60);

/// Something tried again and again (the lock heartbeat, Modal polls,
/// uploads): logs when it starts failing, "still failing" at most every
/// five minutes, and when it works again, instead of every failure.
pub struct FailureTracker {
    target: &'static str,
    what: &'static str,
    failing: Option<Failing>,
}

struct Failing {
    since: chrono::DateTime<chrono::Local>,
    count: u32,
    reported: Instant,
}

impl FailureTracker {
    /// `target` is the caller's `module_path!()`; `what` names the thing
    /// that fails ("The lock heartbeat").
    pub const fn new(target: &'static str, what: &'static str) -> Self {
        FailureTracker { target, what, failing: None }
    }

    /// Note a failure; the line to log, if it's time for one.
    pub fn failure_at(&mut self, now: Instant, error: &str) -> Option<String> {
        match &mut self.failing {
            None => {
                self.failing = Some(Failing { since: chrono::Local::now(), count: 1, reported: now });
                Some(format!("{} failed: {}", self.what, error))
            }
            Some(f) => {
                f.count += 1;
                if now.duration_since(f.reported) < STILL_FAILING_EVERY {
                    return None;
                }
                f.reported = now;
                Some(format!(
                    "{} still failing ({} times, since {}): {}",
                    self.what,
                    f.count,
                    f.since.format("%H:%M:%S"),
                    error
                ))
            }
        }
    }

    /// Note a success; the line to log if it had been failing.
    pub fn recovered(&mut self) -> Option<String> {
        let f = self.failing.take()?;
        Some(format!(
            "{} works again, after {} failure{} since {}",
            self.what,
            f.count,
            if f.count == 1 { "" } else { "s" },
            f.since.format("%H:%M:%S")
        ))
    }

    pub fn log_failure(&mut self, error: impl std::fmt::Display) {
        if let Some(line) = self.failure_at(Instant::now(), &error.to_string()) {
            log::warn!(target: self.target, "{}", line);
        }
    }

    pub fn log_recovered(&mut self) {
        if let Some(line) = self.recovered() {
            log::info!(target: self.target, "{}", line);
        }
    }
}

// ---------------------------------------------------------------------------
// Reading the log back (Settings → Copy diagnostics)

/// The log folder.
pub fn log_dir<R: Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    app.path().app_log_dir().map_err(|e| format!("Can't find the log folder: {}", e))
}

/// Rotated files of the log in `dir`, newest first.
fn archives(dir: &Path) -> Vec<PathBuf> {
    let prefix = format!("{}_", FILE_NAME);
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".log"))
        })
        .collect();
    // The names end in the date and time, so they sort by age.
    found.sort();
    found.reverse();
    found
}

/// The last `n` lines of a file, read from its end (the file can be 5 MB).
fn last_lines(path: &Path, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 64 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(len) = file.metadata().map(|m| m.len()) else { return Vec::new() };
    let mut start = len;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let from = start.saturating_sub(CHUNK);
        let mut chunk = vec![0; (start - from) as usize];
        if file.seek(SeekFrom::Start(from)).is_err() || file.read_exact(&mut chunk).is_err() {
            return Vec::new();
        }
        chunk.extend_from_slice(&buf);
        buf = chunk;
        start = from;
        // One more than asked for: the first may be cut off.
        if start == 0 || buf.iter().filter(|&&b| b == b'\n').count() > n {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(n);
    lines.split_off(skip)
}

/// The last `n` lines of the log in `dir`: from the active file, topped up
/// from the newest rotated one when the active file is short.
pub fn tail(dir: &Path, n: usize) -> Vec<String> {
    let mut lines = last_lines(&dir.join(format!("{}.log", FILE_NAME)), n);
    if lines.len() < n {
        if let Some(newest) = archives(dir).first() {
            let mut older = last_lines(newest, n - lines.len());
            older.append(&mut lines);
            lines = older;
        }
    }
    lines
}

/// A logger for tests that keeps every line, as the app would write it
/// and as it was logged, with the app's levels.
#[cfg(test)]
pub mod capture {
    use super::*;

    /// (the line as written, the message as logged)
    static LINES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    struct Capture;

    impl log::Log for Capture {
        fn enabled(&self, meta: &log::Metadata) -> bool {
            let noisy = NOISY.iter().any(|n| meta.target() == *n || meta.target().starts_with(&format!("{}::", n)));
            meta.level() <= if noisy { log::Level::Warn } else { log::Level::Debug }
        }
        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                let raw = record.args().to_string();
                let line = format_line(&chrono::Local::now(), record.level(), record.target(), &raw);
                LINES.lock().unwrap_or_else(|p| p.into_inner()).push((line, raw));
            }
        }
        fn flush(&self) {}
    }

    /// Install it (once per test run; other tests' lines are kept too).
    pub fn install() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            log::set_boxed_logger(Box::new(Capture)).expect("no other logger in tests");
            log::set_max_level(log::LevelFilter::Trace);
        });
    }

    pub fn lines() -> Vec<(String, String)> {
        LINES.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCESS: &str = "ya29.a0AfB_byC-fake-access-token-1234567890";
    const REFRESH: &str = "1//0gFakeRefreshTokenAbCdEf-123456";
    const CODE: &str = "4/0AeanS0FakeAuthCode_xyz123456";
    const SECRET: &str = "GOCSPX-FakeClientSecret_abc123";

    fn redacted(line: &str) -> String {
        redact_with(&[], line).into_owned()
    }

    #[test]
    fn token_shapes_are_masked() {
        for (line, secret) in [
            (format!("token {}", ACCESS), ACCESS),
            (format!("refresh {} failed", REFRESH), REFRESH),
            (format!("GET /callback?code={}&scope=x", CODE), CODE),
            (format!("bare code {}", CODE), CODE),
            (format!("secret {}", SECRET), SECRET),
            ("Authorization: Bearer abc.def-ghi_jkl".to_string(), "abc.def-ghi_jkl"),
            (r#"{"access_token": "opaque-value-1", "expires_in": 3599}"#.to_string(), "opaque-value-1"),
            (r#"{"refresh_token":"opaque-value-2"}"#.to_string(), "opaque-value-2"),
            (r#"{"id_token":"opaque-value-3"}"#.to_string(), "opaque-value-3"),
            (r#"{"client_secret":"opaque-value-4"}"#.to_string(), "opaque-value-4"),
            (r#"{"code":"opaque-value-5"}"#.to_string(), "opaque-value-5"),
            ("PUT https://www.googleapis.com/upload/youtube/v3/videos?uploadType=resumable&upload_id=ABCdef123".to_string(), "ABCdef123"),
            ("https://example.com/x?key=AIzaFakeKey&part=snippet".to_string(), "AIzaFakeKey"),
            ("https://example.com/x?access_token=opaque6".to_string(), "opaque6"),
            ("https://example.com/x?token=opaque7".to_string(), "opaque7"),
        ] {
            let out = redacted(&line);
            assert!(!out.contains(secret), "{} -> {}", line, out);
            assert!(out.contains(REDACTED), "{} -> {}", line, out);
        }
    }

    #[test]
    fn known_secrets_are_masked_wherever_they_are() {
        let key = "modal-key-0123456789";
        let out = redact_with(&[key.to_string()], &format!("POST failed for key{}!", key)).into_owned();
        assert!(!out.contains(key), "{}", out);
        // Too short to register: left alone.
        register_secret("short");
        assert!(redact("a short line").contains("short"));
        register_secret("registered-secret-value");
        assert!(!redact("x registered-secret-value y").contains("registered-secret-value"));
    }

    #[test]
    fn ordinary_lines_are_left_alone() {
        for line in [
            "Transcribing took 4:12; 180 MB of audio",
            "Opened /Users/pastor/Movies/sermon-cut (writable)",
            "Upload 40% (1048576 of 2621440 bytes)",
            "Smart cut at 00:31:04.120 (keyframe 00:31:02.000)",
            "Library scan: 12 videos (3 new, 1 missing)",
            "2024/01/07 service.mp4",
            "HTTP 503 (120 bytes) from the server",
        ] {
            assert_eq!(redacted(line), line);
        }
    }

    #[test]
    fn lines_say_their_offset_and_module_and_indent_more_lines() {
        let now = chrono::Local::now();
        let line = format_line(&now, log::Level::Warn, "sermon_cut::library::jobs", "first\nsecond\n");
        assert!(line.contains(" WARN  library::jobs: first\n    second"), "{}", line);
        assert!(!line.ends_with('\n'));
        let stamp = now.format("%:z").to_string();
        assert!(line.starts_with(&now.format("%Y-%m-%d").to_string()) && line[..30].contains(&stamp), "{}", line);
        assert!(format_line(&now, log::Level::Info, "sermon_cut", "x").contains(" app: x"));
        assert!(!format_line(&now, log::Level::Info, "sermon_cut", ACCESS).contains(ACCESS));
    }

    #[test]
    fn output_tails_keep_the_end_and_only_hosts() {
        let stderr = (1..=30)
            .map(|i| format!("line {} https://rr3.googlevideo.com/videoplayback?expire=1&sig=abc", i))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = output_tail(stderr.as_bytes(), 20);
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines.len(), 20);
        assert!(lines[0].starts_with("line 11 "));
        assert!(lines[19].contains("https://rr3.googlevideo.com/…"));
        assert!(!tail.contains("sig=abc"));
        assert_eq!(url_host("https://www.youtube.com/watch?v=abc"), "www.youtube.com");
    }

    #[test]
    fn repeated_failures_are_logged_when_they_change() {
        let mut t = FailureTracker::new("test", "The heartbeat");
        assert_eq!(t.recovered(), None);
        let start = Instant::now();
        let first = t.failure_at(start, "disk gone").unwrap();
        assert_eq!(first, "The heartbeat failed: disk gone");
        assert_eq!(t.failure_at(start + Duration::from_secs(10), "disk gone"), None);
        assert_eq!(t.failure_at(start + Duration::from_secs(299), "disk gone"), None);
        let still = t.failure_at(start + Duration::from_secs(301), "disk gone").unwrap();
        assert!(still.starts_with("The heartbeat still failing (4 times, since "), "{}", still);
        // At most one per interval, counted from the last one.
        assert_eq!(t.failure_at(start + Duration::from_secs(400), "disk gone"), None);
        assert!(t.failure_at(start + Duration::from_secs(602), "disk gone").is_some());
        let back = t.recovered().unwrap();
        assert!(back.starts_with("The heartbeat works again, after 6 failures since "), "{}", back);
        assert_eq!(t.recovered(), None);
        assert!(t.failure_at(start + Duration::from_secs(603), "again").unwrap().ends_with("failed: again"));
    }

    #[test]
    fn the_tail_reads_from_the_end_and_tops_up_from_the_newest_archive() {
        let dir = tempfile::tempdir().unwrap();
        let older: String = (0..300).map(|i| format!("old {}\n", i)).collect();
        std::fs::write(dir.path().join("sermon-cut_2026-01-01_00-00-00.log"), "oldest\n").unwrap();
        std::fs::write(dir.path().join("sermon-cut_2026-02-01_00-00-00.log"), older).unwrap();
        std::fs::write(dir.path().join("sermon-cut.log"), "new 1\nnew 2\n").unwrap();
        let lines = tail(dir.path(), 5);
        assert_eq!(lines, ["old 297", "old 298", "old 299", "new 1", "new 2"]);

        // A long active file: only its end, across read chunks.
        let long: String = (0..20000).map(|i| format!("line {:05} {}\n", i, "x".repeat(20))).collect();
        std::fs::write(dir.path().join("sermon-cut.log"), long).unwrap();
        let lines = tail(dir.path(), 200);
        assert_eq!(lines.len(), 200);
        assert!(lines[0].starts_with("line 19800 "));
        assert!(lines[199].starts_with("line 19999 "));
    }

    /// The plugin's own file writer (through the logger it builds, never
    /// installed globally): rotation keeps the newest files, a rotation in
    /// the same second as the last keeps both, and every line is on disk
    /// as soon as it's logged.
    #[test]
    fn the_log_file_rotates_and_keeps_what_it_should() {
        use tauri_plugin_log::{Target, TargetKind};
        let dir = tempfile::tempdir().unwrap();
        let app = tauri::test::mock_app();
        let file = Target::new(TargetKind::Folder { path: dir.path().to_path_buf(), file_name: Some(FILE_NAME.into()) });
        let (_plugin, _level, logger) = builder(file, 10_000, 2)
            .level(log::LevelFilter::Trace)
            .split(app.handle())
            .unwrap();
        // About 6 KB: every line after the first fills the file.
        let filler = "x".repeat(6000);
        let write = |n: usize| {
            logger.log(
                &log::Record::builder()
                    .args(format_args!("line {} {}", n, filler))
                    .level(log::Level::Info)
                    .target("sermon_cut::logging")
                    .build(),
            );
        };
        let names = || {
            let mut names: Vec<String> = std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            names
        };
        let pause = || std::thread::sleep(Duration::from_millis(1100));

        write(0);
        assert_eq!(names(), ["sermon-cut.log"]);
        pause();
        write(1);
        assert_eq!(names().len(), 2, "{:?}", names());
        pause();
        write(2);
        assert_eq!(names().len(), 3, "{:?}", names());
        pause();
        // A third rotation: the oldest archive goes, two are kept.
        write(3);
        let now = names();
        assert_eq!(now.len(), 3, "{:?}", now);
        assert_eq!(now.iter().filter(|n| n.starts_with("sermon-cut_") && n.ends_with(".log")).count(), 2);
        assert_eq!(archives(dir.path()).len(), 2);

        // Two rotations within one second: the earlier file is kept as .bak.
        let mut n = 4;
        while !names().iter().any(|f| f.ends_with(".log.bak")) {
            assert!(n < 10, "no .bak after same-second rotations: {:?}", names());
            write(n);
            n += 1;
        }
        // The last line is on disk at once.
        write(99);
        let last = tail(dir.path(), 1);
        assert_eq!(last.len(), 1);
        assert!(last[0].contains(" INFO  logging: line 99 x"), "{:?}", &last[0][..60]);
    }
}
