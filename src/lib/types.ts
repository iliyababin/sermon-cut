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
  | "uploading"
  | "ready_for_review"
  | "done"
  | "error"
  | "missing";

export interface UploadRecord {
  url: string;
  uploaded_at: string;
  file_hash: string;
  privacy?: UploadPrivacy | null;
  playlist_id?: string | null;
  /** YouTube's id for the video (uploads since 0.5). */
  video_id?: string | null;
  /** What didn't work after the upload. */
  warnings?: UploadWarning[];
}

export interface UploadWarning {
  kind: "thumbnail" | "captions" | "playlist";
  message: string;
}

/** What youtube_upload_video resolves with. */
export interface UploadOutcome {
  url: string;
  video_id: string;
  warnings: UploadWarning[];
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
  /** Percent of a YouTube upload in progress. */
  upload_progress: number | null;
  busy: boolean;
  /** For a queued video, its place in line, counting the one being
   * processed as the first ("2 of 3"). */
  queue_position: number | null;
  queue_length: number | null;
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
  thumbnail_options: ThumbnailOption[];
  /** Stored template renders, at most one per source. */
  renders: ThumbnailRender[];
  /** The selected source's id. */
  selected_source: string | null;
  selected_thumbnail: string | null;
  review: Review;
  /** YouTube URL of another library entry with the same file that was
   * already uploaded. */
  duplicate_upload: string | null;
}

/** A thumbnail source: a generated crop or a custom one. */
export interface ThumbnailOption {
  /** Its file name; stable, so renders and the selection are keyed by it. */
  id: string;
  path: string;
  /** The image the template draws over. */
  render_from: string;
  /** Changes when the image is regenerated. */
  version: string;
}

export interface ThumbnailRender {
  source: string;
  path: string;
  render_key: string;
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

export interface YouTubeAuthStatus {
  is_authenticated: boolean;
  channel_name: string | null;
  channel_id: string | null;
}

export type UploadPrivacy = "public" | "unlisted" | "private";

/** Settings as the app shows them. Sign-in tokens never leave the backend. */
export interface Settings {
  download_quality: string;
  output_folder: string;
  modal_api_url: string;
  modal_api_key: string | null;
  logo_path: string | null;
  youtube_playlist_id: string | null;
  upload_privacy: UploadPrivacy;
  youtube_signed_in: boolean;
  youtube_channel_name: string | null;
  /** Speakers named in reviews, most recent first (the app keeps it). */
  recent_preachers: string[];
  /** Process videos that appear in the library folder from now on. */
  auto_process: boolean;
}

/** The settings a save may change. Absent fields are left alone; for the
 * optional ones an empty string clears them. */
export interface SettingsPatch {
  download_quality?: string;
  output_folder?: string;
  modal_api_url?: string;
  modal_api_key?: string;
  logo_path?: string;
  youtube_playlist_id?: string;
  auto_process?: boolean;
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