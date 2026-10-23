import { useState, useEffect, useMemo, useReducer, useRef } from "react";
import {
  regenerateThumbnails,
  getVideo,
  revealVideo,
  youtubeGetAuthStatus,
  youtubeListPlaylists,
  getSettings,
  youtubeUploadVideo,
  processVideo,
  resetVideo,
  saveReview,
  saveRenderedThumbnail,
  cancelUpload,
  errorMessage,
  UPLOAD_CANCELLED,
} from "@/lib/tauri";
import { UploadWarnings } from "./UploadWarnings";
import { renderThumbnailBatch } from "@/lib/thumbnailRenderer";
import { getDefaultTemplate } from "@/templates";
import { splitTitleIntoLines } from "@/lib/utils";
import { renderKey } from "./review/renderKey";
import { canStartUpload, initialReviewState, reviewReducer, reviewShortcut } from "./review/reviewMachine";
import { listen } from "@tauri-apps/api/event";
import { ask } from "@tauri-apps/plugin-dialog";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import type {
  VideoDetail,
  YouTubeAuthStatus,
  Chapter,
  ThumbnailOption,
  ThumbnailRender,
  PlaylistInfo,
  UploadPrivacy,
  UploadWarning,
} from "@/lib/types";
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
  /** Whether the screen is showing (it stays mounted while hidden); its
   * keyboard shortcuts only work then. */
  active?: boolean;
}

const AUTOSAVE_MS = 1000;

const PRIVACY_LABELS: Record<UploadPrivacy, string> = {
  public: "Public",
  unlisted: "Unlisted",
  private: "Private",
};
const RENDER_DEBOUNCE_MS = 1500;

type RenderMap = Record<string, { path: string; key: string }>;

function rendersById(renders: ThumbnailRender[]): RenderMap {
  return Object.fromEntries(renders.map((r) => [r.source, { path: r.path, key: r.render_key }]));
}

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

