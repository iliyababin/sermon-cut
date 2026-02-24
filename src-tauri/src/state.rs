use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_quality: "best".to_string(),
            output_folder: dirs::video_dir()
                .map(|p| p.join("sermon-cut").to_string_lossy().to_string())
                .unwrap_or_default(),
            modal_api_url: "https://sccyouthcenter--youtube-clipper-transcribe-audio.modal.run".to_string(),
            modal_api_key: None,
            logo_path: None,
            youtube_auth: None,
            youtube_playlist_id: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct AppState {
    pub videos: HashMap<String, VideoInfo>,
    pub active_downloads: HashMap<String, tokio::sync::watch::Sender<bool>>,
    pub settings: Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedState {
    videos: HashMap<String, VideoInfo>,
    settings: Settings,
}

fn get_state_file_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sermon-cut")
        .join("state.json")
}

impl AppState {
    pub fn load() -> Self {
        let path = get_state_file_path();

        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => {
                    match serde_json::from_str::<PersistedState>(&contents) {
                        Ok(persisted) => {
                            return AppState {
                                videos: persisted.videos,
                                active_downloads: HashMap::new(),
                                settings: persisted.settings,
                            };
                        }
                        Err(e) => {
                            eprintln!("Failed to parse state file: {}", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to read state file: {}", e);
                }
            }
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
            videos: self.videos.clone(),
            settings: settings_to_save,
        };

        let contents = serde_json::to_string_pretty(&persisted)
            .map_err(|e| format!("Failed to serialize state: {}", e))?;

        std::fs::write(&path, contents)
            .map_err(|e| format!("Failed to write state file: {}", e))?;

        Ok(())
    }
}
