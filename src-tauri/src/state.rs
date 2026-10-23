use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Video metadata from yt-dlp. Also the shape of videos in the pre-0.4
/// state.json, which the library migration reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub id: String,
    pub title: String,
    pub url: String,
    pub thumbnail: Option<String>,
    pub duration: Option<f64>,
    pub channel: Option<String>,
    pub upload_date: Option<String>,
    pub file_path: Option<String>,
    pub audio_path: Option<String>,
    pub thumbnail_path: Option<String>,
    pub transcription: Option<TranscriptionResult>,
    pub status: VideoStatus,
    pub download_progress: f32,
    pub created_at: String,
    // Processing results
    #[serde(default)]
    pub trimmed_path: Option<String>,
    #[serde(default)]
    pub thumbnail_options: Option<Vec<String>>,
    #[serde(default)]
    pub processing_stage: Option<ProcessingStage>,
    #[serde(default)]
    pub processing_progress: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VideoStatus {
    Pending,
    Downloading,
    Downloaded,
    ExtractingAudio,
    AudioReady,
    Transcribing,
    Transcribed,
    Processing,
    Ready,
    ReadyForReview,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStage {
    ExtractingAudio,
    Transcribing,
    Trimming,
    GeneratingThumbnails,
    Complete,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub segments: Vec<TranscriptSegment>,
    pub full_text: String,
    pub sermon_start: Option<f64>,
    pub sermon_end: Option<f64>,
    pub suggested_title: Option<String>,
    pub suggested_description: Option<String>,
    pub suggested_chapters: Option<Vec<Chapter>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub time: f64,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct YouTubeAuth {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    pub channel_name: Option<String>,
    pub channel_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub download_quality: String,
    /// The library folder (named before the library existed).
    pub output_folder: String,
    pub modal_api_url: String,
    pub modal_api_key: Option<String>,
    #[serde(default)]
    pub logo_path: Option<String>,
    #[serde(default)]
    pub youtube_auth: Option<YouTubeAuth>,
    #[serde(default)]
    pub youtube_playlist_id: Option<String>,
    /// Privacy of the last upload, offered again next time. Server-owned:
    /// only an upload changes it.
    #[serde(default = "default_upload_privacy")]
    pub upload_privacy: String,
}

fn default_upload_privacy() -> String {
    "public".to_string()
}

/// What the webview sees: no tokens, just whether (and as whom) YouTube is
/// signed in.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub download_quality: String,
    pub output_folder: String,
    pub modal_api_url: String,
    pub modal_api_key: Option<String>,
    pub logo_path: Option<String>,
    pub youtube_playlist_id: Option<String>,
    pub upload_privacy: String,
    pub youtube_signed_in: bool,
    pub youtube_channel_name: Option<String>,
}

/// What the webview may change: only the user-editable fields, each one
/// optional so a save touches just what was edited. Sign-in, privacy and
/// anything else the app manages itself can't be sent at all. For the
/// optional settings, an empty string clears them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default)]
    pub download_quality: Option<String>,
    #[serde(default)]
    pub output_folder: Option<String>,
    #[serde(default)]
    pub modal_api_url: Option<String>,
    #[serde(default)]
    pub modal_api_key: Option<String>,
    #[serde(default)]
    pub logo_path: Option<String>,
    #[serde(default)]
    pub youtube_playlist_id: Option<String>,
}

/// The transcription server (backend/app.py, deployed on Modal).
pub const DEFAULT_MODAL_API_URL: &str = "https://iliyababin--youtube-clipper-transcribe-audio.us-east.modal.run";

/// The original server, which no longer works (its AI model was retired).
/// Installs that saved it are moved to the default.
const RETIRED_MODAL_API_URL: &str = "https://sccyouthcenter--youtube-clipper-transcribe-audio.modal.run";

/// ~/Movies/sermon-cut on macOS, ~/Videos/sermon-cut elsewhere.
pub fn default_library_dir() -> String {
    dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
        .map(|p| p.join("sermon-cut").to_string_lossy().to_string())
        .unwrap_or_default()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_quality: "best".to_string(),
            output_folder: default_library_dir(),
            modal_api_url: DEFAULT_MODAL_API_URL.to_string(),
            modal_api_key: None,
            logo_path: None,
            youtube_auth: None,
            youtube_playlist_id: None,
            upload_privacy: default_upload_privacy(),
        }
    }
}

impl Settings {
    pub fn view(&self) -> SettingsView {
        let auth = self.youtube_auth.as_ref();
        SettingsView {
            download_quality: self.download_quality.clone(),
            output_folder: self.output_folder.clone(),
            modal_api_url: self.modal_api_url.clone(),
            modal_api_key: self.modal_api_key.clone(),
            logo_path: self.logo_path.clone(),
            youtube_playlist_id: self.youtube_playlist_id.clone(),
            upload_privacy: self.upload_privacy.clone(),
            youtube_signed_in: auth.is_some_and(|a| a.access_token.is_some() || a.refresh_token.is_some()),
            youtube_channel_name: auth.and_then(|a| a.channel_name.clone()),
        }
    }

    /// Apply the fields present in `patch`, leaving everything else alone.
    /// The folder is applied separately (it needs the library switched).
    pub fn apply(&mut self, patch: &SettingsPatch) {
        fn optional(v: &str) -> Option<String> {
            let v = v.trim();
            (!v.is_empty()).then(|| v.to_string())
        }
        if let Some(v) = &patch.download_quality {
            self.download_quality = v.clone();
        }
        if let Some(v) = &patch.modal_api_url {
            self.modal_api_url = v.trim().to_string();
        }
        if let Some(v) = &patch.modal_api_key {
            self.modal_api_key = optional(v);
        }
        if let Some(v) = &patch.logo_path {
            self.logo_path = optional(v);
        }
        if let Some(v) = &patch.youtube_playlist_id {
            self.youtube_playlist_id = optional(v);
        }
    }

