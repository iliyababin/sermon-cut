use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[cfg(target_os = "macos")]
const INSTALL_HINT: &str = "Run the installer again to fix this:\n\ncurl -fsSL https://raw.githubusercontent.com/iliyababin/sermon-cut/main/install.sh | bash";
#[cfg(not(target_os = "macos"))]
const INSTALL_HINT: &str = "See the Prerequisites section of the README to install it.";

/// Folder where install.sh puts ffmpeg, ffprobe, yt-dlp and qjs.
pub fn tools_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("com.youthscc.sermon-cut").join("bin"))
}

fn installed(name: &str) -> Option<PathBuf> {
    let path = tools_dir()?.join(format!("{}{}", name, std::env::consts::EXE_SUFFIX));
    path.is_file().then_some(path)
}

/// Build a command for an external tool, preferring the copy from install.sh
/// and falling back to whatever is on PATH. The process is killed if the
/// future running it is dropped (e.g. a cancelled job).
pub fn command(name: &str) -> Command {
    let mut command = match installed(name) {
        Some(path) => Command::new(path),
        None => Command::new(name),
    };
    command.kill_on_drop(true);
    command
}

/// Blocking version of `command`, for code already on a blocking thread.
pub fn std_command(name: &str) -> std::process::Command {
    match installed(name) {
        Some(path) => std::process::Command::new(path),
        None => std::process::Command::new(name),
    }
}

static MISSING_TOOL: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

/// Remember that a tool couldn't be started, so the UI can say so once
/// instead of showing every video as unreadable.
pub fn mark_missing(name: &'static str) {
    *MISSING_TOOL.lock().unwrap() = Some(name);
}

pub fn clear_missing(name: &str) {
    let mut missing = MISSING_TOOL.lock().unwrap();
    if *missing == Some(name) {
        *missing = None;
    }
}

pub fn missing_tool_message() -> Option<String> {
    MISSING_TOOL
        .lock()
        .unwrap()
        .map(|name| format!("{} isn't installed. {}", name, INSTALL_HINT))
}

/// Where each outside tool comes from, for the diagnostics.
pub fn report() -> Vec<String> {
    let on_path = |name: &str| {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(format!("{}{}", name, std::env::consts::EXE_SUFFIX)))
                .find(|p| p.is_file())
        })
    };
    let mut lines: Vec<String> = ["ffmpeg", "ffprobe", "yt-dlp"]
        .into_iter()
        .map(|name| match (installed(name), on_path(name)) {
            (Some(p), _) => format!("{}: {} (the app's copy)", name, p.display()),
            (None, Some(p)) => format!("{}: {}", name, p.display()),
            (None, None) => format!("{}: not found", name),
        })
        .collect();
    lines.extend(missing_tool_message().map(|m| format!("Couldn't start: {}", m.lines().next().unwrap_or_default())));
    lines
}

/// Turn a failure to start a tool into a message a non-technical user can act on.
pub fn spawn_error(name: &str, e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        format!("{} isn't installed. {}", name, INSTALL_HINT)
    } else {
        format!("Failed to run {}: {}", name, e)
    }
}

/// Apps opened from Finder or the Dock get a minimal PATH
/// (/usr/bin:/bin:/usr/sbin:/sbin), so add the tools folder and Homebrew's
/// locations. Child processes (e.g. yt-dlp looking for ffmpeg) inherit this.
pub fn extend_path() {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(dir) = tools_dir() {
        paths.push(dir);
    }
    if cfg!(target_os = "macos") {
        paths.push("/opt/homebrew/bin".into());
        paths.push("/usr/local/bin".into());
    }
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
    }
}

/// YouTube breaks yt-dlp every few weeks, so update our copy at most once a day.
pub async fn update_ytdlp_if_stale() {
    let (Some(ytdlp), Some(dir)) = (installed("yt-dlp"), tools_dir()) else {
        return;
    };
    let stamp = dir.join(".yt-dlp-last-update");
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    let fresh = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map_or(false, |age| age < day);
    if fresh {
        return;
    }

    match Command::new(&ytdlp).arg("-U").output().await {
        Ok(output) if output.status.success() => {
            log::info!("yt-dlp update: {}", crate::logging::output_tail(&output.stdout, 5));
            let _ = std::fs::write(&stamp, "");
        }
        Ok(output) => log::warn!(
            "yt-dlp update failed ({}):\n{}",
            output.status,
            crate::logging::output_tail(&output.stderr, 20)
        ),
        Err(e) => log::warn!("yt-dlp update failed: {}", e),
    }
}

