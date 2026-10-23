import { useState, useEffect, useMemo, useRef } from "react";
import {
  regenerateThumbnails,
  getVideo,
  revealVideo,
  youtubeGetAuthStatus,
  youtubeListPlaylists,
  getSettings,
  errorMessage,
  reprocessVideo,
} from "@/lib/tauri";
import { renderThumbnailBatch } from "@/lib/thumbnailRenderer";
import { getDefaultTemplate } from "@/templates";
import { splitTitleIntoLines } from "@/lib/utils";
import { renderKey } from "./review/renderKey";
import { useRenderSync } from "@/hooks/useRenderSync";
import { useReviewAutosave } from "@/hooks/useReviewAutosave";
import { useUpload } from "@/hooks/useUpload";
import { buildYoutubeDescription, youtubeChapters, youtubeTitle } from "@/lib/youtube";
import { canStartUpload, reviewShortcut } from "./review/reviewMachine";
import { ask } from "@tauri-apps/plugin-dialog";
import type {
  VideoDetail,
  YouTubeAuthStatus,
  Chapter,
  ThumbnailOption,
  PlaylistInfo,
  UploadPrivacy,
} from "@/lib/types";
import { LocalImage } from "./LocalImage";
import { ThumbnailEditor } from "./ThumbnailEditor";
import { ReviewHeader } from "./review/ReviewHeader";
import { ReviewBanners } from "./review/ReviewBanners";
import { ThumbnailGrid } from "./review/ThumbnailGrid";
import { TrimPanel } from "./TrimPanel";
import {
  formatTimeDisplay,
} from "@/lib/clips";
import {
  FolderOpen,
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
  const chapters: Chapter[] = useMemo(() => video.transcription?.suggested_chapters || [], [video]);
  const [preacher, setPreacher] = useState(video.review.preacher ?? "");
  // Speakers named before, offered as suggestions; a video with no speaker
  // saved yet starts with the most recent one.
  const [recentPreachers, setRecentPreachers] = useState<string[]>([]);
  const preacherTouched = useRef(false);
  // The speaker was filled in from the last video, not by the user.
  const [preacherPrefilled, setPreacherPrefilled] = useState(false);

  // Trim boundaries: where the trimmed video really starts and ends in the
  // original, which chapters and captions are timed from. The trim panel
  // changes them.
  const [trim, setTrimPoints] = useState(() => ({
    start: video.trim_start ?? video.transcription?.sermon_start ?? 0,
    end: video.trim_end ?? video.transcription?.sermon_end ?? video.duration ?? 0,
  }));
  const startTime = trim.start;
  const endTime = trim.end;
  // The AI couldn't find the sermon: ask for the points until they're set.
  const [trimmedHere, setTrimmedHere] = useState(false);
  const trimPanel = useRef<HTMLDivElement>(null);
  // What YouTube gets: chapters of the trimmed video, within its limits.
  const shownChapters = useMemo(() => youtubeChapters(chapters, startTime, endTime), [chapters, startTime, endTime]);
  const youtubeDescription = useMemo(
    () => buildYoutubeDescription({ description, chapters, startTime, endTime, footer: SOCIAL_LINKS }),
    [description, chapters, startTime, endTime]
  );

  // Thumbnails: sources by id, plus a stored template render per source.
  const [thumbnailOptions, setThumbnailOptions] = useState<ThumbnailOption[]>(video.thumbnail_options);
  const [selectedSource, setSelectedSource] = useState<string | null>(
    video.selected_source ?? video.thumbnail_options[0]?.id ?? null
  );
  const [isRegeneratingThumbnails, setIsRegeneratingThumbnails] = useState(false);

  // The template draws title/speaker/logo over each source; the result is
  // saved into the video's library folder. A render is current when its key
  // matches what it would be rendered with now; only stale ones are redone.
  const [singleLine, setSingleLine] = useState(video.review.single_line ?? false);
  const titleLines = useMemo(
    () => (singleLine ? [title.trim()] : splitTitleIntoLines(title)).filter((l) => l),
    [title, singleLine]
  );
  // The logo chosen in Settings (null: the built-in mark); undefined until
  // read, and nothing is rendered before then.
  const [logo, setLogo] = useState<string | null | undefined>(undefined);
  const canRender = titleLines.length > 0 && preacher.trim() !== "" && logo !== undefined;
  const keys = useMemo(() => {
    const inputs = { title, preacher, singleLine, logo };
    return Object.fromEntries(thumbnailOptions.map((o) => [o.id, renderKey(o, inputs)]));
  }, [thumbnailOptions, title, preacher, singleLine, logo]);
  // A render that failed doesn't block uploading: the plain image is used.
  const {
    renders,
    replaceRenders,
    stale: staleOptions,
    progress: renderProgress,
    error: renderError,
    retry: retryRenders,
  } = useRenderSync({
    videoId: video.id,
    options: thumbnailOptions,
    keys,
    enabled: canRender,
    initial: video.renders,
    render: async (option) => {
      const [result] = await renderThumbnailBatch(getDefaultTemplate(), [option.render_from], titleLines, preacher, {
        logoPath: logo,
      });
      return result.imageData;
    },
    // The backend checks each render's key against the saved title, speaker
    // and layout, so those are saved first.
    beforeSave: () => saveRenderInputs(),
  });

  const thumbnailsReady = staleOptions.length === 0 || renderError !== null;
  const shownPath = (o: ThumbnailOption) => renders[o.id]?.path ?? o.path;
  const selectedOption = thumbnailOptions.find((o) => o.id === selectedSource) ?? thumbnailOptions[0] ?? null;

  const [isEditorOpen, setIsEditorOpen] = useState(false);

  const [youtubeAuth, setYoutubeAuth] = useState<YouTubeAuthStatus | null>(null);
  // What stops an upload, said next to the button.
  const uploadBlocker =
    youtubeTitle(title) === ""
      ? "Add a title to upload."
      : preacher.trim() === ""
        ? "Add the speaker's name to upload."
        : null;
  const uploadReady = thumbnailsReady && uploadBlocker === null;

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

  const { flush, saveRenderInputs, lastSaveError, savesInFlight, savedOnce } = useReviewAutosave({
    video,
    title,
    description,
    preacher,
    singleLine,
    selectedSource,
    setSaveError,
  });

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
        setLogo(s.logo_path ?? null);
        const recent = s.recent_preachers ?? [];
        setRecentPreachers(recent);
        if (video.review.preacher == null && !preacherTouched.current && recent[0]) {
          setPreacher(recent[0]);
          setPreacherPrefilled(true);
        }
        if (choicesTouched.current) return;
        setPrivacy(s.upload_privacy);
        setPlaylistId(s.youtube_playlist_id ?? "");
      })
      .catch((err) => {
        console.error(err);
        setLogo(null);
      });
  }, []);

  // Back on this screen (e.g. from Settings): the logo may have changed.
  const wasActive = useRef(active);
  useEffect(() => {
    if (active && !wasActive.current) {
      getSettings()
        .then((s) => setLogo(s.logo_path ?? null))
        .catch(console.error);
    }
    wasActive.current = active;
  }, [active]);

  const {
    review,
    dispatch,
    phase,
    handleUpload,
    canForgetEarlier,
    forgetError,
    setForgetError,
    handleForgetEarlier,
    updatingMetadata,
    metadataNotice,
    handleUpdateMetadata,
    handleCancelUpload,
  } = useUpload({
    video,
    title,
    preacher,
    privacy,
    playlistId,
    youtubeDescription,
    keys,
    selectedOption,
    startTime,
    endTime,
    uploadReady,
    flush,
    setSaveError,
  });

  // Take the thumbnails as the library now has them.
  const reloadThumbnails = async (select?: string) => {
    const fresh = await getVideo(video.id);
    setThumbnailOptions(fresh.thumbnail_options);
    replaceRenders(fresh.renders);
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
      "This transcribes the video again and makes new suggestions, a new cut and new thumbnails. Your trim and thumbnail choices are replaced.\n\nThe title, description and speaker you typed are kept. Continue?",
      { title: "Reprocess", kind: "warning", okLabel: "Reprocess", cancelLabel: "Cancel" }
    );
    if (!confirmed) return;

    try {
      setIsReprocessing(true);
      setActionError(null);
      // What was typed is kept, so it's saved first.
      if (!(await flush())) throw new Error(lastSaveError.current ?? "Couldn't save your changes");
      // The old transcript goes, so processing transcribes again.
      await reprocessVideo(video.id);
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

  // ⌘Enter uploads, Esc goes back or leaves a field (see reviewShortcut).
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
        inField: isEditable(e.target),
        // keyCode 229: Safari's key that an input method handles.
        composing: e.isComposing || e.keyCode === 229,
        blocked: s.blocked,
      });
      if (action === "blur") {
        (e.target as HTMLElement).blur();
      } else if (action === "back") {
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

  const formatTime = formatTimeDisplay;

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <ReviewHeader
        video={video}
        handleBack={handleBack}
        handleReprocess={handleReprocess}
        isReprocessing={isReprocessing}
        phase={phase}
        review={review}
        youtubeAuth={youtubeAuth}
        privacy={privacy}
        setPrivacy={setPrivacy}
        playlistId={playlistId}
        setPlaylistId={setPlaylistId}
        playlists={playlists}
        choicesTouched={choicesTouched}
        handleUpload={handleUpload}
        uploadReady={uploadReady}
        uploadBlocker={uploadBlocker}
        thumbnailsReady={thumbnailsReady}
        handleCancelUpload={handleCancelUpload}
      />

      <ReviewBanners
        video={video}
        phase={phase}
        dispatch={dispatch}
        uploadReady={uploadReady}
        handleUpload={handleUpload}
        updatingMetadata={updatingMetadata}
        metadataNotice={metadataNotice}
        handleUpdateMetadata={handleUpdateMetadata}
        canForgetEarlier={canForgetEarlier}
        handleForgetEarlier={handleForgetEarlier}
        trimmedHere={trimmedHere}
        trimPanel={trimPanel}
        saveError={saveError}
        forgetError={forgetError}
        setForgetError={setForgetError}
        actionError={actionError}
        setActionError={setActionError}
        youtubeError={youtubeError}
        loadYoutube={loadYoutube}
        renderError={renderError}
        renderProgress={renderProgress}
        retryRenders={retryRenders}
      />

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
            <ThumbnailGrid
              thumbnailOptions={thumbnailOptions}
              selectedOption={selectedOption}
              setSelectedSource={setSelectedSource}
              shownPath={shownPath}
              flush={flush}
              setIsEditorOpen={setIsEditorOpen}
              handleRegenerateThumbnails={handleRegenerateThumbnails}
              isRegeneratingThumbnails={isRegeneratingThumbnails}
              phase={phase}
              singleLine={singleLine}
              setSingleLine={setSingleLine}
              renderProgress={renderProgress}
            />

            {/* Metadata */}
            <div className="bg-card border border-border rounded-lg p-4 space-y-4">
              <div className="flex items-baseline justify-between">
                <h3 className="font-semibold">Metadata</h3>
                <span className="text-xs text-muted-foreground" aria-live="polite">
                  {savesInFlight > 0 ? "Saving…" : savedOnce && !saveError ? "Saved" : ""}
                </span>
              </div>

              <div>
                <label htmlFor="review-title" className="block text-sm font-medium mb-1">Title</label>
                <input
                  id="review-title"
                  type="text"
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                  className="w-full px-3 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring"
                />
              </div>

              <div>
                <label htmlFor="review-preacher" className="block text-sm font-medium mb-1">
                  Preacher / Speaker <span className="text-red-500">*</span>
                </label>
                <input
                  id="review-preacher"
                  type="text"
                  aria-label="Preacher"
                  value={preacher}
                  list="recent-preachers"
                  onChange={(e) => {
                    preacherTouched.current = true;
                    setPreacherPrefilled(false);
                    setPreacher(e.target.value);
                  }}
                  placeholder="Enter speaker name"
                  className={`w-full px-3 py-2 border rounded-lg focus:outline-none focus:ring-2 focus:ring-ring ${
                    !preacher.trim() ? "border-red-300" : "border-input"
                  }`}
                />
                {preacherPrefilled && (
                  <p className="mt-1 text-xs text-muted-foreground">(from last time — change if needed)</p>
                )}
                <datalist id="recent-preachers">
                  {recentPreachers.map((name) => (
                    <option key={name} value={name} />
                  ))}
                </datalist>
              </div>

              {video.source_url && (
                <div>
                  <label htmlFor="review-source-url" className="block text-sm font-medium mb-1">
                    Original YouTube URL
                  </label>
                  <input
                    id="review-source-url"
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

              {shownChapters.length > 0 ? (
                <div>
                  <label className="block text-sm font-medium mb-1">
                    Chapters
                  </label>
                  <div className="text-sm text-muted-foreground space-y-1 bg-muted p-2 rounded-lg max-h-32 overflow-y-auto">
                    {shownChapters.map((ch, index) => (
                      <div key={index} className="flex gap-2">
                        <span className="font-mono">{formatTime(ch.time)}</span>
                        <span>{ch.title}</span>
                      </div>
                    ))}
                  </div>
                </div>
              ) : chapters.length > 0 ? (
                <p className="text-xs text-muted-foreground">
                  No chapters: YouTube needs at least 3, each at least 10 seconds long.
                </p>
              ) : null}

              <details className="text-sm">
                <summary className="cursor-pointer font-medium">Preview YouTube description</summary>
                <pre className="mt-2 p-2 bg-muted rounded-lg whitespace-pre-wrap break-words font-sans text-xs text-muted-foreground max-h-64 overflow-y-auto">
                  {youtubeDescription}
                </pre>
              </details>

            </div>
          </div>

          <div ref={trimPanel}>
          <TrimPanel
            videoId={video.id}
            sourcePath={video.file_path}
            duration={video.duration}
            start={startTime}
            end={endTime}
            segments={video.transcription?.segments ?? []}
            disabled={phase.kind === "uploading"}
            onTrimmed={(start, end) => {
              setTrimPoints({ start, end });
              setTrimmedHere(true);
            }}
          />
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


/** A text field, select or other element that takes typing. */
function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement ||
    target.isContentEditable ||
    target.closest('[contenteditable=""], [contenteditable="true"], [contenteditable="plaintext-only"]') !== null
  );
}
