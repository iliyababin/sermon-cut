import { useState, useEffect } from "react";
import {
  regenerateThumbnails,
  openFolder,
  youtubeGetAuthStatus,
  youtubeUploadVideo,
  processVideoFull,
} from "@/lib/tauri";
import { listen } from "@tauri-apps/api/event";
import type { VideoInfo, YouTubeAuthStatus, Chapter } from "@/lib/types";
import { LocalImage } from "./LocalImage";
import { ThumbnailEditor } from "./ThumbnailEditor";
import {
  formatTimeDisplay,
} from "@/lib/clips";
import {
  ArrowLeft,
  FolderOpen,
  Youtube,
  Loader2,
  CheckCircle,
  AlertCircle,
  RefreshCw,
  ExternalLink,
  Pencil,
  RotateCcw,
} from "lucide-react";

const SOCIAL_LINKS = [
  "Website: https://www.sccyouth.com",
  "Instagram: https://www.instagram.com/youthscc",
  "Facebook: https://www.facebook.com/YouthSCCMinistry/",
].join("\n");

interface ReviewScreenProps {
  video: VideoInfo;
  onBack: () => void;
  onVideoUpdate: (video: VideoInfo) => void;
}

export function ReviewScreen({ video, onBack, onVideoUpdate }: ReviewScreenProps) {
  // Metadata editing
  const [title, setTitle] = useState(
    video.transcription?.suggested_title || video.title
  );
  const [description, setDescription] = useState(
    video.transcription?.suggested_description || ""
  );
  const [chapters, setChapters] = useState<Chapter[]>(
    video.transcription?.suggested_chapters || []
  );
  const [preacher, setPreacher] = useState("");

  // Trim boundaries (from transcription, used for chapter calculation)
  const [startTime, setStartTime] = useState(
    video.transcription?.sermon_start || 0
  );
  const [endTime, setEndTime] = useState(
    video.transcription?.sermon_end || video.duration || 0
  );

  // Thumbnails
  const [thumbnailOptions, setThumbnailOptions] = useState<string[]>(
    video.thumbnail_options || []
  );
  const [selectedThumbnail, setSelectedThumbnail] = useState(
    video.thumbnail_path || (video.thumbnail_options?.[0] ?? null)
  );
  const [isRegeneratingThumbnails, setIsRegeneratingThumbnails] = useState(false);
  const [isEditorOpen, setIsEditorOpen] = useState(false);

  // Upload state
  const [youtubeAuth, setYoutubeAuth] = useState<YouTubeAuthStatus | null>(null);
  const [isUploading, setIsUploading] = useState(false);
  const [uploadProgress, setUploadProgress] = useState(0);
  const [uploadedUrl, setUploadedUrl] = useState<string | null>(null);
  const [uploadError, setUploadError] = useState<string | null>(null);

  // Reprocessing state
  const [isReprocessing, setIsReprocessing] = useState(false);

  // Load YouTube auth status
  useEffect(() => {
    youtubeGetAuthStatus().then(setYoutubeAuth).catch(console.error);
  }, []);

  // Listen for upload progress
  useEffect(() => {
    const unlisten = listen<{ progress: number; status: string }>(
      "youtube-upload-progress",
      (event) => {
        setUploadProgress((prev) => Math.max(prev, event.payload.progress));
      }
    );

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const handleRegenerateThumbnails = async () => {
    try {
      setIsRegeneratingThumbnails(true);
      const newOptions = await regenerateThumbnails(video.id);
      setThumbnailOptions(newOptions);
      if (newOptions.length > 0) {
        setSelectedThumbnail(newOptions[0]);
      }
    } catch (err) {
      console.error("Failed to regenerate thumbnails:", err);
    } finally {
      setIsRegeneratingThumbnails(false);
    }
  };

  const handleSelectThumbnail = (path: string) => {
    setSelectedThumbnail(path);
  };

  const handleThumbnailEditorSave = (newPath: string) => {
    // Add to options if not already there
    if (!thumbnailOptions.includes(newPath)) {
      setThumbnailOptions((prev) => [newPath, ...prev]);
    }
    setSelectedThumbnail(newPath);
    setIsEditorOpen(false);
  };

  const handleOpenFolder = async () => {
    const pathToOpen = video.trimmed_path || video.file_path;
    if (pathToOpen) {
      await openFolder(pathToOpen);
    }
  };

  const handleReprocess = async () => {
    if (!confirm("This will re-transcribe the video and regenerate all suggestions. Continue?")) {
      return;
    }

    try {
      setIsReprocessing(true);
      const updatedVideo = await processVideoFull(video.id);
      onVideoUpdate(updatedVideo);

      // Reset local state with new data
      setTitle(updatedVideo.transcription?.suggested_title || updatedVideo.title);
      setDescription(updatedVideo.transcription?.suggested_description || "");
      setChapters(updatedVideo.transcription?.suggested_chapters || []);
      setStartTime(updatedVideo.transcription?.sermon_start || 0);
      setEndTime(updatedVideo.transcription?.sermon_end || updatedVideo.duration || 0);
      setThumbnailOptions(updatedVideo.thumbnail_options || []);
      setSelectedThumbnail(updatedVideo.thumbnail_path || updatedVideo.thumbnail_options?.[0] || null);

      // Reset upload state
      setUploadedUrl(null);
    } catch (err) {
      console.error("Reprocessing failed:", err);
      alert(`Reprocessing failed: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setIsReprocessing(false);
    }
  };

  const handleUpload = async () => {
    const videoPath = video.trimmed_path || video.file_path;
    if (!videoPath) return;

    try {
      setIsUploading(true);
      setUploadError(null);
      setUploadProgress(0);

      // Build description with chapters (adjusted for clip) and social links
      let fullDescription = description;
      const adjustedChapters = chapters.filter(
        (ch) => ch.time >= startTime && ch.time <= endTime
      );
      if (adjustedChapters.length > 0) {
        fullDescription += "\n\nChapters:\n";
        fullDescription += adjustedChapters
          .map((ch) => `${formatTime(ch.time - startTime)} ${ch.title}`)
          .join("\n");
      }

      // Add social links footer
      fullDescription += `\n\n---\n${SOCIAL_LINKS}`;

      const url = await youtubeUploadVideo(
        videoPath,
        title,
        fullDescription,
        selectedThumbnail || undefined,
        "public",
        video.id,    // sourceVideoId for captions
        startTime,   // trim start for caption adjustment
        endTime      // trim end for caption adjustment
      );
      setUploadedUrl(url);
      setIsUploading(false);
    } catch (err) {
      console.error("Upload failed:", err);
      setUploadError(err instanceof Error ? err.message : String(err));
      setIsUploading(false);
    }
  };

  const formatTime = formatTimeDisplay;

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="p-4 border-b border-border flex items-center justify-between">
        <div className="flex items-center gap-4">
          <button
            onClick={onBack}
            className="p-2 hover:bg-secondary rounded-lg transition-colors"
          >
            <ArrowLeft className="w-5 h-5" />
          </button>
          <div>
            <h2 className="font-semibold">{video.title}</h2>
            <p className="text-sm text-muted-foreground">{video.channel}</p>
          </div>
        </div>

        <div className="flex items-center gap-2">
          <button
            onClick={handleReprocess}
            disabled={isReprocessing}
            title="Re-transcribe and regenerate all suggestions"
            className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 flex items-center gap-2 transition-colors"
          >
            {isReprocessing ? (
              <Loader2 className="w-4 h-4 animate-spin" />
            ) : (
              <RotateCcw className="w-4 h-4" />
            )}
            Reprocess
          </button>
          {youtubeAuth?.is_authenticated && !uploadedUrl && (
            <button
              onClick={handleUpload}
              disabled={isUploading || !preacher.trim() || !video.url}
              title={!preacher.trim() ? "Speaker name is required" : !video.url ? "Original YouTube URL is required" : undefined}
              className="px-4 py-2 bg-red-600 text-white rounded-lg hover:bg-red-700 disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 transition-colors"
            >
              {isUploading ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  {uploadProgress}%
                </>
              ) : (
                <>
                  <Youtube className="w-4 h-4" />
                  Upload
                </>
              )}
            </button>
          )}
        </div>
      </div>

      {/* Upload Success Banner */}
      {uploadedUrl && (
        <div className="mx-4 mt-4 p-4 bg-green-100 dark:bg-green-900/30 rounded-lg">
          <div className="flex items-center justify-between">
            <div className="flex flex-col gap-2">
              <div className="flex items-center gap-2">
                <CheckCircle className="w-5 h-5 text-green-600" />
                <span className="font-medium text-green-800 dark:text-green-400">
                  Uploaded to YouTube
                </span>
              </div>
            </div>
            <div className="flex items-center gap-2">
              <button
                onClick={() => {
                  setUploadedUrl(null);
                  setUploadProgress(0);
                }}
                className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
              >
                Upload Again
              </button>
              <a
                href={uploadedUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-2 px-4 py-2 bg-green-600 text-white rounded-lg hover:bg-green-700 transition-colors"
              >
                Open on YouTube
                <ExternalLink className="w-4 h-4" />
              </a>
            </div>
          </div>
        </div>
      )}

      {/* Upload Error Banner */}
      {uploadError && (
        <div className="mx-4 mt-4 p-4 bg-destructive/10 rounded-lg flex items-center justify-between">
          <div className="flex items-center gap-3">
            <AlertCircle className="w-6 h-6 text-destructive" />
            <span className="text-destructive">{uploadError}</span>
          </div>
          <button
            onClick={handleUpload}
            className="px-4 py-2 bg-red-600 text-white rounded-lg hover:bg-red-700 transition-colors"
          >
            Retry
          </button>
        </div>
      )}

      {/* Main Content */}
      <div className="flex-1 overflow-auto p-6">
        <div className="max-w-5xl mx-auto space-y-6">
          {/* Video Preview */}
          <div className="bg-card border border-border rounded-lg overflow-hidden">
            <div className="aspect-video bg-black flex items-center justify-center">
              {selectedThumbnail ? (
                <LocalImage
                  path={selectedThumbnail}
                  alt="Selected thumbnail"
                  className="w-full h-full object-cover"
                />
              ) : (
                <p className="text-muted-foreground">No thumbnail selected</p>
              )}
            </div>
          </div>

          {/* Two Column Layout */}
          <div className="grid md:grid-cols-2 gap-6">
            {/* Thumbnails */}
            <div className="bg-card border border-border rounded-lg p-4">
              <div className="flex items-center justify-between mb-4">
                <h3 className="font-semibold">Thumbnails</h3>
                <div className="flex items-center gap-2">
                  <button
                    onClick={() => setIsEditorOpen(true)}
                    disabled={thumbnailOptions.length === 0}
                    className="text-sm px-3 py-1.5 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 flex items-center gap-1.5 transition-colors"
                  >
                    <Pencil className="w-3 h-3" />
                    Edit
                  </button>
                  <button
                    onClick={handleRegenerateThumbnails}
                    disabled={isRegeneratingThumbnails}
                    className="text-sm px-3 py-1.5 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 flex items-center gap-1.5 transition-colors"
                  >
                    {isRegeneratingThumbnails ? (
                      <>
                        <Loader2 className="w-3 h-3 animate-spin" />
                        Regenerating...
                      </>
                    ) : (
                      <>
                        <RefreshCw className="w-3 h-3" />
                        Regenerate
                      </>
                    )}
                  </button>
                </div>
              </div>

              <div className="grid grid-cols-3 gap-2">
                {thumbnailOptions.map((path, index) => (
                  <button
                    key={path}
                    onClick={() => handleSelectThumbnail(path)}
                    className={`relative aspect-video rounded-lg overflow-hidden border-2 transition-colors ${
                      selectedThumbnail === path
                        ? "border-primary ring-2 ring-primary/30"
                        : "border-transparent hover:border-muted-foreground/30"
                    }`}
                  >
                    <LocalImage
                      path={path}
                      alt={`Thumbnail option ${index + 1}`}
                      className="w-full h-full object-cover"
                    />
                    {selectedThumbnail === path && (
                      <div className="absolute top-1 right-1 bg-primary text-primary-foreground rounded-full p-0.5">
                        <CheckCircle className="w-3 h-3" />
                      </div>
                    )}
                  </button>
                ))}
              </div>

              {thumbnailOptions.length === 0 && (
                <p className="text-muted-foreground text-sm text-center py-8">
                  No thumbnails generated yet
                </p>
              )}
            </div>

            {/* Metadata */}
            <div className="bg-card border border-border rounded-lg p-4 space-y-4">
              <h3 className="font-semibold">Metadata</h3>

              <div>
                <label className="block text-sm font-medium mb-1">Title</label>
                <input
                  type="text"
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                  className="w-full px-3 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring"
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">
                  Preacher / Speaker <span className="text-red-500">*</span>
                </label>
                <input
                  type="text"
                  value={preacher}
                  onChange={(e) => setPreacher(e.target.value)}
                  placeholder="Enter speaker name"
                  className={`w-full px-3 py-2 border rounded-lg focus:outline-none focus:ring-2 focus:ring-ring ${
                    !preacher.trim() ? "border-red-300" : "border-input"
                  }`}
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">
                  Original YouTube URL <span className="text-red-500">*</span>
                </label>
                <input
                  type="text"
                  value={video.url}
                  readOnly
                  className={`w-full px-3 py-2 border rounded-lg bg-muted text-muted-foreground ${
                    !video.url ? "border-red-300" : "border-input"
                  }`}
                />
              </div>

              <div>
                <label className="block text-sm font-medium mb-1">
                  Description
                </label>
                <textarea
                  value={description}
                  onChange={(e) => setDescription(e.target.value)}
                  rows={4}
                  className="w-full px-3 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring resize-none"
                />
              </div>

              {chapters.filter((ch) => ch.time >= startTime && ch.time <= endTime).length > 0 && (
                <div>
                  <label className="block text-sm font-medium mb-1">
                    Chapters
                  </label>
                  <div className="text-sm text-muted-foreground space-y-1 bg-muted p-2 rounded-lg max-h-32 overflow-y-auto">
                    {chapters
                      .filter((ch) => ch.time >= startTime && ch.time <= endTime)
                      .map((ch, index) => (
                      <div key={index} className="flex gap-2">
                        <span className="font-mono">{formatTime(ch.time - startTime)}</span>
                        <span>{ch.title}</span>
                      </div>
                    ))}
                  </div>
                </div>
              )}

            </div>
          </div>

          {/* Actions Footer */}
          <div className="flex justify-between items-center pt-4 border-t border-border">
            <button
              onClick={handleOpenFolder}
              className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 flex items-center gap-2 transition-colors"
            >
              <FolderOpen className="w-4 h-4" />
              Open Folder
            </button>

            {!youtubeAuth?.is_authenticated && (
              <p className="text-sm text-muted-foreground">
                Sign in to YouTube in Settings to enable upload
              </p>
            )}
          </div>
        </div>
      </div>

      {/* Thumbnail Editor Modal */}
      {isEditorOpen && (
        <ThumbnailEditor
          videoId={video.id}
          thumbnailOptions={thumbnailOptions}
          selectedThumbnail={selectedThumbnail}
          onSave={handleThumbnailEditorSave}
          onClose={() => setIsEditorOpen(false)}
        />
      )}
    </div>
  );
}
