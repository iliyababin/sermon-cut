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
  review: { title: string; description: string; preacher: string },
  selectedSource: string | null
): Promise<void> {
  return invoke("save_review", { videoId, ...review, selectedSource });
}

export async function revealVideo(videoId: string): Promise<void> {
  return invoke("reveal_video", { videoId });
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

export async function cancelDownload(videoId: string): Promise<void> {
  return invoke("cancel_download", { videoId });
}

// Settings

export async function getSettings(): Promise<Settings> {
  return invoke("get_settings");
}

// Increases with every save, also across a reload of the webview, so the
// backend can drop a save that arrives after a newer one.
let lastSettingsSeq = 0;

export function nextSettingsSeq(): number {
  lastSettingsSeq = Math.max(Date.now(), lastSettingsSeq + 1);
  return lastSettingsSeq;
}

/** Save only the given fields. Resolves with the settings as they now are. */
export async function saveSettings(patch: SettingsPatch): Promise<Settings> {
  return invoke("save_settings", { patch, seq: nextSettingsSeq() });
}

export async function resetState(): Promise<void> {
  return invoke("reset_state");
}

export async function getAppDataDir(): Promise<string> {
  return invoke("get_app_data_dir");
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

/** What a cancelled upload rejects with (UploadError::Cancelled). */
export const UPLOAD_CANCELLED = "Upload cancelled.";

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
  applyColorGrading: boolean,
  applyLogoOverlay: boolean
): Promise<string> {
  return invoke("process_custom_thumbnail", {
    videoId,
    sourceImagePath,
    cropRect,
    applyColorGrading,
    applyLogoOverlay,
  });
}

/** Tauri rejects with a plain string; turn anything into a message. */
export function errorMessage(err: unknown, fallback = "Something went wrong"): string {
  if (err instanceof Error) return err.message;
  return String(err || fallback);
}

/** Save a template render of one thumbnail source; resolves with its path. */
export async function saveRenderedThumbnail(
  videoId: string,
  source: string,
  renderKey: string,
  imageData: Uint8Array,
): Promise<string> {
  return invoke("save_rendered_thumbnail", {
    videoId,
    source,
    renderKey,
    imageData: Array.from(imageData),
  });
}

/// Crop an arbitrary image for the standalone Thumbnail tab (no library video).
export async function cropStandaloneThumbnail(
  sourceImagePath: string,
  cropRect: CropRect,
): Promise<string> {
  return invoke("crop_standalone_thumbnail", { sourceImagePath, cropRect });
}

/** Save a Thumbnail-tab render (PNG bytes); resolves with its path. */
export async function saveStandaloneThumbnail(imageData: Uint8Array): Promise<string> {
  return invoke("save_standalone_thumbnail", { imageData: Array.from(imageData) });
}

export async function copyFile(
  source: string,
  destination: string,
): Promise<void> {
  return invoke("copy_file", { source, destination });
}
