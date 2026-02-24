import { useEffect, useState } from "react";
import { getVideos, deleteVideo, addLocalVideo } from "@/lib/tauri";
import { open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import type { VideoInfo, ProcessingStage } from "@/lib/types";
import {
  Film,
  Trash2,
  Clock,
  CheckCircle,
  Loader2,
  AlertCircle,
  Music,
  FolderPlus,
  Eye,
} from "lucide-react";

interface DownloadProgress {
  video_id: string;
  progress: number;
  speed: string | null;
  eta: string | null;
  status: string;
}

interface ProcessingProgress {
  video_id: string;
  stage: ProcessingStage;
  progress: number;
  message: string;
}

interface ProgressInfo {
  progress: number;
  speed: string | null;
  eta: string | null;
  status: string;
  // Processing info
  processingStage?: ProcessingStage;
  processingMessage?: string;
}

interface VideoLibraryProps {
  onVideoSelect: (video: VideoInfo) => void;
  visible?: boolean;
}

export function VideoLibrary({ onVideoSelect, visible = true }: VideoLibraryProps) {
  const [videos, setVideos] = useState<VideoInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [progressMap, setProgressMap] = useState<Record<string, ProgressInfo>>({});

  useEffect(() => {
    loadVideos();
  }, []);

  // Reload videos when the library becomes visible
  useEffect(() => {
    if (visible) {
      loadVideos();
    }
  }, [visible]);

  useEffect(() => {
    const unlistenDownload = listen<DownloadProgress>("download-progress", (event) => {
      setProgressMap((prev) => ({
        ...prev,
        [event.payload.video_id]: {
          ...prev[event.payload.video_id],
          progress: event.payload.progress,
          speed: event.payload.speed,
          eta: event.payload.eta,
          status: event.payload.status,
        },
      }));
    });

    const unlistenProcessing = listen<ProcessingProgress>("processing-progress", (event) => {
      setProgressMap((prev) => ({
        ...prev,
        [event.payload.video_id]: {
          ...prev[event.payload.video_id],
          progress: event.payload.progress,
          speed: null,
          eta: null,
          status: "processing",
          processingStage: event.payload.stage,
          processingMessage: event.payload.message,
        },
      }));

      // Reload videos when processing completes
      if (event.payload.stage === "complete") {
        loadVideos();
      }
    });

    return () => {
      unlistenDownload.then((fn) => fn());
      unlistenProcessing.then((fn) => fn());
    };
  }, []);

  async function loadVideos() {
    try {
      setLoading(true);
      const videoList = await getVideos();
      setVideos(videoList);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load videos");
    } finally {
      setLoading(false);
    }
  }

  async function handleDelete(video: VideoInfo, e: React.MouseEvent) {
    e.stopPropagation();
    if (confirm(`Delete "${video.title}"? This will remove the video files.`)) {
      try {
        await deleteVideo(video.id);
        setVideos(videos.filter((v) => v.id !== video.id));
      } catch (err) {
        console.error("Failed to delete video:", err);
      }
    }
  }

  async function handleAddLocalVideo() {
    try {
      const selected = await open({
        multiple: false,
        filters: [
          {
            name: "Video",
            extensions: ["mp4", "mkv", "webm", "avi", "mov", "m4v"],
          },
        ],
      });

      if (selected) {
        const video = await addLocalVideo(selected as string);
        setVideos((prev) => [...prev, video]);
      }
    } catch (err) {
      console.error("Failed to add local video:", err);
      setError(err instanceof Error ? err.message : "Failed to add video");
    }
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex flex-col items-center justify-center h-full gap-4">
        <AlertCircle className="w-12 h-12 text-destructive" />
        <p className="text-destructive">{error}</p>
        <button
          onClick={loadVideos}
          className="px-4 py-2 bg-primary text-primary-foreground rounded-lg"
        >
          Retry
        </button>
      </div>
    );
  }

  if (videos.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full gap-4 text-muted-foreground">
        <Film className="w-16 h-16" />
        <p>No videos yet</p>
        <p className="text-sm">Download a video or add a local file to get started</p>
        <button
          onClick={handleAddLocalVideo}
          className="flex items-center gap-2 px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
        >
          <FolderPlus className="w-5 h-5" />
          Add Local Video
        </button>
      </div>
    );
  }

  return (
    <div className="p-6">
      <div className="flex items-center justify-between mb-6">
        <h2 className="text-2xl font-bold">Video Library</h2>
        <button
          onClick={handleAddLocalVideo}
          className="flex items-center gap-2 px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
        >
          <FolderPlus className="w-5 h-5" />
          Add Local Video
        </button>
      </div>

      <div className="grid gap-4">
        {videos.map((video) => (
          <VideoCard
            key={video.id}
            video={video}
            progressInfo={progressMap[video.id] ?? { progress: video.download_progress, speed: null, eta: null, status: "downloading" }}
            onClick={() => onVideoSelect(video)}
            onDelete={(e) => handleDelete(video, e)}
          />
        ))}
      </div>
    </div>
  );
}

interface VideoCardProps {
  video: VideoInfo;
  progressInfo: ProgressInfo;
  onClick: () => void;
  onDelete: (e: React.MouseEvent) => void;
}

