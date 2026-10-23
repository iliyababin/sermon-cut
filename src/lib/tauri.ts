import { invoke } from "@tauri-apps/api/core";
import type {
  VideoInfo,
  Settings,
  SettingsPatch,
  YouTubeAuthStatus,
  CropRect,
  PlaylistInfo,
  UploadPrivacy,
  UploadOutcome,
  UploadWarning,
  LibraryView,
  VideoDetail,
} from "./types";

export async function getVideoInfo(url: string): Promise<VideoInfo> {
  return invoke("get_video_info", { url });
}

// Library

export async function getLibrary(): Promise<LibraryView> {
  return invoke("get_library");
}

/** List the library folder again and return the result. */
export async function rescanLibrary(): Promise<LibraryView> {
  return invoke("rescan_library");
}

export async function getVideo(videoId: string): Promise<VideoDetail> {
  return invoke("get_video", { videoId });
}

export async function processVideo(videoId: string): Promise<void> {
  return invoke("process_video", { videoId });
}

export async function cancelProcessing(videoId: string): Promise<void> {
  return invoke("cancel_processing", { videoId });
}

export async function resetVideo(videoId: string): Promise<void> {
  return invoke("reset_video", { videoId });
}

export async function hideVideo(videoId: string, hidden: boolean): Promise<void> {
  return invoke("hide_video", { videoId, hidden });
}

export async function trashVideo(videoId: string): Promise<void> {
  return invoke("trash_video", { videoId });
}

export async function forgetVideo(videoId: string): Promise<void> {
  return invoke("forget_video", { videoId });
}

export async function saveReview(
  videoId: string,
  review: { title: string; description: string; preacher: string; singleLine: boolean },
  selectedSource: string | null
): Promise<void> {
  return invoke("save_review", { videoId, ...review, selectedSource });
}

/** Cut the video again at new points. Resolves with where the cut really
 * starts (a keyframe may be a moment early) and ends. Progress comes as
 * `trim-progress` events. */
export async function setTrim(videoId: string, start: number, end: number): Promise<{ start: number; end: number }> {
  return invoke("set_trim", { videoId, start, end });
}

/** A still of the original video at `time`, for when it can't play here. */
export async function frameStill(videoId: string, time: number): Promise<string> {
  return invoke("frame_still", { videoId, time });
}

export async function revealVideo(videoId: string): Promise<void> {
  return invoke("reveal_video", { videoId });
}

/** Show a video's leftover ".commit-backup" folder in Finder. */
export async function revealCommitBackup(videoId: string): Promise<void> {
  return invoke("reveal_commit_backup", { videoId });
}

/** The error a save gives when an earlier one left its backup folder
 * behind and it couldn't be put back (STUCK_COMMIT in the backend). */
export function isStuckCommit(message: string | null | undefined): boolean {
  return !!message && message.includes(".commit-backup");
}

export async function openLibraryFolder(): Promise<void> {
  return invoke("open_library_folder");
}

export async function dismissNotices(): Promise<void> {
  return invoke("dismiss_notices");
}

export async function takeOverLibrary(): Promise<LibraryView> {
  return invoke("take_over_library");
}

export async function clearLibraryData(): Promise<void> {
  return invoke("clear_library_data");
}

export async function checkLibraryFolder(path: string): Promise<string[]> {
  return invoke("check_library_folder", { path });
}

/** Download into the library. Resolves with the new video's id. */
export async function downloadVideo(
  url: string,
  quality?: string,
  process?: boolean
): Promise<string> {
  return invoke("download_video", { url, quality, process });
}

/** Before installing an app update; rejects while work is running. */
export async function beginUpdate(): Promise<void> {
  return invoke("begin_update");
}

/** The update didn't install: let work start again. */
export async function endUpdate(): Promise<void> {
  return invoke("end_update");
}

export async function cancelDownload(videoId: string): Promise<void> {
  return invoke("cancel_download", { videoId });
}

// Settings

export async function getSettings(): Promise<Settings> {
  return invoke("get_settings");
}

// Increases with every save, so the backend can drop a field's value that
// arrives after a newer one. The epoch is new with every load of the
// webview, whose counter starts again; the backend starts over with it.
let lastSettingsSeq = 0;
const settingsEpoch = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;

export function nextSettingsSeq(): number {
  lastSettingsSeq += 1;
  return lastSettingsSeq;
}

/** Save only the given fields. Resolves with the settings as they now are.
 * Pass a `seq` from nextSettingsSeq() to tell responses apart. */
export async function saveSettings(patch: SettingsPatch, seq = nextSettingsSeq()): Promise<Settings> {
  return invoke("save_settings", { patch, seq, epoch: settingsEpoch });
}

export async function resetState(): Promise<void> {
  return invoke("reset_state");
}

/** Where the app keeps its settings and its tools (ffmpeg, yt-dlp). */
export async function getAppFolders(): Promise<{ settings: string; tools: string | null }> {
  return invoke("get_app_folders");
}

