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
        }
    }
}

impl Settings {
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

fn get_state_file_path() -> PathBuf {
    state_dir().join("state.json")
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
