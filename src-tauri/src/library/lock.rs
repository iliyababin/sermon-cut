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
    /// Which Mac and which account hold the lock. Absent in locks written
    /// by v0.4.0, which are compared by host name instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Who this app is, for telling our own lock from another computer's.
/// Host names aren't unique (two Macs both called "iMac"), so the
/// hardware UUID is used where there is one.
#[derive(Debug, Clone)]
pub struct Identity {
    pub host: String,
    pub machine_id: String,
    pub user: String,
}

pub fn identity() -> &'static Identity {
    static ME: std::sync::OnceLock<Identity> = std::sync::OnceLock::new();
    ME.get_or_init(|| Identity {
        host: host_name(),
        machine_id: machine_id().unwrap_or_else(host_name),
        user: std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".into()),
    })
}

/// The Mac's hardware UUID (IOPlatformUUID).
#[cfg(target_os = "macos")]
fn machine_id() -> Option<String> {
    let out = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    parse_platform_uuid(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(target_os = "macos"))]
fn machine_id() -> Option<String> {
    None
}

/// `"IOPlatformUUID" = "1234-..."` from ioreg's output.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_platform_uuid(ioreg: &str) -> Option<String> {
    ioreg
        .lines()
        .find(|l| l.contains("\"IOPlatformUUID\""))
        .and_then(|l| l.split('=').nth(1))
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

/// Whether a lock was written by this computer and account. Old locks
/// without an identity fall back to the host name.
fn is_ours(lock: &LockInfo, me: &Identity) -> bool {
    match (&lock.machine_id, &lock.user) {
        (Some(machine), Some(user)) => *machine == me.machine_id && *user == me.user,
        _ => lock.host == me.host,
    }
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

fn write(paths: &LibraryPaths, session: &str, me: &Identity) -> Result<(), String> {
    let info = LockInfo {
        host: me.host.clone(),
        pid: std::process::id(),
        session: session.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        heartbeat: chrono::Utc::now().timestamp(),
        machine_id: Some(me.machine_id.clone()),
        user: Some(me.user.clone()),
    };
    fsutil::write_json_atomic(&paths.lock_file(), &info)
}

/// Take the lock unless another computer (or another account on this one)
/// holds a fresh one. A lock from this computer and account is always ours
/// to take: single-instance means no other copy runs.
pub fn acquire(paths: &LibraryPaths, session: &str, force: bool) -> Result<Acquire, String> {
    acquire_as(paths, session, force, identity())
}

fn acquire_as(paths: &LibraryPaths, session: &str, force: bool, me: &Identity) -> Result<Acquire, String> {
    if !force {
        if let Some(existing) = read(paths) {
            let fresh = chrono::Utc::now().timestamp() - existing.heartbeat < STALE_AFTER_SECS;
            if fresh && existing.session != session && !is_ours(&existing, me) {
                return Ok(Acquire::HeldBy(existing.host));
            }
        }
    }
    write(paths, session, me)?;
    Ok(Acquire::Acquired)
}

/// Refresh our heartbeat. Returns the other host's name if someone took over.
pub fn heartbeat(paths: &LibraryPaths, session: &str) -> Result<Option<String>, String> {
    if let Some(existing) = read(paths) {
        if existing.session != session {
            return Ok(Some(existing.host));
        }
    }
    write(paths, session, identity())?;
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
            machine_id: None,
            user: None,
        };
        fsutil::write_json_atomic(&paths.lock_file(), &info).unwrap();
    }

    fn me() -> Identity {
        Identity { host: "iMac".into(), machine_id: "UUID-A".into(), user: "media".into() }
    }

    fn plant_identity(paths: &LibraryPaths, host: &str, machine: &str, user: &str) {
        let info = LockInfo {
            host: host.into(),
            pid: 1,
            session: "other".into(),
            app_version: "0".into(),
            heartbeat: chrono::Utc::now().timestamp(),
            machine_id: Some(machine.into()),
            user: Some(user.into()),
        };
        fsutil::write_json_atomic(&paths.lock_file(), &info).unwrap();
    }

    #[test]
    fn locks_are_ours_only_for_the_same_machine_and_user() {
        let (_t, paths) = lib();
        // Another Mac with the same name.
        plant_identity(&paths, "iMac", "UUID-B", "media");
        assert!(matches!(acquire_as(&paths, "me", false, &me()).unwrap(), Acquire::HeldBy(h) if h == "iMac"));
        // This Mac, another account.
        plant_identity(&paths, "iMac", "UUID-A", "someone");
        assert!(matches!(acquire_as(&paths, "me", false, &me()).unwrap(), Acquire::HeldBy(_)));
        // This Mac and account (e.g. after a crash).
        plant_identity(&paths, "Renamed iMac", "UUID-A", "media");
        assert!(matches!(acquire_as(&paths, "me", false, &me()).unwrap(), Acquire::Acquired));
        let written = read(&paths).unwrap();
        assert_eq!((written.machine_id.as_deref(), written.user.as_deref()), (Some("UUID-A"), Some("media")));
    }

    #[test]
    fn old_locks_without_identity_compare_host_names() {
        let (_t, paths) = lib();
        plant(&paths, "iMac", 5);
        assert!(matches!(acquire_as(&paths, "me", false, &me()).unwrap(), Acquire::Acquired));
        plant(&paths, "Office iMac", 5);
        assert!(matches!(acquire_as(&paths, "me", false, &me()).unwrap(), Acquire::HeldBy(_)));
    }

    #[test]
    fn parses_ioreg_uuid() {
        let out = "+-o J314sAP  <class IOPlatformExpertDevice>\n    {\n      \"IOPlatformSerialNumber\" = \"X\"\n      \"IOPlatformUUID\" = \"1A2B-3C\"\n    }\n";
        assert_eq!(parse_platform_uuid(out).as_deref(), Some("1A2B-3C"));
        assert_eq!(parse_platform_uuid("nothing"), None);
        assert!(!identity().machine_id.is_empty());
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
        plant(&paths, &identity().host, 5);
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
