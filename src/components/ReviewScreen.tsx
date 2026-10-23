import { useState, useEffect, useMemo, useRef } from "react";
import {
  regenerateThumbnails,
  getVideo,
  revealVideo,
  youtubeGetAuthStatus,
  youtubeUploadVideo,
  processVideo,
  resetVideo,
  saveReview,
  saveRenderedThumbnail,
  errorMessage,
} from "@/lib/tauri";
import { renderThumbnailBatch } from "@/lib/thumbnailRenderer";
import { getDefaultTemplate } from "@/templates";
import { splitTitleIntoLines } from "@/lib/utils";
import { renderKey } from "./review/renderKey";
import { listen } from "@tauri-apps/api/event";
import { ask } from "@tauri-apps/plugin-dialog";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import type { VideoDetail, YouTubeAuthStatus, Chapter, ThumbnailOption, ThumbnailRender } from "@/lib/types";
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
  video: VideoDetail;
  onBack: () => void;
}

const AUTOSAVE_MS = 1000;
const RENDER_DEBOUNCE_MS = 1500;

type RenderMap = Record<string, { path: string; key: string }>;

function rendersById(renders: ThumbnailRender[]): RenderMap {
  return Object.fromEntries(renders.map((r) => [r.source, { path: r.path, key: r.render_key }]));
}

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

