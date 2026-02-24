import { useState, useEffect } from "react";
import { getVideoInfo, downloadVideo, processVideoFull } from "@/lib/tauri";
import { listen } from "@tauri-apps/api/event";
import type { VideoInfo } from "@/lib/types";
import { Loader2, AlertCircle, Clock, User, Zap } from "lucide-react";

interface DownloadProgress {
  video_id: string;
  progress: number;
  speed: string | null;
  eta: string | null;
  status: string;
}

interface DownloadFormProps {
  onGoToLibrary: () => void;
}

export function DownloadForm({ onGoToLibrary }: DownloadFormProps) {
  const [url, setUrl] = useState("");
  const [quality, setQuality] = useState("best");
  const [loading, setLoading] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const [downloadProgress, setDownloadProgress] = useState(0);
  const [downloadSpeed, setDownloadSpeed] = useState<string | null>(null);
  const [downloadEta, setDownloadEta] = useState<string | null>(null);
  const [downloadStatus, setDownloadStatus] = useState<string>("downloading");
  const [error, setError] = useState<string | null>(null);
  const [videoInfo, setVideoInfo] = useState<VideoInfo | null>(null);
  const [isProcessing, setIsProcessing] = useState(false);

  useEffect(() => {
    const unlisten = listen<DownloadProgress>("download-progress", (event) => {
      if (videoInfo && event.payload.video_id === videoInfo.id) {
        setDownloadProgress(event.payload.progress);
        setDownloadSpeed(event.payload.speed);
        setDownloadEta(event.payload.eta);
        setDownloadStatus(event.payload.status);
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, [videoInfo]);

  async function handleFetchInfo() {
    if (!url.trim()) {
      setError("Please enter a YouTube URL");
      return;
    }

    try {
      setLoading(true);
      setError(null);
      const info = await getVideoInfo(url);
      setVideoInfo(info);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to fetch video info");
      setVideoInfo(null);
    } finally {
      setLoading(false);
    }
  }

  async function handleDownloadAndProcess() {
    if (!videoInfo) return;

    try {
      setDownloading(true);
      setDownloadProgress(0);
      setDownloadStatus("downloading");
      setError(null);

      // Step 1: Download the video
      const result = await downloadVideo(url, quality);

      // Step 2: Start processing in background and redirect to library
      setIsProcessing(true);
      setDownloading(false);

      // Start processing (this runs in background, we don't await)
      processVideoFull(result.id).catch((err) => {
        console.error("Background processing failed:", err);
      });

      // Redirect to library immediately
      onGoToLibrary();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Download failed");
      setDownloading(false);
      setIsProcessing(false);
      setDownloadProgress(0);
    }
  }

  return (
    <div className="p-6 max-w-2xl mx-auto">
      <h2 className="text-2xl font-bold mb-6">Download Video</h2>

      {/* URL Input */}
      <div className="space-y-4">
        <div>
          <label className="block text-sm font-medium mb-2">YouTube URL</label>
          <div className="flex gap-2">
            <input
              type="text"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://youtube.com/watch?v=..."
              className="flex-1 px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring"
              onKeyDown={(e) => e.key === "Enter" && handleFetchInfo()}
            />
            <button
              onClick={handleFetchInfo}
              disabled={loading}
              className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
            >
              {loading ? (
                <Loader2 className="w-5 h-5 animate-spin" />
              ) : (
                "Fetch"
              )}
            </button>
          </div>
        </div>

        {/* Quality Selector */}
        <div>
          <label className="block text-sm font-medium mb-2">Quality</label>
          <select
            value={quality}
            onChange={(e) => setQuality(e.target.value)}
            className="w-full px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring bg-background"
          >
            <option value="best">Best Available</option>
            <option value="2160p">4K (2160p)</option>
            <option value="1440p">1440p</option>
            <option value="1080p">1080p</option>
            <option value="720p">720p</option>
            <option value="480p">480p</option>
          </select>
        </div>

        {/* Error */}
        {error && (
          <div className="flex items-center gap-2 p-4 bg-destructive/10 text-destructive rounded-lg">
            <AlertCircle className="w-5 h-5 flex-shrink-0" />
            <p>{error}</p>
          </div>
        )}

        {/* Video Preview */}
        {videoInfo && (
          <div className="border border-border rounded-lg overflow-hidden">
            {/* Thumbnail */}
            {videoInfo.thumbnail && (
              <img
                src={videoInfo.thumbnail}
                alt={videoInfo.title}
                className="w-full h-48 object-cover"
              />
            )}

            <div className="p-4">
              <h3 className="font-semibold text-lg">{videoInfo.title}</h3>

              <div className="flex items-center gap-4 mt-2 text-sm text-muted-foreground">
                {videoInfo.channel && (
                  <span className="flex items-center gap-1">
                    <User className="w-4 h-4" />
                    {videoInfo.channel}
                  </span>
                )}
                {videoInfo.duration && (
                  <span className="flex items-center gap-1">
                    <Clock className="w-4 h-4" />
                    {formatDuration(videoInfo.duration)}
                  </span>
                )}
              </div>

              {downloading || isProcessing ? (
                <div className="mt-4">
                  <div className="flex items-center justify-between mb-2">
                    <span className="text-sm font-medium">
                      {isProcessing
                        ? "Starting processing..."
                        : downloadStatus === "processing"
                          ? "Processing..."
                          : "Downloading..."}
                    </span>
                    <span className="text-sm text-muted-foreground">
                      {isProcessing
                        ? "Redirecting to library"
                        : downloadStatus === "processing"
                          ? "Merging streams"
                          : `${downloadProgress.toFixed(1)}%`}
                    </span>
                  </div>
                  <div className="h-3 bg-muted rounded-full overflow-hidden">
                    <div
                      className={`h-full transition-all duration-300 ${
                        isProcessing || downloadStatus === "processing"
                          ? "bg-yellow-500 animate-pulse"
                          : "bg-primary"
                      }`}
                      style={{ width: isProcessing ? "100%" : `${downloadProgress}%` }}
                    />
                  </div>
                  {!isProcessing && downloadStatus !== "processing" && (
                    <div className="flex items-center justify-between mt-2 text-xs text-muted-foreground">
                      <span>{downloadSpeed || "Calculating..."}</span>
                      {downloadEta && <span>ETA: {downloadEta}</span>}
                    </div>
                  )}
                </div>
              ) : (
                <button
                  onClick={handleDownloadAndProcess}
                  className="w-full mt-4 px-4 py-3 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 flex items-center justify-center gap-2 transition-colors"
                >
                  <Zap className="w-5 h-5" />
                  Download & Process
                </button>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
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