export function ReviewScreen({ video, onBack, active = true }: ReviewScreenProps) {
  // Metadata editing: saved edits first, then the AI's suggestions
  const [title, setTitle] = useState(
    video.review.title ?? video.transcription?.suggested_title ?? video.title
  );
  const [description, setDescription] = useState(
    video.review.description ?? video.transcription?.suggested_description ?? ""
  );
  const chapters: Chapter[] = video.transcription?.suggested_chapters || [];
  const [preacher, setPreacher] = useState(video.review.preacher ?? "");
  // Speakers named before, offered as suggestions; a video with no speaker
  // saved yet starts with the most recent one.
  const [recentPreachers, setRecentPreachers] = useState<string[]>([]);
  const preacherTouched = useRef(false);

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
  // A render that failed doesn't block uploading: the plain image is used.
  const [renderError, setRenderError] = useState<string | null>(null);
  // Bumped by "Try again" to render the stale thumbnails again.
  const [renderAttempt, setRenderAttempt] = useState(0);
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
        setRenderError(null);
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
        if (!cancelled) setRenderError(errorMessage(err, "Couldn't update the thumbnails"));
      } finally {
        if (!cancelled) setRenderProgress("");
      }
    }, delay);

    return () => {
      cancelled = true;
      clearTimeout(timeout);
    };
    // staleIds + keys capture everything a render depends on.
  }, [staleIds, keys, video.id, renderAttempt]);

  const thumbnailsReady = staleOptions.length === 0 || renderError !== null;
  const shownPath = (o: ThumbnailOption) => renders[o.id]?.path ?? o.path;
  const selectedOption = thumbnailOptions.find((o) => o.id === selectedSource) ?? thumbnailOptions[0] ?? null;

  const [isEditorOpen, setIsEditorOpen] = useState(false);

  // Upload flow (see reviewMachine). The upload runs in the background; if
  // this screen was reopened while one is going, it picks it up from the
  // library's state.
  const [youtubeAuth, setYoutubeAuth] = useState<YouTubeAuthStatus | null>(null);
  const [review, dispatch] = useReducer(reviewReducer, video, (v) =>
    initialReviewState({ status: v.status, uploadProgress: v.upload_progress, uploads: v.uploads })
  );
  const phase = review.phase;
  const uploadReady = thumbnailsReady && preacher.trim() !== "";

  // Privacy and playlist for this upload, starting from the last privacy
  // used and the default playlist in Settings. "" means no playlist.
  const [privacy, setPrivacy] = useState<UploadPrivacy>("public");
  const [playlistId, setPlaylistId] = useState("");
  const [playlists, setPlaylists] = useState<PlaylistInfo[]>([]);
  const choicesTouched = useRef(false);

  // Reprocessing state
  const [isReprocessing, setIsReprocessing] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  // Regenerate or Reprocess failed.
  const [actionError, setActionError] = useState<string | null>(null);
  // The YouTube sign-in or playlists couldn't be loaded.
  const [youtubeError, setYoutubeError] = useState<string | null>(null);

  // Autosave edits (debounced), and flush whatever is pending on close.
  const latest = useRef({ title, description, preacher, selectedSource });
  latest.current = { title, description, preacher, selectedSource };
  const dirty = useRef(false);
  const firstRender = useRef(true);
  // The save in flight, if any; flushing waits for it, so leaving never
  // races a save that already started.
  const saving = useRef<Promise<boolean>>(Promise.resolve(true));

  /** Save pending edits; resolves with whether everything is saved. */
  const flush = (): Promise<boolean> => {
    const run = saving.current.then((): boolean | Promise<boolean> => {
      if (!dirty.current) return true;
      dirty.current = false;
      const v = latest.current;
      return saveReview(
        video.id,
        { title: v.title, description: v.description, preacher: v.preacher },
        v.selectedSource
      )
        .then(() => {
          setSaveError(null);
          return true;
        })
        .catch((err) => {
          // Still unsaved: the next flush (e.g. Back) tries again.
          dirty.current = true;
          setSaveError(errorMessage(err, "Couldn't save your changes"));
          return false;
        });
    });
    saving.current = run;
    return run;
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

  // Load YouTube auth status, the upload defaults and the playlists
  const loadYoutube = () => {
    setYoutubeError(null);
    youtubeGetAuthStatus()
      .then((status) => {
        setYoutubeAuth(status);
        if (status.is_authenticated) {
          youtubeListPlaylists()
            .then(setPlaylists)
            .catch((err) => setYoutubeError(`Couldn't load your playlists: ${errorMessage(err)}`));
        }
      })
      .catch((err) => setYoutubeError(`Couldn't check the YouTube sign-in: ${errorMessage(err)}`));
  };
  useEffect(() => {
    loadYoutube();
    getSettings()
      .then((s) => {
        const recent = s.recent_preachers ?? [];
        setRecentPreachers(recent);
        if (video.review.preacher == null && !preacherTouched.current && recent[0]) {
          setPreacher(recent[0]);
        }
        if (choicesTouched.current) return;
        setPrivacy(s.upload_privacy);
        setPlaylistId(s.youtube_playlist_id ?? "");
      })
      .catch(console.error);
  }, []);

  // Listen for upload progress
  useEffect(() => {
    const unlisten = listen<{
      video_id: string;
      progress: number;
      status: string;
      url: string | null;
      error: string | null;
      warnings?: UploadWarning[] | null;
    }>("youtube-upload-progress", (event) => {
      const e = event.payload;
      if (e.video_id !== video.id) return;
      if (e.status === "done" && e.url) {
        dispatch({ type: "succeeded", url: e.url, warnings: e.warnings ?? [] });
      } else if (e.status === "cancelled") {
        dispatch({ type: "cancelled" });
      } else if (e.status === "error") {
        dispatch({ type: "failed", message: e.error || "Upload failed" });
      } else {
        dispatch({ type: "progress", percent: e.progress });
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
      setActionError(null);
      await regenerateThumbnails(video.id);
      await reloadThumbnails();
    } catch (err) {
      setActionError(`Couldn't regenerate the thumbnails: ${errorMessage(err)}`);
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
      setActionError(null);
      dirty.current = false;
      // Reset deletes the saved transcript, so processing transcribes again.
      await resetVideo(video.id);
      await processVideo(video.id);
      onBack();
    } catch (err) {
      setActionError(`Couldn't reprocess: ${errorMessage(err)}`);
    } finally {
      setIsReprocessing(false);
    }
  };

  // Leave only once the edits are saved; if saving fails, stay so the
  // error shows (Back again retries).
  const handleBack = async () => {
    if (await flush()) onBack();
  };

  // The upload is confirmed when the machine says so, once, naming what
  // goes up and how.
  useEffect(() => {
    if (phase.kind !== "confirming") return;
    let current = true;
    const again = review.uploadCount > 0;
    const lines = [
      `Upload "${title.trim()}" by ${preacher.trim()} as ${PRIVACY_LABELS[privacy]}?`,
    ];
    if (again) {
      lines.push("This video has already been uploaded to YouTube. This makes a new, separate YouTube video.");
    }
    ask(lines.join("\n\n"), {
      title: again ? "Upload again" : "Upload to YouTube",
      kind: again ? "warning" : "info",
      okLabel: again ? "Upload Again" : "Upload",
      cancelLabel: "Cancel",
    })
      .then((yes) => current && dispatch({ type: yes ? "confirm" : "cancelConfirm" }))
      .catch(() => current && dispatch({ type: "cancelConfirm" }));
    return () => {
      current = false;
    };
  }, [phase.kind]);

  // Each upload the machine starts is sent exactly once: save the edits,
  // then hand it to the backend.
  const uploadRequest = phase.kind === "uploading" ? phase.request : null;
  const startedRequest = useRef<number | null>(null);
  useEffect(() => {
    if (uploadRequest === null || startedRequest.current === uploadRequest) return;
    startedRequest.current = uploadRequest;

    (async () => {
      try {
        await flush();

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
        const outcome = await youtubeUploadVideo(
          video.id,
          title,
          fullDescription,
          source,
          source ? keys[source] : undefined,
          privacy,
          startTime,   // trim start for caption adjustment
          endTime,     // trim end for caption adjustment
          playlistId
        );
        // The "done" event reports this too; the machine counts it once.
        dispatch({ type: "succeeded", url: outcome.url, warnings: outcome.warnings });
      } catch (err) {
        const message = errorMessage(err);
        if (message === UPLOAD_CANCELLED) {
          dispatch({ type: "cancelled" });
          return;
        }
        console.error("Upload failed:", err);
        dispatch({ type: "failed", message });
      }
    })();
  }, [uploadRequest]);

  const handleUpload = () => dispatch({ type: "upload", ready: uploadReady });

  // ⌘Enter uploads, Esc goes back (see reviewShortcut).
  const shortcutState = useRef({ blocked: false, canUpload: false, handleBack, handleUpload });
  shortcutState.current = {
    blocked: isEditorOpen || phase.kind === "confirming",
    canUpload: !!youtubeAuth?.is_authenticated && canStartUpload(phase),
    handleBack,
    handleUpload,
  };
  useEffect(() => {
    if (!active) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.repeat) return;
      const s = shortcutState.current;
      const action = reviewShortcut({
        key: e.key,
        metaKey: e.metaKey,
        ctrlKey: e.ctrlKey,
        inTextarea: e.target instanceof HTMLTextAreaElement,
        blocked: s.blocked,
      });
      if (action === "back") {
        e.preventDefault();
        void s.handleBack();
      } else if (action === "upload" && s.canUpload) {
        e.preventDefault();
        s.handleUpload();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [active]);

  // The upload stops; its session is kept, so Upload continues it.
  const handleCancelUpload = () => {
    dispatch({ type: "cancelRequested" });
    cancelUpload(video.id).catch((err) => setSaveError(errorMessage(err, "Couldn't cancel the upload")));
  };

  const formatTime = formatTimeDisplay;

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="p-4 border-b border-border flex items-center justify-between">
        <div className="flex items-center gap-4">
          <button
            onClick={handleBack}
            aria-label="Back"
            title="Back (Esc)"
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
          {youtubeAuth?.is_authenticated && phase.kind !== "done" && (
            <>
              <select
                aria-label="Privacy"
                value={privacy}
                disabled={!canStartUpload(phase)}
                onChange={(e) => {
                  choicesTouched.current = true;
                  setPrivacy(e.target.value as UploadPrivacy);
                }}
                className="px-3 py-2 border border-input rounded-lg bg-background text-sm disabled:opacity-50"
              >
                <option value="public">Public</option>
                <option value="unlisted">Unlisted</option>
                <option value="private">Private</option>
              </select>
              <select
                aria-label="Playlist"
                value={playlistId}
                disabled={!canStartUpload(phase)}
                onChange={(e) => {
                  choicesTouched.current = true;
                  setPlaylistId(e.target.value);
                }}
                className="px-3 py-2 border border-input rounded-lg bg-background text-sm max-w-48 disabled:opacity-50"
              >
                <option value="">No playlist</option>
                {playlistId && !playlists.some((p) => p.id === playlistId) && (
                  <option value={playlistId}>Default playlist</option>
                )}
                {playlists.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.title}
                  </option>
                ))}
              </select>
              <button
                onClick={handleUpload}
                disabled={!canStartUpload(phase) || !uploadReady}
                title={!preacher.trim() ? "Speaker name is required" : "Upload (⌘Enter)"}
                className="px-4 py-2 bg-red-600 text-white rounded-lg hover:bg-red-700 disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 transition-colors"
              >
                {phase.kind === "uploading" ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    {phase.progress}%
                </>
              ) : !thumbnailsReady ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Updating thumbnail…
                </>
              ) : (
                <>
                  <Youtube className="w-4 h-4" />
                  Upload
                </>
              )}
            </button>
            {phase.kind === "uploading" && (
              <button
                onClick={handleCancelUpload}
                disabled={review.cancelling}
                className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
              >
                {review.cancelling ? "Cancelling…" : "Cancel upload"}
              </button>
            )}
            </>
          )}
        </div>
      </div>

      {/* Upload Success Banner */}
      {phase.kind === "done" && (
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
                onClick={() => dispatch({ type: "uploadAgain" })}
                className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
              >
                Upload Again
              </button>
              <button
                onClick={() => openUrl(phase.url)}
                className="flex items-center gap-2 px-4 py-2 bg-green-600 text-white rounded-lg hover:bg-green-700 transition-colors"
              >
                Open on YouTube
                <ExternalLink className="w-4 h-4" />
              </button>
            </div>
          </div>
          {phase.warnings.length > 0 && (
            <div className="mt-3">
              <UploadWarnings
                videoId={video.id}
                warnings={phase.warnings}
                onChange={(warnings) => dispatch({ type: "warnings", warnings })}
              />
            </div>
          )}
        </div>
      )}

      {phase.kind === "cancelled" && (
        <div className="mx-4 mt-4 p-3 bg-muted rounded-lg text-sm text-muted-foreground">
          Upload cancelled. Upload again to continue where it stopped.
        </div>
      )}

      {/* Upload Error Banner */}
      {phase.kind === "error" && (
        <div className="mx-4 mt-4 p-4 bg-destructive/10 rounded-lg flex items-center justify-between">
          <div className="flex items-center gap-3">
            <AlertCircle className="w-6 h-6 text-destructive" />
            <span className="text-destructive">{phase.message}</span>
          </div>
          <button
            onClick={handleUpload}
            disabled={!uploadReady}
            className="px-4 py-2 bg-red-600 text-white rounded-lg hover:bg-red-700 disabled:opacity-50 transition-colors"
          >
            Retry
          </button>
        </div>
      )}

      {saveError && <ErrorBanner message={saveError} />}
      {actionError && <ErrorBanner message={actionError} onDismiss={() => setActionError(null)} />}
      {youtubeError && <ErrorBanner message={youtubeError} onRetry={loadYoutube} />}
      {renderError && !renderProgress && (
        <ErrorBanner
          message={`Couldn't update the thumbnails: ${renderError}. Uploading uses the plain image.`}
          onRetry={() => {
            setRenderError(null);
            setRenderAttempt((n) => n + 1);
          }}
        />
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
                    onClick={() => {
                      // Saving a custom thumbnail prunes old ones but keeps the
                      // selected one, so the library should know which that is.
                      void flush();
                      setIsEditorOpen(true);
                    }}
                    disabled={thumbnailOptions.length === 0}
                    className="text-sm px-3 py-1.5 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 flex items-center gap-1.5 transition-colors"
                  >
                    <Pencil className="w-3 h-3" />
                    Edit
                  </button>
                  <button
                    onClick={handleRegenerateThumbnails}
                    disabled={isRegeneratingThumbnails || phase.kind === "uploading"}
                    title={phase.kind === "uploading" ? "Wait for the upload to finish" : undefined}
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
                  aria-label="Preacher"
                  value={preacher}
                  list="recent-preachers"
                  onChange={(e) => {
                    preacherTouched.current = true;
                    setPreacher(e.target.value);
                  }}
                  placeholder="Enter speaker name"
                  className={`w-full px-3 py-2 border rounded-lg focus:outline-none focus:ring-2 focus:ring-ring ${
                    !preacher.trim() ? "border-red-300" : "border-input"
                  }`}
                />
                <datalist id="recent-preachers">
                  {recentPreachers.map((name) => (
                    <option key={name} value={name} />
                  ))}
                </datalist>
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
                <label htmlFor="review-description" className="block text-sm font-medium mb-1">
                  Description
                </label>
                <textarea
                  id="review-description"
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
          thumbnailOptions={thumbnailOptions}
          selectedSource={selectedOption?.id ?? null}
          onSave={handleThumbnailEditorSave}
          onClose={() => setIsEditorOpen(false)}
        />
      )}
    </div>
  );
}

function ErrorBanner({
  message,
  onRetry,
  onDismiss,
}: {
  message: string;
  onRetry?: () => void;
  onDismiss?: () => void;
}) {
  return (
    <div role="alert" className="mx-4 mt-4 p-3 bg-destructive/10 text-destructive rounded-lg text-sm flex items-center gap-2">
      <AlertCircle className="w-4 h-4 shrink-0" />
      <span className="flex-1">{message}</span>
      {onRetry && (
        <button onClick={onRetry} className="px-3 py-1 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors">
          Try again
        </button>
      )}
      {onDismiss && (
        <button onClick={onDismiss} className="px-3 py-1 hover:bg-destructive/10 rounded-lg transition-colors">
          Dismiss
        </button>
      )}
    </div>
  );
}