function VideoCard({ video, progressInfo, onClick, onDelete }: VideoCardProps) {
  return (
    <div
      onClick={onClick}
      className="flex gap-4 p-4 bg-card border border-border rounded-lg cursor-pointer hover:bg-accent transition-colors"
    >
      {/* Thumbnail */}
      <div className="w-48 h-28 bg-muted rounded-lg overflow-hidden flex-shrink-0">
        {video.thumbnail ? (
          <img
            src={video.thumbnail}
            alt={video.title}
            className="w-full h-full object-cover"
          />
        ) : (
          <div className="w-full h-full flex items-center justify-center">
            <Film className="w-8 h-8 text-muted-foreground" />
          </div>
        )}
      </div>

      {/* Info */}
      <div className="flex-1 min-w-0">
        <h3 className="font-semibold truncate">{video.title}</h3>

        {video.channel && (
          <p className="text-sm text-muted-foreground">{video.channel}</p>
        )}

        <div className="flex items-center gap-4 mt-2 text-sm text-muted-foreground">
          {video.duration && (
            <span className="flex items-center gap-1">
              <Clock className="w-4 h-4" />
              {formatDuration(video.duration)}
            </span>
          )}

          <StatusBadge
            status={video.status}
            processingStage={progressInfo.processingStage || video.processing_stage || undefined}
          />
        </div>

        {/* Download Progress */}
        {video.status === "downloading" && (
          <div className="mt-2">
            <div className="h-2 bg-muted rounded-full overflow-hidden">
              <div
                className={`h-full transition-all duration-300 ${progressInfo.status === "processing" ? "bg-yellow-500 animate-pulse" : "bg-primary"}`}
                style={{ width: `${progressInfo.progress}%` }}
              />
            </div>
            <div className="flex items-center justify-between mt-1">
              {progressInfo.status === "processing" ? (
                <p className="text-xs text-muted-foreground">Processing... (merging streams)</p>
              ) : (
                <>
                  <p className="text-xs text-muted-foreground">
                    {progressInfo.progress.toFixed(1)}% {progressInfo.speed && `• ${progressInfo.speed}`}
                  </p>
                  {progressInfo.eta && (
                    <p className="text-xs text-muted-foreground">ETA: {progressInfo.eta}</p>
                  )}
                </>
              )}
            </div>
          </div>
        )}

        {/* Processing Progress */}
        {video.status === "processing" && (
          <div className="mt-2">
            <div className="h-2 bg-muted rounded-full overflow-hidden">
              <div
                className="h-full bg-orange-500 transition-all duration-300"
                style={{ width: `${progressInfo.progress || video.processing_progress || 0}%` }}
              />
            </div>
            <p className="text-xs text-muted-foreground mt-1">
              {progressInfo.processingMessage || "Processing..."}
            </p>
          </div>
        )}
      </div>

      {/* Actions */}
      <div className="flex items-start gap-2">
        <button
          onClick={onDelete}
          className="p-2 text-muted-foreground hover:text-destructive rounded-lg hover:bg-destructive/10 transition-colors"
        >
          <Trash2 className="w-5 h-5" />
        </button>
      </div>
    </div>
  );
}

function StatusBadge({ status, processingStage }: { status: VideoInfo["status"]; processingStage?: ProcessingStage }) {
  const config: Record<
    VideoInfo["status"],
    { icon: React.ReactNode; label: string; className: string }
  > = {
    pending: {
      icon: <Clock className="w-4 h-4" />,
      label: "Pending",
      className: "bg-muted text-muted-foreground",
    },
    downloading: {
      icon: <Loader2 className="w-4 h-4 animate-spin" />,
      label: "Downloading",
      className: "bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400",
    },
    downloaded: {
      icon: <CheckCircle className="w-4 h-4" />,
      label: "Downloaded",
      className: "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400",
    },
    extracting_audio: {
      icon: <Loader2 className="w-4 h-4 animate-spin" />,
      label: "Extracting Audio",
      className: "bg-purple-100 text-purple-700 dark:bg-purple-900/30 dark:text-purple-400",
    },
    audio_ready: {
      icon: <Music className="w-4 h-4" />,
      label: "Audio Ready",
      className: "bg-purple-100 text-purple-700 dark:bg-purple-900/30 dark:text-purple-400",
    },
    transcribing: {
      icon: <Loader2 className="w-4 h-4 animate-spin" />,
      label: "Transcribing",
      className: "bg-yellow-100 text-yellow-700 dark:bg-yellow-900/30 dark:text-yellow-400",
    },
    transcribed: {
      icon: <CheckCircle className="w-4 h-4" />,
      label: "Transcribed",
      className: "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400",
    },
    processing: {
      icon: <Loader2 className="w-4 h-4 animate-spin" />,
      label: getProcessingLabel(processingStage),
      className: "bg-orange-100 text-orange-700 dark:bg-orange-900/30 dark:text-orange-400",
    },
    ready: {
      icon: <CheckCircle className="w-4 h-4" />,
      label: "Ready",
      className: "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400",
    },
    ready_for_review: {
      icon: <Eye className="w-4 h-4" />,
      label: "Ready for Review",
      className: "bg-primary/10 text-primary",
    },
    error: {
      icon: <AlertCircle className="w-4 h-4" />,
      label: "Error",
      className: "bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400",
    },
  };

  const { icon, label, className } = config[status];

  return (
    <span
      className={`flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium ${className}`}
    >
      {icon}
      {label}
    </span>
  );
}

function getProcessingLabel(stage?: ProcessingStage): string {
  switch (stage) {
    case "extracting_audio":
      return "Extracting Audio";
    case "transcribing":
      return "Transcribing";
    case "trimming":
      return "Trimming";
    case "generating_thumbnails":
      return "Generating Thumbnails";
    case "complete":
      return "Complete";
    case "error":
      return "Error";
    default:
      return "Processing";
  }
}

function formatDuration(seconds: number): string {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const secs = Math.floor(seconds % 60);

  if (hours > 0) {
    return `${hours}:${minutes.toString().padStart(2, "0")}:${secs.toString().padStart(2, "0")}`;
  }
  return `${minutes}:${secs.toString().padStart(2, "0")}`;
}
