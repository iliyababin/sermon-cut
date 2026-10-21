//! lock.json: notice when another Mac has the same library open (shared
//! drive, Dropbox), since two writers would clobber library.json.

use super::fsutil;
use super::LibraryPaths;
use serde::{Deserialize, Serialize};

/// A heartbeat older than this means the other app is gone.
const STALE_AFTER_SECS: i64 = 120;
pub const HEARTBEAT_SECS: u64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockInfo {
    pub host: String,
    pub pid: u32,
    pub session: String,
    pub app_version: String,
    pub heartbeat: i64,
}

pub fn host_name() -> String {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let output = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output();
        #[cfg(not(target_os = "macos"))]
        let output = std::process::Command::new("hostname").output();
        output
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "this computer".to_string())
    })
    .clone()
}

pub enum Acquire {
    Acquired,
    /// Another computer has it open; the library should be read-only.
    HeldBy(String),
}

fn read(paths: &LibraryPaths) -> Option<LockInfo> {
    fsutil::read_json(&paths.lock_file(), 64 * 1024).ok().flatten()
}

fn write(paths: &LibraryPaths, session: &str) -> Result<(), String> {
    let info = LockInfo {
        host: host_name(),
        pid: std::process::id(),
        session: session.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        heartbeat: chrono::Utc::now().timestamp(),
    };
    fsutil::write_json_atomic(&paths.lock_file(), &info)
}

/// Take the lock unless another computer holds a fresh one. A lock from this
/// computer is always ours to take: single-instance means no other copy runs.
pub fn acquire(paths: &LibraryPaths, session: &str, force: bool) -> Result<Acquire, String> {
    if !force {
        if let Some(existing) = read(paths) {
            let fresh = chrono::Utc::now().timestamp() - existing.heartbeat < STALE_AFTER_SECS;
            if fresh && existing.session != session && existing.host != host_name() {
                return Ok(Acquire::HeldBy(existing.host));
            }
        }
    }
    write(paths, session)?;
    Ok(Acquire::Acquired)
}

/// Refresh our heartbeat. Returns the other host's name if someone took over.
pub fn heartbeat(paths: &LibraryPaths, session: &str) -> Result<Option<String>, String> {
    if let Some(existing) = read(paths) {
        if existing.session != session {
            return Ok(Some(existing.host));
        }
    }
    write(paths, session)?;
    Ok(None)
}

pub fn release(paths: &LibraryPaths, session: &str) {
    if read(paths).is_some_and(|l| l.session == session) {
        let _ = std::fs::remove_file(paths.lock_file());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> (tempfile::TempDir, LibraryPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(tmp.path().to_path_buf());
        paths.ensure_dir(&paths.data_dir()).unwrap();
        (tmp, paths)
    }

    fn plant(paths: &LibraryPaths, host: &str, age: i64) {
        let info = LockInfo {
            host: host.into(),
            pid: 1,
            session: "other".into(),
            app_version: "0".into(),
            heartbeat: chrono::Utc::now().timestamp() - age,
        };
        fsutil::write_json_atomic(&paths.lock_file(), &info).unwrap();
    }

    #[test]
    fn fresh_lock_from_another_host_blocks() {
        let (_t, paths) = lib();
        plant(&paths, "Office iMac", 5);
        assert!(matches!(acquire(&paths, "me", false).unwrap(), Acquire::HeldBy(h) if h == "Office iMac"));
        // Take over.
        assert!(matches!(acquire(&paths, "me", true).unwrap(), Acquire::Acquired));
        assert_eq!(heartbeat(&paths, "me").unwrap(), None);
    }

    #[test]
    fn stale_or_local_locks_are_taken() {
        let (_t, paths) = lib();
        plant(&paths, "Office iMac", 600);
        assert!(matches!(acquire(&paths, "me", false).unwrap(), Acquire::Acquired));
        plant(&paths, &host_name(), 5);
        assert!(matches!(acquire(&paths, "me", false).unwrap(), Acquire::Acquired));
    }

    #[test]
    fn heartbeat_notices_takeover_and_release_is_ours_only() {
        let (_t, paths) = lib();
        acquire(&paths, "me", false).unwrap();
        plant(&paths, "Office iMac", 0);
        assert_eq!(heartbeat(&paths, "me").unwrap().as_deref(), Some("Office iMac"));
        release(&paths, "me");
        assert!(paths.lock_file().exists());
    }
}