    pub fn library_folder(&self) -> String {
        if self.output_folder.trim().is_empty() {
            default_library_dir()
        } else {
            self.output_folder.clone()
        }
    }
}

#[derive(Debug, Default)]
pub struct AppState {
    pub settings: Settings,
    /// Videos from the pre-0.4 state.json, kept verbatim (and written back
    /// on every save) until the library migration has fully succeeded.
    pub legacy_videos: serde_json::Map<String, serde_json::Value>,
    /// One-time messages for the library screen.
    pub notices: Vec<String>,
    /// The newest settings save applied this run; older ones are ignored so
    /// saves that arrive out of order can't undo a later edit.
    pub settings_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    settings: Option<Settings>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    videos: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    notices: Vec<String>,
}

pub fn state_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sermon-cut")
}

#[cfg(not(test))]
fn get_state_file_path() -> PathBuf {
    state_dir().join("state.json")
}

/// Tests must never touch the real state.json.
#[cfg(test)]
fn get_state_file_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sermon-cut-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir.join("state.json")
}

impl AppState {
    pub fn load() -> Self {
        let path = get_state_file_path();

        match std::fs::read(&path) {
            Ok(contents) => match serde_json::from_slice::<PersistedState>(&contents) {
                Ok(persisted) => {
                    let mut settings = persisted.settings.unwrap_or_default();
                    if settings.modal_api_url.trim_end_matches('/') == RETIRED_MODAL_API_URL {
                        settings.modal_api_url = DEFAULT_MODAL_API_URL.to_string();
                    }
                    return AppState {
                        settings,
                        legacy_videos: persisted.videos,
                        notices: persisted.notices,
                        settings_seq: 0,
                    };
                }
                Err(e) => {
                    // Keep the unreadable file for recovery instead of
                    // overwriting it with defaults on the next save.
                    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                    let aside = path.with_file_name(format!("state.json.corrupt-{}", stamp));
                    eprintln!("Failed to parse state file ({}); moved to {}", e, aside.display());
                    let _ = std::fs::rename(&path, &aside);
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("Failed to read state file: {}", e),
        }

        AppState::default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = get_state_file_path();

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create state directory: {}", e))?;
        }

        // Clone settings and strip access_token before saving to disk
        let mut settings_to_save = self.settings.clone();
        if let Some(ref mut auth) = settings_to_save.youtube_auth {
            auth.access_token = None;
        }

        let persisted = PersistedState {
            settings: Some(settings_to_save),
            videos: self.legacy_videos.clone(),
            notices: self.notices.clone(),
        };

        crate::library::fsutil::write_json_atomic(&path, &persisted)
    }

    /// Before dropping the old videos from state.json, keep a copy of the
    /// original file (once: a later run must not overwrite the real original).
    pub fn backup_pre_library_state() -> Result<(), String> {
        let path = get_state_file_path();
        let backup = path.with_file_name("state.v0.json");
        if backup.exists() || !path.exists() {
            return Ok(());
        }
        std::fs::copy(&path, &backup)
            .map(|_| ())
            .map_err(|e| format!("Failed to back up state.json: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_in() -> Settings {
        Settings {
            youtube_auth: Some(YouTubeAuth {
                access_token: Some("ya29.secret".into()),
                refresh_token: Some("1//refresh".into()),
                expires_at: Some(1),
                channel_name: Some("Youth".into()),
                channel_id: Some("UC1".into()),
            }),
            upload_privacy: "unlisted".into(),
            ..Settings::default()
        }
    }

    #[test]
    fn patch_never_touches_server_owned_fields() {
        let mut s = signed_in();
        let patch: SettingsPatch = serde_json::from_value(serde_json::json!({
            "download_quality": "720p",
            "modal_api_key": "",
            "logo_path": "/x/logo.png",
        }))
        .unwrap();
        s.apply(&patch);
        assert_eq!(s.download_quality, "720p");
        assert_eq!(s.modal_api_key, None);
        assert_eq!(s.logo_path.as_deref(), Some("/x/logo.png"));
        let auth = s.youtube_auth.as_ref().unwrap();
        assert_eq!(auth.refresh_token.as_deref(), Some("1//refresh"));
        assert_eq!(s.upload_privacy, "unlisted");
    }

    #[test]
    fn patch_rejects_server_owned_fields() {
        for field in ["youtube_auth", "upload_privacy", "recent_preachers", "auto_process_since"] {
            let json = serde_json::json!({ field: null });
            assert!(serde_json::from_value::<SettingsPatch>(json).is_err(), "{field}");
        }
    }

    #[test]
    fn view_has_no_tokens() {
        let view = serde_json::to_string(&signed_in().view()).unwrap();
        assert!(!view.contains("token"), "{view}");
        assert!(!view.contains("secret") && !view.contains("refresh"), "{view}");
        assert!(view.contains("\"youtube_signed_in\":true"));
        assert!(view.contains("Youth"));
    }

    #[test]
    fn old_state_files_default_to_public() {
        let s: Settings = serde_json::from_value(serde_json::json!({
            "download_quality": "best",
            "output_folder": "",
            "modal_api_url": "x",
            "modal_api_key": null,
        }))
        .unwrap();
        assert_eq!(s.upload_privacy, "public");
    }
}
