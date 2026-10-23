import { useState, useEffect, useRef } from "react";
import { getVideoInfo, downloadVideo, cancelDownload, getSettings, errorMessage } from "@/lib/tauri";
import { listen } from "@tauri-apps/api/event";
import type { VideoInfo } from "@/lib/types";
import { Loader2, Clock, User, Zap, Download, X } from "lucide-react";
import { ErrorBanner } from "./ErrorBanner";

interface DownloadProgress {
  video_id: string;
  progress: number;
  speed: string | null;
  eta: string | null;
  status: string;
}

interface DownloadFormProps {
  onGoToLibrary: () => void;
  /** Whether this screen is showing; a finished download only switches to
   * the library if it still is. */
  isActive: boolean;
}

export function DownloadForm({ onGoToLibrary, isActive }: DownloadFormProps) {
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
  // The URL videoInfo was fetched for.
  const [fetchedUrl, setFetchedUrl] = useState<string | null>(null);
  const active = useRef(isActive);
  active.current = isActive;

  // Start from the quality chosen in Settings each time the screen opens.
  useEffect(() => {
    if (!isActive || downloading) return;
    getSettings()
      .then((s) => setQuality(s.download_quality || "best"))
      .catch(() => {});
  }, [isActive]);

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

  async function handleFetchInfo(target = url) {
    const wanted = target.trim();
    if (!wanted) {
      setError("Please enter a YouTube URL");
      return;
    }

    try {
      setLoading(true);
      setError(null);
      const info = await getVideoInfo(wanted);
      setVideoInfo(info);
      setFetchedUrl(wanted);
    } catch (err) {
      setError(errorMessage(err, "Failed to fetch video info"));
      setVideoInfo(null);
    } finally {
      setLoading(false);
    }
  }

  async function handleDownload(process: boolean) {
    if (!videoInfo) return;

    try {
      setDownloading(true);
      setDownloadProgress(0);
      setDownloadStatus("downloading");
      setError(null);

      // Downloads into the library folder; with `process` it's queued for
      // processing as soon as it lands.
      await downloadVideo(fetchedUrl ?? url.trim(), quality, process);

      setDownloading(false);
      setVideoInfo(null);
      setFetchedUrl(null);
      setUrl("");
      if (active.current) onGoToLibrary();
    } catch (err) {
      setError(errorMessage(err, "Download failed"));
      setDownloading(false);
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
              aria-label="YouTube URL"
              disabled={downloading}
              className="flex-1 px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring"
              onPaste={(e) => {
                // Pasting into an empty field looks the video up straight away.
                if (url.trim() || loading) return;
                const pasted = e.clipboardData.getData("text").trim();
                if (!pasted) return;
                e.preventDefault();
                setUrl(pasted);
                handleFetchInfo(pasted);
              }}
              onKeyDown={(e) => {
                if (e.key !== "Enter" || loading || downloading) return;
                // Enter looks the video up, then (once shown) downloads and
                // processes it.
                if (videoInfo && fetchedUrl === url.trim()) handleDownload(true);
                else handleFetchInfo();
              }}
            />
            <button
              onClick={() => handleFetchInfo()}
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
          <label htmlFor="download-quality" className="block text-sm font-medium mb-2">Quality</label>
          <select
            id="download-quality"
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
          <ErrorBanner message={error} />
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

              {downloading ? (
                <div className="mt-4">
                  <div className="flex items-center justify-between mb-2">
                    <span className="text-sm font-medium">
                      {downloadStatus === "processing" ? "Finishing..." : "Downloading..."}
                    </span>
                    <div className="flex items-center gap-2">
                      <span className="text-sm text-muted-foreground">
                        {downloadStatus === "processing"
                          ? "Merging streams"
                          : `${downloadProgress.toFixed(1)}%`}
                      </span>
                      <button
                        onClick={() =>
                          cancelDownload(videoInfo.id).catch((err) =>
                            setError(errorMessage(err, "Couldn't cancel the download"))
                          )
                        }
                        className="p-1 text-muted-foreground hover:text-destructive rounded"
                        title="Cancel download"
                      >
                        <X className="w-4 h-4" />
                      </button>
                    </div>
                  </div>
                  <div className="h-3 bg-muted rounded-full overflow-hidden">
                    <div
                      className={`h-full transition-all duration-300 ${
                        downloadStatus === "processing"
                          ? "bg-yellow-500 animate-pulse"
                          : "bg-primary"
                      }`}
                      style={{ width: `${downloadProgress}%` }}
                    />
                  </div>
                  {downloadStatus !== "processing" && (
                    <div className="flex items-center justify-between mt-2 text-xs text-muted-foreground">
                      <span>{downloadSpeed || "Calculating..."}</span>
                      {downloadEta && <span>ETA: {downloadEta}</span>}
                    </div>
                  )}
                  <p className="mt-2 text-xs text-muted-foreground">
                    You can leave this page; the download keeps going and shows up in the library.
                  </p>
                </div>
              ) : (
                <div className="mt-4 flex gap-2">
                  <button
                    onClick={() => handleDownload(false)}
                    className="flex-1 px-4 py-3 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 flex items-center justify-center gap-2 transition-colors"
                  >
                    <Download className="w-5 h-5" />
                    Download
                  </button>
                  <button
                    onClick={() => handleDownload(true)}
                    className="flex-1 px-4 py-3 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 flex items-center justify-center gap-2 transition-colors"
                  >
                    <Zap className="w-5 h-5" />
                    Download & Process
                  </button>
                </div>
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