/// For tests that need ffmpeg and ffprobe (found the way the app finds
/// them): false, with a note, if they're missing, so the test can skip.
/// With REQUIRE_FFMPEG=1 a missing tool fails the test instead.
#[cfg(test)]
pub fn ffmpeg_for_tests() -> bool {
    let works = |name: &str| {
        std_command(name)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let missing: Vec<&str> = ["ffmpeg", "ffprobe"].into_iter().filter(|n| !works(n)).collect();
    if missing.is_empty() {
        return true;
    }
    if std::env::var("REQUIRE_FFMPEG").is_ok_and(|v| v == "1") {
        panic!("{} not found (REQUIRE_FFMPEG=1)", missing.join(" and "));
    }
    eprintln!("skipping: {} not found", missing.join(" and "));
    false
}

/// Something to show with the system's own apps.
#[derive(Debug, Clone, Copy)]
pub enum OsOpen<'a> {
    /// A web page, in the default browser.
    Url(&'a str),
    /// A file, in its default app.
    File(&'a Path),
    /// A folder, in the file manager.
    Folder(&'a Path),
    /// A file, selected in its folder in the file manager (on Linux, its
    /// folder is opened).
    Reveal(&'a Path),
}

/// The program and arguments that show `target` on this system.
fn os_open_command(target: OsOpen<'_>) -> (&'static str, Vec<OsString>) {
    let os = |s: &str| OsString::from(s);
    if cfg!(target_os = "macos") {
        match target {
            OsOpen::Url(url) => ("open", vec![os(url)]),
            OsOpen::File(p) | OsOpen::Folder(p) => ("open", vec![p.into()]),
            OsOpen::Reveal(p) => ("open", vec![os("-R"), p.into()]),
        }
    } else if cfg!(target_os = "windows") {
        match target {
            OsOpen::Url(url) => ("rundll32", vec![os("url.dll,FileProtocolHandler"), os(url)]),
            OsOpen::File(p) => ("cmd", vec![os("/C"), os("start"), os(""), p.into()]),
            OsOpen::Folder(p) => ("explorer", vec![p.into()]),
            OsOpen::Reveal(p) => {
                let mut select = os("/select,");
                select.push(p);
                ("explorer", vec![select])
            }
        }
    } else {
        match target {
            OsOpen::Url(url) => ("xdg-open", vec![os(url)]),
            OsOpen::File(p) | OsOpen::Folder(p) => ("xdg-open", vec![p.into()]),
            OsOpen::Reveal(p) => ("xdg-open", vec![p.parent().unwrap_or(p).into()]),
        }
    }
}

/// Show `target` with the system's own apps (browser, default app, file
/// manager). Doesn't wait for them.
pub fn os_open(target: OsOpen<'_>) -> Result<(), String> {
    let (program, args) = os_open_command(target);
    std::process::Command::new(program).args(args).spawn().map(|_| ()).map_err(|e| {
        let what = match target {
            OsOpen::Url(_) => "the browser",
            OsOpen::File(_) => "the file",
            OsOpen::Folder(_) => "the folder",
            OsOpen::Reveal(_) => "the file manager",
        };
        format!("Failed to open {}: {}", what, e)
    })
}

#[cfg(test)]
mod os_open_tests {
    use super::*;

    #[test]
    fn each_target_has_its_command() {
        let p = Path::new("/lib/Sunday service.mp4");
        let args = |t| os_open_command(t).1.into_iter().map(|a| a.to_string_lossy().to_string()).collect::<Vec<_>>();
        if cfg!(target_os = "macos") {
            assert_eq!(os_open_command(OsOpen::Reveal(p)).0, "open");
            assert_eq!(args(OsOpen::Reveal(p)), ["-R", "/lib/Sunday service.mp4"]);
            assert_eq!(args(OsOpen::File(p)), ["/lib/Sunday service.mp4"]);
            assert_eq!(args(OsOpen::Url("https://youtu.be/x")), ["https://youtu.be/x"]);
        } else if cfg!(target_os = "linux") {
            assert_eq!(args(OsOpen::Reveal(p)), ["/lib"]);
        }
        // The path is one argument, never split or read as an option.
        assert_eq!(args(OsOpen::Folder(p)).last().unwrap(), "/lib/Sunday service.mp4");
    }
}
