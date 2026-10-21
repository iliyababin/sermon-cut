export interface VideoInfo {
  id: string;
  title: string;
  url: string;
  thumbnail: string | null;
  duration: number | null;
  channel: string | null;
  upload_date: string | null;
  file_path: string | null;
  audio_path: string | null;
  thumbnail_path: string | null;
  transcription: TranscriptionResult | null;
  status: VideoStatus;
  download_progress: number;
  created_at: string;
  // Processing results
  trimmed_path: string | null;
  thumbnail_options: string[] | null;
  processing_stage: ProcessingStage | null;
  processing_progress: number | null;
}

export type VideoStatus =
  | "pending"
  | "downloading"
  | "downloaded"
  | "extracting_audio"
  | "audio_ready"
  | "transcribing"
  | "transcribed"
  | "processing"
  | "ready"
  | "ready_for_review"
  | "error";

export type LibraryStatus =
  | "copying"
  | "unreadable"
  | "new"
  | "too_long"
  | "queued"
  | "processing"
  | "ready_for_review"
  | "done"
  | "error"
  | "missing";

export interface UploadRecord {
  url: string;
  uploaded_at: string;
  file_hash: string;
}

/** A video in the library folder. */
export interface LibraryVideo {
  id: string;
  file_name: string;
  file_path: string;
  title: string;
  status: LibraryStatus;
  error: string | null;
  duration: number | null;
  size: number;
  channel: string | null;
  source_url: string | null;
  hidden: boolean;
  uploads: UploadRecord[];
  added_at: string;
  thumbnail: string | null;
  processing_stage: ProcessingStage | null;
  processing_progress: number | null;
  processing_message: string | null;
  busy: boolean;
}

export interface DownloadItem {
  job_id: string;
  youtube_id: string;
  title: string;
}

export interface LibraryView {
  folder: string;
  available: boolean;
  writable: boolean;
  message: string | null;
  locked_by: string | null;
  tool_problem: string | null;
  videos: LibraryVideo[];
  downloads: DownloadItem[];
  notices: string[];
  max_transcribe_secs: number;
}

export interface Review {
  title: string | null;
  description: string | null;
  preacher: string | null;
}

/** Everything the review screen needs. */
export interface VideoDetail extends LibraryVideo {
  transcription: TranscriptionResult | null;
  trimmed_path: string | null;
  trim_start: number | null;
  trim_end: number | null;
  thumbnail_options: string[];
  selected_thumbnail: string | null;
  review: Review;
}

export type ProcessingStage =
  | "extracting_audio"
  | "transcribing"
  | "trimming"
  | "generating_thumbnails"
  | "complete"
  | "error";

export interface TranscriptSegment {
  start: number;
  end: number;
  text: string;
}

export interface Chapter {
  time: number;
  title: string;
}

export interface TranscriptionResult {
  segments: TranscriptSegment[];
  full_text: string;
  sermon_start: number | null;
  sermon_end: number | null;
  suggested_title: string | null;
  suggested_description: string | null;
  suggested_chapters: Chapter[] | null;
}

export interface YouTubeAuth {
  access_token: string | null;
  refresh_token: string | null;
  expires_at: number | null;
  channel_name: string | null;
  channel_id: string | null;
}

export interface YouTubeAuthStatus {
  is_authenticated: boolean;
  channel_name: string | null;
  channel_id: string | null;
}

export interface Settings {
  download_quality: string;
  output_folder: string;
  modal_api_url: string;
  modal_api_key: string | null;
  logo_path: string | null;
  youtube_auth: YouTubeAuth | null;
  youtube_playlist_id: string | null;
}

export interface PlaylistInfo {
  id: string;
  title: string;
}

export interface CropRect {
  x: number;
  y: number;
  width: number;
  height: number;
}