use std::path::PathBuf;
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
/// and falling back to whatever is on PATH.
pub fn command(name: &str) -> Command {
    match installed(name) {
        Some(path) => Command::new(path),
        None => Command::new(name),
    }
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
            println!("[tools] {}", String::from_utf8_lossy(&output.stdout).trim());
            let _ = std::fs::write(&stamp, "");
        }
        Ok(output) => eprintln!(
            "[tools] yt-dlp update failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(e) => eprintln!("[tools] yt-dlp update failed: {}", e),
    }
}