export function ReviewScreen({ video, onBack }: ReviewScreenProps) {
  // Metadata editing: saved edits first, then the AI's suggestions
  const [title, setTitle] = useState(
    video.review.title ?? video.transcription?.suggested_title ?? video.title
  );
  const [description, setDescription] = useState(
    video.review.description ?? video.transcription?.suggested_description ?? ""
  );
  const chapters: Chapter[] = video.transcription?.suggested_chapters || [];
  const [preacher, setPreacher] = useState(video.review.preacher ?? "");

  // Trim boundaries (what the trimmed video was cut to, used for chapters and captions)
  const startTime = video.trim_start ?? video.transcription?.sermon_start ?? 0;
  const endTime = video.trim_end ?? video.transcription?.sermon_end ?? video.duration ?? 0;

  // Thumbnails: sources by id, plus a stored template render per source.
  const [thumbnailOptions, setThumbnailOptions] = useState<ThumbnailOption[]>(video.thumbnail_options);
  const [selectedSource, setSelectedSource] = useState<string | null>(
    video.selected_source ?? video.thumbnail_options[0]?.id ?? null
  );
  const [renders, setRenders] = useState<RenderMap>(() => rendersById(video.renders));
  const [isRegeneratingThumbnails, setIsRegeneratingThumbnails] = useState(false);

  // The template draws title/speaker/logo over each source; the result is
  // saved into the video's library folder. A render is current when its key
  // matches what it would be rendered with now; only stale ones are redone.
  const [singleLine, setSingleLine] = useState(false);
  const [renderProgress, setRenderProgress] = useState("");
  const titleLines = useMemo(
    () => (singleLine ? [title.trim()] : splitTitleIntoLines(title)).filter((l) => l),
    [title, singleLine]
  );
  const canRender = titleLines.length > 0 && preacher.trim() !== "";
  const keys = useMemo(() => {
    const inputs = { titleLines, preacher, singleLine };
    return Object.fromEntries(thumbnailOptions.map((o) => [o.id, renderKey(o, inputs)]));
  }, [thumbnailOptions, titleLines, preacher, singleLine]);
  const staleOptions = canRender ? thumbnailOptions.filter((o) => renders[o.id]?.key !== keys[o.id]) : [];
  const staleIds = staleOptions.map((o) => o.id).join("|");

  useEffect(() => {
    if (staleOptions.length === 0) return;
    let cancelled = false;
    const inputs = { titleLines, preacher };
    const todo = staleOptions.map((o) => ({ option: o, key: keys[o.id] }));

    // Debounced so typing in the title field doesn't start a render per
    // keystroke; right away when nothing has been rendered yet.
    const delay = Object.keys(renders).length === 0 ? 0 : RENDER_DEBOUNCE_MS;
    const timeout = setTimeout(async () => {
      try {
        const template = getDefaultTemplate();
        for (let i = 0; i < todo.length; i++) {
          if (cancelled) return;
          setRenderProgress(`Updating thumbnails ${i + 1}/${todo.length}...`);
          const { option, key } = todo[i];
          const [result] = await renderThumbnailBatch(template, [option.render_from], inputs.titleLines, inputs.preacher);
          if (cancelled) return;
          const path = await saveRenderedThumbnail(video.id, option.id, key, result.imageData);
          if (cancelled) return;
          setRenders((prev) => ({ ...prev, [option.id]: { path, key } }));
        }
      } catch (err) {
        console.error("Failed to render thumbnails:", err);
      } finally {
        if (!cancelled) setRenderProgress("");
      }
    }, delay);

    return () => {
      cancelled = true;
      clearTimeout(timeout);
    };
    // staleIds + keys capture everything a render depends on.
  }, [staleIds, keys, video.id]);

  const shownPath = (o: ThumbnailOption) => renders[o.id]?.path ?? o.path;
  const selectedOption = thumbnailOptions.find((o) => o.id === selectedSource) ?? thumbnailOptions[0] ?? null;

  const [isEditorOpen, setIsEditorOpen] = useState(false);

  // Upload state
  const [youtubeAuth, setYoutubeAuth] = useState<YouTubeAuthStatus | null>(null);
  // The upload runs in the background; if this screen was reopened while one
  // is going, pick it up from the library's state.
  const [isUploading, setIsUploading] = useState(video.status === "uploading");
  const [uploadProgress, setUploadProgress] = useState(video.upload_progress ?? 0);
  const lastUpload = video.uploads[video.uploads.length - 1];
  const [uploadedUrl, setUploadedUrl] = useState<string | null>(
    video.status === "done" && lastUpload ? lastUpload.url : null
  );
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [uploadCount, setUploadCount] = useState(video.uploads.length);

  // Reprocessing state
  const [isReprocessing, setIsReprocessing] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  // Autosave edits (debounced), and flush whatever is pending on close.
  const latest = useRef({ title, description, preacher, selectedSource });
  latest.current = { title, description, preacher, selectedSource };
  const dirty = useRef(false);
  const firstRender = useRef(true);

  const flush = () => {
    if (!dirty.current) return Promise.resolve();
    dirty.current = false;
    const v = latest.current;
    return saveReview(
      video.id,
      { title: v.title, description: v.description, preacher: v.preacher },
      v.selectedSource
    )
      .then(() => setSaveError(null))
      .catch((err) => setSaveError(errorMessage(err, "Couldn't save your changes")));
  };

  useEffect(() => {
    if (firstRender.current) {
      firstRender.current = false;
      return;
    }
    dirty.current = true;
    const timer = setTimeout(flush, AUTOSAVE_MS);
    return () => clearTimeout(timer);
  }, [title, description, preacher, selectedSource]);

  useEffect(() => () => void flush(), []);

  // Load YouTube auth status
  useEffect(() => {
    youtubeGetAuthStatus().then(setYoutubeAuth).catch(console.error);
  }, []);

  // Listen for upload progress
  useEffect(() => {
    const unlisten = listen<{
      video_id: string;
      progress: number;
      status: string;
      url: string | null;
      error: string | null;
    }>("youtube-upload-progress", (event) => {
      const e = event.payload;
      if (e.video_id !== video.id) return;
      if (e.status === "done" && e.url) {
        setUploadedUrl(e.url);
        setUploadCount((n) => n + 1);
        setIsUploading(false);
      } else if (e.status === "error") {
        setUploadError(e.error || "Upload failed");
        setIsUploading(false);
      } else {
        setIsUploading(true);
        setUploadProgress((prev) => Math.max(prev, e.progress));
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Take the thumbnails as the library now has them.
  const reloadThumbnails = async (select?: string) => {
    const fresh = await getVideo(video.id);
    setThumbnailOptions(fresh.thumbnail_options);
    setRenders(rendersById(fresh.renders));
    setSelectedSource(select ?? fresh.selected_source ?? fresh.thumbnail_options[0]?.id ?? null);
  };

  const handleRegenerateThumbnails = async () => {
    try {
      setIsRegeneratingThumbnails(true);
      await regenerateThumbnails(video.id);
      await reloadThumbnails();
    } catch (err) {
      console.error("Failed to regenerate thumbnails:", err);
    } finally {
      setIsRegeneratingThumbnails(false);
    }
  };

  const handleThumbnailEditorSave = async (newPath: string) => {
    setIsEditorOpen(false);
    try {
      await reloadThumbnails(fileName(newPath));
    } catch (err) {
      setSaveError(errorMessage(err));
    }
  };

  const handleOpenFolder = async () => {
    try {
      await revealVideo(video.id);
    } catch (err) {
      setSaveError(errorMessage(err));
    }
  };

  const handleReprocess = async () => {
    const confirmed = await ask(
      "This re-transcribes the video and regenerates all suggestions and thumbnails. Your edits here are replaced. Continue?",
      { title: "Reprocess", kind: "warning", okLabel: "Reprocess", cancelLabel: "Cancel" }
    );
    if (!confirmed) return;

    try {
      setIsReprocessing(true);
      dirty.current = false;
      // Reset deletes the saved transcript, so processing transcribes again.
      await resetVideo(video.id);
      await processVideo(video.id);
      onBack();
    } catch (err) {
      console.error("Reprocessing failed:", err);
      alert(`Reprocessing failed: ${errorMessage(err)}`);
    } finally {
      setIsReprocessing(false);
    }
  };

  const handleUpload = async () => {
    if (uploadCount > 0) {
      const confirmed = await ask(
        "This video has already been uploaded to YouTube. Upload it again as a new YouTube video?",
        { title: "Upload again", kind: "warning", okLabel: "Upload Again", cancelLabel: "Cancel" }
      );
      if (!confirmed) return;
    }

    try {
      await flush();
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

      const source = selectedOption?.id;
      const url = await youtubeUploadVideo(
        video.id,
        title,
        fullDescription,
        source,
        source ? keys[source] : undefined,
        "public",
        startTime,   // trim start for caption adjustment
        endTime      // trim end for caption adjustment
      );
      // The "done" event also sets this (and counts the upload).
      setUploadedUrl(url);
      setIsUploading(false);
    } catch (err) {
      console.error("Upload failed:", err);
      setUploadError(errorMessage(err));
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
            <p className="text-sm text-muted-foreground">{video.channel || video.file_name}</p>
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
              disabled={isUploading || !preacher.trim()}
              title={!preacher.trim() ? "Speaker name is required" : undefined}
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
              <button
                onClick={() => openUrl(uploadedUrl)}
                className="flex items-center gap-2 px-4 py-2 bg-green-600 text-white rounded-lg hover:bg-green-700 transition-colors"
              >
                Open on YouTube
                <ExternalLink className="w-4 h-4" />
              </button>
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

      {saveError && (
        <div className="mx-4 mt-4 p-3 bg-destructive/10 text-destructive rounded-lg text-sm flex items-center gap-2">
          <AlertCircle className="w-4 h-4" />
          {saveError}
        </div>
      )}

      {/* Main Content */}
      <div className="flex-1 overflow-auto p-6">
        <div className="max-w-5xl mx-auto space-y-6">
          {/* Video Preview */}
          <div className="bg-card border border-border rounded-lg overflow-hidden">
            <div className="aspect-video bg-black flex items-center justify-center">
              {selectedOption ? (
                <LocalImage
                  path={shownPath(selectedOption)}
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

              <label className="flex items-center gap-2 text-sm mb-3 text-muted-foreground">
                <input
                  type="checkbox"
                  checked={singleLine}
                  onChange={(e) => setSingleLine(e.target.checked)}
                  className="rounded"
                />
                Title on a single line
              </label>

              {renderProgress && (
                <div className="mb-3 p-2 bg-muted rounded-lg flex items-center gap-2">
                  <Loader2 className="w-3 h-3 animate-spin" />
                  <span className="text-xs text-muted-foreground">{renderProgress}</span>
                </div>
              )}

              <div className="grid grid-cols-3 gap-2">
                {thumbnailOptions.map((option, index) => (
                  <button
                    key={option.id}
                    onClick={() => setSelectedSource(option.id)}
                    className={`relative aspect-video rounded-lg overflow-hidden border-2 transition-colors ${
                      selectedOption?.id === option.id
                        ? "border-primary ring-2 ring-primary/30"
                        : "border-transparent hover:border-muted-foreground/30"
                    }`}
                  >
                    <LocalImage
                      path={shownPath(option)}
                      alt={`Thumbnail option ${index + 1}`}
                      className="w-full h-full object-cover"
                    />
                    {selectedOption?.id === option.id && (
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

              {video.source_url && (
                <div>
                  <label className="block text-sm font-medium mb-1">
                    Original YouTube URL
                  </label>
                  <input
                    type="text"
                    value={video.source_url}
                    readOnly
                    className="w-full px-3 py-2 border border-input rounded-lg bg-muted text-muted-foreground"
                  />
                </div>
              )}

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
              Show in Finder
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
          thumbnailOptions={thumbnailOptions.map((o) => o.path)}
          selectedThumbnail={selectedOption?.path ?? null}
          onSave={handleThumbnailEditorSave}
          onClose={() => setIsEditorOpen(false)}
        />
      )}
    </div>
  );
}
