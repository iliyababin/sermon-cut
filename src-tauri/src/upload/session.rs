//! Resumable sessions that outlive the app: kept on the library entry, so
//! an upload interrupted by sleep, quit or a crash continues where it
//! stopped instead of starting over.

use serde::{Deserialize, Serialize};

/// YouTube keeps a resumable session for about a week; reuse it for less.
pub const MAX_SESSION_AGE: chrono::Duration = chrono::Duration::days(6);

const TRUSTED_PREFIX: &str = "https://www.googleapis.com/upload/";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadSession {
    pub uri: String,
    /// The entry's fingerprint hash (never recomputed for this).
    pub file_hash: String,
    /// Size of the file being sent (the trimmed one, usually).
    pub size: u64,
    /// Covers the title, description, privacy and which file is sent.
    pub metadata_hash: String,
    /// RFC 3339.
    pub created: String,
    /// Which file was sent (name, type, modification time), apart from the
    /// metadata: tells a new cut from an edited title when an earlier
    /// session turns out finished. None for sessions from before 0.6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_key: Option<String>,
}

impl UploadSession {
    pub fn new(uri: String, file_hash: &str, size: u64, metadata_hash: &str) -> Self {
        UploadSession {
            uri,
            file_hash: file_hash.to_string(),
            size,
            metadata_hash: metadata_hash.to_string(),
            created: chrono::Utc::now().to_rfc3339(),
            file_key: None,
        }
    }

    pub fn with_file_key(mut self, key: &str) -> Self {
        self.file_key = Some(key.to_string());
        self
    }

    /// Whether this session sent a different file (another source, or a
    /// new cut) than the one described. Sessions without a file key are
    /// judged by size alone.
    pub fn other_file(&self, file_hash: &str, size: u64, file_key: &str) -> bool {
        self.file_hash != file_hash || self.size != size || self.file_key.as_deref().is_some_and(|k| k != file_key)
    }

    /// A session URI from library.json (a synced, editable file) is only
    /// used if it points at YouTube's upload server.
    pub fn is_trusted(&self) -> bool {
        self.uri.starts_with(TRUSTED_PREFIX) && !self.uri.contains(['\r', '\n', ' '])
    }

    /// Whether this session can carry on the upload described.
    pub fn reusable_for(&self, file_hash: &str, size: u64, metadata_hash: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
        let fresh = chrono::DateTime::parse_from_rfc3339(&self.created)
            .map(|c| {
                let age = now.signed_duration_since(c.with_timezone(&chrono::Utc));
                age >= chrono::Duration::zero() && age < MAX_SESSION_AGE
            })
            .unwrap_or(false);
        fresh && self.file_hash == file_hash && self.size == size && self.metadata_hash == metadata_hash
    }
}

/// Hash of what the session was created with, so changed metadata or a
/// different file starts a new session.
pub fn metadata_hash(metadata_json: &str, content_type: &str, file_name: &str, mtime_ms: i64) -> String {
    let input = format!("{}\n{}\n{}\n{}", metadata_json, content_type, file_name, mtime_ms);
    format!("{:x}", md5::compute(input.as_bytes()))
}

/// Hash of which file is sent, apart from the metadata.
pub fn file_key(content_type: &str, file_name: &str, mtime_ms: i64) -> String {
    let input = format!("{}\n{}\n{}", content_type, file_name, mtime_ms);
    format!("{:x}", md5::compute(input.as_bytes()))
}

/// Where the session is kept between attempts.
pub trait SessionStore: Send + Sync {
    fn load(&self) -> Option<UploadSession>;
    fn save(&self, session: Option<&UploadSession>);
    /// YouTube has the whole video, as `video_id`, made from the file whose
    /// fingerprint is `file_hash`: drop the session and record the upload in
    /// one step, so a crash afterwards (during the thumbnail, captions or
    /// playlist) neither loses the video nor sends it again.
    fn complete(&self, video_id: &str, file_hash: &str) {
        let _ = (video_id, file_hash);
        self.save(None);
    }
}

/// A store in memory, for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore(pub std::sync::Mutex<Option<UploadSession>>);

#[cfg(test)]
impl SessionStore for MemoryStore {
    fn load(&self) -> Option<UploadSession> {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, session: Option<&UploadSession>) {
        *self.0.lock().unwrap() = session.cloned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reused_only_when_young_and_matching() {
        let s = UploadSession::new("u".into(), "h", 10, "m");
        let now = chrono::Utc::now();
        assert!(s.reusable_for("h", 10, "m", now));
        assert!(!s.reusable_for("other", 10, "m", now));
        assert!(!s.reusable_for("h", 11, "m", now));
        assert!(!s.reusable_for("h", 10, "changed", now));
        assert!(s.reusable_for("h", 10, "m", now + chrono::Duration::days(5)));
        assert!(!s.reusable_for("h", 10, "m", now + chrono::Duration::days(6)));
        let bad = UploadSession { created: "yesterday".into(), ..s };
        assert!(!bad.reusable_for("h", 10, "m", now));
    }

    #[test]
    fn tells_another_file_from_changed_metadata() {
        let s = UploadSession::new("u".into(), "h", 10, "m").with_file_key("k");
        assert!(!s.other_file("h", 10, "k"));
        assert!(s.other_file("h", 10, "k2"), "a new cut");
        assert!(s.other_file("h", 11, "k"));
        assert!(s.other_file("h2", 10, "k"), "another source");
        // An old session without a key: only the size tells.
        let old = UploadSession { file_key: None, ..s };
        assert!(!old.other_file("h", 10, "anything"));
    }

    #[test]
    fn only_youtube_session_uris_are_trusted() {
        let s = |uri: &str| UploadSession::new(uri.into(), "h", 1, "m");
        assert!(s("https://www.googleapis.com/upload/youtube/v3/videos?upload_id=x").is_trusted());
        assert!(!s("https://evil.example/upload/").is_trusted());
        assert!(!s("https://www.googleapis.com.evil.example/upload/").is_trusted());
        assert!(!s("http://www.googleapis.com/upload/x").is_trusted());
    }

    #[test]
    fn metadata_hash_changes_with_inputs() {
        let a = metadata_hash("{}", "video/mp4", "trimmed.mp4", 1);
        assert_eq!(a, metadata_hash("{}", "video/mp4", "trimmed.mp4", 1));
        assert_ne!(a, metadata_hash("{\"t\":1}", "video/mp4", "trimmed.mp4", 1));
        assert_ne!(a, metadata_hash("{}", "video/mp4", "trimmed.mp4", 2));
    }
}
