import { invoke } from "@tauri-apps/api/core";
import type { VideoInfo, TranscriptionResult, Settings, YouTubeAuthStatus, CropRect, PlaylistInfo } from "./types";

export async function getVideoInfo(url: string): Promise<VideoInfo> {
  return invoke("get_video_info", { url });
}

export async function downloadVideo(
  url: string,
  quality?: string
): Promise<VideoInfo> {
  return invoke("download_video", { url, quality });
}

export async function cancelDownload(videoId: string): Promise<void> {
  return invoke("cancel_download", { videoId });
}

export async function getDownloadProgress(videoId: string): Promise<number> {
  return invoke("get_download_progress", { videoId });
}

export async function extractAudio(videoId: string): Promise<string> {
  return invoke("extract_audio", { videoId });
}

export async function transcribeAudio(
  videoId: string,
  forceRefresh?: boolean
): Promise<TranscriptionResult> {
  return invoke("transcribe_audio", { videoId, forceRefresh });
}

export async function trimVideo(
  videoId: string,
  startTime: number,
  endTime: number
): Promise<string> {
  return invoke("trim_video", { videoId, startTime, endTime });
}

export async function getVideos(): Promise<VideoInfo[]> {
  return invoke("get_videos");
}

export async function deleteVideo(videoId: string): Promise<void> {
  return invoke("delete_video", { videoId });
}

export async function getSettings(): Promise<Settings> {
  return invoke("get_settings");
}

export async function saveSettings(settings: Settings): Promise<void> {
  return invoke("save_settings", { settings });
}

export async function resetState(): Promise<void> {
  return invoke("reset_state");
}

export async function getAppDataDir(): Promise<string> {
  return invoke("get_app_data_dir");
}

export async function addLocalVideo(filePath: string): Promise<VideoInfo> {
  return invoke("add_local_video", { filePath });
}

export async function generateThumbnail(
  videoId: string,
  startTime: number,
  endTime: number,
  title: string,
  logoPath?: string
): Promise<string> {
  return invoke("generate_thumbnail", {
    videoId,
    startTime,
    endTime,
    title,
    logoPath,
  });
}

export async function generateThumbnailOptions(
  videoId: string,
  startTime: number,
  endTime: number,
  title: string,
  count?: number,
  logoPath?: string
): Promise<string[]> {
  return invoke("generate_thumbnail_options", {
    videoId,
    startTime,
    endTime,
    title,
    count,
    logoPath,
  });
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
  videoPath: string,
  title: string,
  description: string,
  thumbnailPath?: string,
  privacy?: "public" | "unlisted" | "private",
  sourceVideoId?: string,
  startTime?: number,
  endTime?: number
): Promise<string> {
  return invoke("youtube_upload_video", {
    videoPath,
    title,
    description,
    thumbnailPath,
    privacy,
    sourceVideoId,
    startTime,
    endTime,
  });
}

export async function processVideoFull(videoId: string): Promise<VideoInfo> {
  return invoke("process_video_full", { videoId });
}

export async function updateVideoMetadata(
  videoId: string,
  title?: string,
  description?: string,
  thumbnailPath?: string
): Promise<VideoInfo> {
  return invoke("update_video_metadata", {
    videoId,
    title,
    description,
    thumbnailPath,
  });
}

export async function retrimVideo(
  videoId: string,
  startTime: number,
  endTime: number
): Promise<string> {
  return invoke("retrim_video", { videoId, startTime, endTime });
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