export async function openFile(path: string): Promise<void> {
  return invoke("open_file", { path });
}

export async function openFolder(path: string): Promise<void> {
  return invoke("open_folder", { path });
}

export async function readImageBase64(path: string): Promise<string> {
  return invoke("read_image_base64", { path });
}

// YouTube API functions

export async function youtubeGetAuthStatus(): Promise<YouTubeAuthStatus> {
  return invoke("youtube_get_auth_status");
}

export async function youtubeSignIn(): Promise<YouTubeAuthStatus> {
  return invoke("youtube_sign_in");
}

export async function youtubeSignOut(): Promise<void> {
  return invoke("youtube_sign_out");
}

export async function youtubeListPlaylists(): Promise<PlaylistInfo[]> {
  return invoke("youtube_list_playlists");
}

export async function youtubeUploadVideo(
  videoId: string,
  title: string,
  description: string,
  thumbnailSource?: string,
  renderKey?: string,
  privacy?: UploadPrivacy,
  startTime?: number,
  endTime?: number,
  playlistId?: string
): Promise<UploadOutcome> {
  return invoke("youtube_upload_video", {
    videoId,
    title,
    description,
    thumbnailSource,
    renderKey,
    privacy,
    startTime,
    endTime,
    playlistId,
  });
}

/** An earlier upload attempt couldn't be checked with YouTube, so nothing
 * was sent (UploadError::CheckFailed). */
export const UPLOAD_CHECK_FAILED =
  "Couldn't check the earlier upload with YouTube, so nothing was sent. Check the internet connection and try again.";

/** Forget the video's earlier upload attempt, so the next upload starts a
 * new one (it may duplicate the earlier upload if that had finished). */
export async function forgetUploadSession(videoId: string): Promise<void> {
  return invoke("forget_upload_session", { videoId });
}

/** What a cancelled upload rejects with (UploadError::Cancelled). */
export const UPLOAD_CANCELLED = "Upload cancelled.";
/** A cancel during the last chunk that couldn't tell whether it arrived
 * (UploadError::CancelledMaybeFinished). */
export const UPLOAD_CANCELLED_MAYBE_FINISHED =
  "Upload cancelled, but it may have finished just before. Uploading again checks with YouTube first.";

/** Give a finished upload of this video the current title and description. */
export async function youtubeUpdateMetadata(
  videoId: string,
  youtubeId: string,
  title: string,
  description: string
): Promise<void> {
  return invoke("youtube_update_metadata", { videoId, youtubeId, title, description });
}

/** Stop a running upload; uploading again continues where it stopped. */
export async function cancelUpload(videoId: string): Promise<void> {
  return invoke("cancel_upload", { videoId });
}

/** Set the last upload's thumbnail again; resolves with the warnings left. */
export async function retryThumbnail(videoId: string): Promise<UploadWarning[]> {
  return invoke("retry_thumbnail", { videoId });
}

export async function regenerateThumbnails(videoId: string): Promise<string[]> {
  return invoke("regenerate_thumbnails", { videoId });
}

export async function processCustomThumbnail(
  videoId: string,
  sourceImagePath: string,
  cropRect: CropRect,
  applyColorGrading: boolean
): Promise<string> {
  return invoke("process_custom_thumbnail", {
    videoId,
    sourceImagePath,
    cropRect,
    applyColorGrading,
  });
}

/** Tauri rejects with a plain string; turn anything into a message. */
export function errorMessage(err: unknown, fallback = "Something went wrong"): string {
  if (err instanceof Error) return err.message;
  return String(err || fallback);
}

/** Save a template render (JPEG bytes) of one thumbnail source; resolves
 * with its path. The bytes go as the raw invoke body, the rest as
 * percent-encoded (so ASCII) headers. */
export async function saveRenderedThumbnail(
  videoId: string,
  source: string,
  renderKey: string,
  templateVersion: number,
  imageData: Uint8Array,
): Promise<string> {
  return invoke("save_rendered_thumbnail", imageData, {
    headers: {
      "x-video-id": encodeURIComponent(videoId),
      "x-source": encodeURIComponent(source),
      "x-render-key": encodeURIComponent(renderKey),
      // The backend checks the key against the saved review drawn with it.
      "x-template-version": String(templateVersion),
    },
  });
}

/// Crop an arbitrary image for the standalone Thumbnail tab (no library video).
export async function cropStandaloneThumbnail(
  sourceImagePath: string,
  cropRect: CropRect,
): Promise<string> {
  return invoke("crop_standalone_thumbnail", { sourceImagePath, cropRect });
}

/** Save a Thumbnail-tab render as a YouTube-ready JPEG; resolves with its path. */
export async function saveStandaloneThumbnail(imageData: Uint8Array): Promise<string> {
  return invoke("save_standalone_thumbnail", imageData);
}

export async function copyFile(
  source: string,
  destination: string,
): Promise<void> {
  return invoke("copy_file", { source, destination });
}
