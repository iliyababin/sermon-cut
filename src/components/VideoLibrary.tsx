import { useCallback, useEffect, useRef, useState } from "react";
import { ErrorBanner } from "./ErrorBanner";
import {
  getLibrary,
  rescanLibrary,
  processVideo,
  cancelProcessing,
  resetVideo,
  hideVideo,
  trashVideo,
  forgetVideo,
  revealVideo,
  revealCommitBackup,
  isStuckCommit,
  openLibraryFolder,
  dismissNotices,
  takeOverLibrary,
  cancelDownload,
  cancelUpload,
  errorMessage,
} from "@/lib/tauri";
import { UploadWarnings } from "./UploadWarnings";
import { ask } from "@tauri-apps/plugin-dialog";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import { listen } from "@tauri-apps/api/event";
import type { LibraryVideo, LibraryView, LibraryStatus, ProcessingStage, DownloadItem } from "@/lib/types";
import { LocalImage } from "./LocalImage";
import { duplicateFileNames, formatFileSize } from "@/lib/utils";
import {
  Film,
  Clock,
  CheckCircle,
  Loader2,
  AlertCircle,
  Eye,
  EyeOff,
  FolderOpen,
  Play,
  X,
  RotateCcw,
  MoreHorizontal,
  Trash2,
  Search,
  HardDrive,
  Lock,
  Info,
  ExternalLink,
  Download,
} from "lucide-react";

interface DownloadProgress {
  video_id: string;
  progress: number;
  speed: string | null;
  eta: string | null;
  status: string;
}

interface UploadProgress {
  video_id: string;
  progress: number;
  status: string;
}

interface ProcessingProgress {
  video_id: string;
  stage: ProcessingStage;
  progress: number;
  message: string;
}

const RESCAN_MS = 5000;

interface VideoLibraryProps {
  onVideoSelect: (video: LibraryVideo) => void;
  visible?: boolean;
}

export function VideoLibrary({ onVideoSelect, visible = true }: VideoLibraryProps) {
  const [library, setLibrary] = useState<LibraryView | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [showHidden, setShowHidden] = useState(false);
  const [openMenu, setOpenMenu] = useState<string | null>(null);
  const [progress, setProgress] = useState<Record<string, ProcessingProgress>>({});
  const [downloads, setDownloads] = useState<Record<string, DownloadProgress>>({});
  const [uploads, setUploads] = useState<Record<string, number>>({});
  const scanning = useRef(false);

  // Requests are numbered; an answer older than one already shown is
  // dropped, so a slow scan can't put back a library that has moved on.
  const requests = useRef(0);
  const shown = useRef(0);
  const load = useCallback(async (fetch: () => Promise<LibraryView>, failure: string) => {
    const request = ++requests.current;
    try {
      const view = await fetch();
      if (request < shown.current) return;
      shown.current = request;
      setLibrary(view);
      setLoadError(null);
    } catch (err) {
      if (request < shown.current) return;
      setLoadError(errorMessage(err, failure));
    }
  }, []);

  const refresh = useCallback(() => load(getLibrary, "Failed to load the library"), [load]);

  const rescan = useCallback(async () => {
    if (scanning.current) return;
    scanning.current = true;
    try {
      await load(rescanLibrary, "Failed to read the library folder");
    } finally {
      scanning.current = false;
    }
  }, [load]);

  // Look at the folder when shown, every few seconds while shown, and
  // whenever the window comes back to the front (e.g. after using Finder).
  useEffect(() => {
    if (!visible) return;
    rescan();
    const timer = setInterval(rescan, RESCAN_MS);
    const onFocus = () => rescan();
    window.addEventListener("focus", onFocus);
    return () => {
      clearInterval(timer);
      window.removeEventListener("focus", onFocus);
    };
  }, [visible, rescan]);

  useEffect(() => {
    const unlistenChanged = listen("library-changed", () => refresh());
    const unlistenProcessing = listen<ProcessingProgress>("processing-progress", (event) => {
      setProgress((prev) => ({ ...prev, [event.payload.video_id]: event.payload }));
      if (event.payload.stage === "complete" || event.payload.stage === "error") {
        refresh();
      }
    });
    const unlistenDownload = listen<DownloadProgress>("download-progress", (event) => {
      setDownloads((prev) => ({ ...prev, [event.payload.video_id]: event.payload }));
    });
    const unlistenUpload = listen<UploadProgress>("youtube-upload-progress", (event) => {
      const { video_id, progress, status } = event.payload;
      if (status === "done" || status === "error" || status === "cancelled") {
        // Finished: the card shows the stored result from here on.
        setUploads(({ [video_id]: _, ...rest }) => rest);
        refresh();
      } else {
        setUploads((prev) => ({ ...prev, [video_id]: progress }));
      }
    });
    return () => {
      unlistenChanged.then((fn) => fn());
      unlistenProcessing.then((fn) => fn());
      unlistenDownload.then((fn) => fn());
      unlistenUpload.then((fn) => fn());
    };
  }, [refresh]);

  // Close the actions menu on any outside click, or Esc.
  useEffect(() => {
    if (!openMenu) return;
    const close = () => setOpenMenu(null);
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("click", close);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [openMenu]);

  async function run(action: () => Promise<unknown>) {
    try {
      setActionError(null);
      await action();
    } catch (err) {
      setActionError(errorMessage(err));
    } finally {
      refresh();
    }
  }

  async function handleTrash(video: LibraryVideo) {
    const confirmed = await ask(
      `Move "${video.file_name}" to the Trash?\n\nSermon Cut's transcript, trimmed video and thumbnails for it are deleted too.`,
      { title: "Move to Trash", kind: "warning", okLabel: "Move to Trash", cancelLabel: "Cancel" }
    );
    if (confirmed) run(() => trashVideo(video.id));
  }

  async function handleReset(video: LibraryVideo) {
    const confirmed = await ask(
      `Start over with "${video.title}"?\n\nThis deletes its transcript, trimmed video and thumbnails. The video file itself is kept.`,
      { title: "Reset", kind: "warning", okLabel: "Reset", cancelLabel: "Cancel" }
    );
    if (confirmed) run(() => resetVideo(video.id));
  }

  if (!library) {
    return (
      <div className="flex flex-col items-center justify-center h-full gap-4 py-24">
        {loadError ? (
          <>
            <AlertCircle className="w-12 h-12 text-destructive" />
            <p className="text-destructive">{loadError}</p>
            <button onClick={rescan} className="px-4 py-2 bg-primary text-primary-foreground rounded-lg">
              Retry
            </button>
          </>
        ) : (
          <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
        )}
      </div>
    );
  }

  const hiddenCount = library.videos.filter((v) => v.hidden).length;
  const videos = library.videos.filter((v) => showHidden || !v.hidden);
  const sharedNames = duplicateFileNames(library.videos);
  const maxMinutes = Math.round(library.max_transcribe_secs / 60);
  // The video waiting longest for review.
  const nextToReview = library.videos
    .filter((v) => v.status === "ready_for_review" && !v.hidden)
    .sort((a, b) => a.added_at.localeCompare(b.added_at))[0];

  return (
    <div className="p-6">
      <div className="flex items-start justify-between gap-4 mb-6">
        <div className="min-w-0">
          <h2 className="text-2xl font-bold">Video Library</h2>
          <p className="text-sm text-muted-foreground truncate" title={library.folder}>
            {library.folder}
          </p>
        </div>
        <div className="flex items-center gap-2 flex-shrink-0">
          {nextToReview && (
            <button
              onClick={() => onVideoSelect(nextToReview)}
              title={`Open "${nextToReview.title}", the video waiting longest`}
              className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors"
            >
              <Eye className="w-5 h-5" />
              Review next
            </button>
          )}
          {hiddenCount > 0 && (
            <button
              onClick={() => setShowHidden(!showHidden)}
              className="flex items-center gap-2 px-3 py-2 text-sm text-muted-foreground rounded-lg hover:bg-secondary transition-colors"
            >
              {showHidden ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
              {showHidden ? "Hide hidden" : `Show hidden (${hiddenCount})`}
            </button>
          )}
          <button
            onClick={() => run(openLibraryFolder)}
            disabled={!library.available}
            className="flex items-center gap-2 px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
          >
            <FolderOpen className="w-5 h-5" />
            Open Folder
          </button>
        </div>
      </div>

      <div className="space-y-3 mb-4">
        {library.tool_problem && (
          <Banner tone="error" icon={<AlertCircle className="w-5 h-5" />}>
            <span className="whitespace-pre-line">{library.tool_problem}</span>
          </Banner>
        )}
        {library.message && (
          <Banner
            tone={library.available ? "warning" : "error"}
            icon={library.locked_by ? <Lock className="w-5 h-5" /> : library.available ? <Info className="w-5 h-5" /> : <HardDrive className="w-5 h-5" />}
            action={
              library.locked_by ? (
                <button
                  onClick={async () => {
                    const confirmed = await ask(
                      `Only do this if Sermon Cut is closed on ${library.locked_by}. If both computers change the library at once, one of them loses its changes.`,
                      { title: "Use this library here", kind: "warning", okLabel: "Use Here", cancelLabel: "Cancel" }
                    );
                    if (confirmed) run(async () => setLibrary(await takeOverLibrary()));
                  }}
                  className="px-3 py-1.5 text-sm bg-secondary rounded-lg hover:bg-secondary/80"
                >
                  Use Here
                </button>
              ) : undefined
            }
          >
            {library.message}
          </Banner>
        )}
        {library.notices.map((notice, i) => (
          <Banner
            key={i}
            tone="info"
            icon={<Info className="w-5 h-5" />}
            action={
              <button onClick={() => run(dismissNotices)} className="p-1 rounded hover:bg-secondary" title="Dismiss">
                <X className="w-4 h-4" />
              </button>
            }
          >
            {notice}
          </Banner>
        ))}
        {actionError && (
          <ErrorBanner message={actionError} onDismiss={() => setActionError(null)} />
        )}
      </div>

      {library.downloads.length > 0 && (
        <div className="grid gap-4 mb-4">
          {library.downloads.map((d) => (
            <DownloadCard key={d.job_id} item={d} progress={downloads[d.youtube_id]} onCancel={() => run(() => cancelDownload(d.job_id))} />
          ))}
        </div>
      )}

      {videos.length === 0 && library.downloads.length === 0 ? (
        <div className="flex flex-col items-center justify-center gap-3 py-24 text-muted-foreground text-center">
          <Film className="w-16 h-16" />
          <p>No videos yet</p>
          <p className="text-sm max-w-md">
            Drop video files into <span className="font-medium text-foreground">{library.folder}</span> and
            they'll appear here, or download one from YouTube.
          </p>
        </div>
      ) : (
        <div className="grid gap-4">
          {videos.map((video) => (
            <VideoCard
              key={video.id}
              video={video}
              sharedName={sharedNames.has(video.file_name.normalize("NFC"))}
              live={progress[video.id]}
              uploadPercent={uploads[video.id] ?? video.upload_progress ?? 0}
              writable={library.writable}
              maxMinutes={maxMinutes}
              menuOpen={openMenu === video.id}
              onToggleMenu={() => setOpenMenu(openMenu === video.id ? null : video.id)}
              onOpen={() => onVideoSelect(video)}
              onProcess={() => run(() => processVideo(video.id))}
              onCancel={() => run(() => cancelProcessing(video.id))}
              onCancelUpload={() => run(() => cancelUpload(video.id))}
              onReveal={() => run(() => revealVideo(video.id))}
              onRevealLeftovers={() => run(() => revealCommitBackup(video.id))}
              onReset={() => handleReset(video)}
              onHide={() => run(() => hideVideo(video.id, !video.hidden))}
              onTrash={() => handleTrash(video)}
              onForget={() => run(() => forgetVideo(video.id))}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function Banner({
  tone,
  icon,
  action,
  children,
}: {
  tone: "error" | "warning" | "info";
  icon: React.ReactNode;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  const colors = {
    error: "bg-destructive/10 text-destructive",
    warning: "bg-yellow-100 text-yellow-800 dark:bg-yellow-900/30 dark:text-yellow-300",
    info: "bg-blue-50 text-blue-800 dark:bg-blue-900/30 dark:text-blue-300",
  }[tone];
  return (
    <div className={`flex items-start gap-3 p-3 rounded-lg text-sm ${colors}`}>
      <span className="flex-shrink-0 mt-0.5">{icon}</span>
      <div className="flex-1 min-w-0 break-words">{children}</div>
      {action && <div className="flex-shrink-0">{action}</div>}
    </div>
  );
}

function DownloadCard({ item, progress, onCancel }: { item: DownloadItem; progress?: DownloadProgress; onCancel: () => void }) {
  const merging = progress?.status === "processing";
  const pct = progress?.progress ?? 0;
  return (
    <div className="flex gap-4 p-4 bg-card border border-border rounded-lg">
      <div className="w-48 h-28 bg-muted rounded-lg flex items-center justify-center flex-shrink-0">
        <Download className="w-8 h-8 text-muted-foreground" />
      </div>
      <div className="flex-1 min-w-0">
        <h3 className="font-semibold truncate">{item.title}</h3>
        <div className="mt-2 text-sm text-muted-foreground flex items-center gap-2">
          <Loader2 className="w-4 h-4 animate-spin" />
          {merging ? "Finishing download..." : "Downloading from YouTube"}
        </div>
        <div className="mt-2 h-2 bg-muted rounded-full overflow-hidden">
          <div
            className={`h-full transition-all duration-300 ${merging ? "bg-yellow-500 animate-pulse" : "bg-primary"}`}
            style={{ width: `${merging ? 100 : pct}%` }}
          />
        </div>
        {!merging && (
          <p className="text-xs text-muted-foreground mt-1">
            {pct.toFixed(1)}% {progress?.speed && `• ${progress.speed}`} {progress?.eta && `• ETA ${progress.eta}`}
          </p>
        )}
      </div>
      <div>
        <button
          onClick={onCancel}
          className="p-2 text-muted-foreground hover:text-destructive rounded-lg hover:bg-destructive/10 transition-colors"
          title="Cancel download"
        >
          <X className="w-5 h-5" />
        </button>
      </div>
    </div>
  );
}

interface VideoCardProps {
  video: LibraryVideo;
  /** Another item has the same file name: show what tells them apart. */
  sharedName: boolean;
  live?: ProcessingProgress;
  uploadPercent: number;
  writable: boolean;
  maxMinutes: number;
  menuOpen: boolean;
  onToggleMenu: () => void;
  onOpen: () => void;
  onProcess: () => void;
  onCancel: () => void;
  onCancelUpload: () => void;
  onReveal: () => void;
  onRevealLeftovers: () => void;
  onReset: () => void;
  onHide: () => void;
  onTrash: () => void;
  onForget: () => void;
}

function VideoCard(props: VideoCardProps) {
  const { video, live, writable, maxMinutes } = props;
  const reviewable = video.status === "ready_for_review" || video.status === "done" || video.status === "uploading";
  const pending = video.id.startsWith("pending:");
  const dimmed = video.status === "missing" || video.status === "copying" || video.status === "unreadable";
  const lastUpload = video.uploads[video.uploads.length - 1];
  const processing = video.status === "processing";
  const pct = (processing && live?.progress) || video.processing_progress || 0;
  const message = (processing && live?.message) || video.processing_message || "Processing...";

  return (
    <div
      className={`relative flex gap-4 p-4 bg-card border border-border rounded-lg transition-colors ${
        reviewable ? "cursor-pointer hover:bg-accent" : ""
      } ${dimmed ? "opacity-60" : ""} ${video.hidden ? "border-dashed" : ""}`}
    >
      {/* The whole card opens the review: one button stretched over it.
          The card's own controls sit above it, as its siblings. */}
      {reviewable && (
        <button
          type="button"
          onClick={props.onOpen}
          aria-label={`Review ${video.title}`}
          className="absolute inset-0 rounded-lg focus:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        />
      )}
      <div className="relative pointer-events-none w-48 h-28 bg-muted rounded-lg overflow-hidden flex-shrink-0">
        {video.thumbnail ? (
          <LocalImage path={video.thumbnail} alt={video.title} className="w-full h-full object-cover" />
        ) : (
          <div className="w-full h-full flex items-center justify-center">
            <Film className="w-8 h-8 text-muted-foreground" />
          </div>
        )}
      </div>

      <div className="relative pointer-events-none flex-1 min-w-0">
        <h3 className="font-semibold truncate" title={video.title}>
          {video.title}
        </h3>
        <p className="text-sm text-muted-foreground truncate" title={video.file_name}>
          {props.sharedName
            ? `${video.file_name} · added ${formatAddedDate(video.added_at)} · ${formatFileSize(video.size)}`
            : video.title !== video.file_name.replace(/\.[^.]+$/, "")
              ? video.file_name
              : video.channel}
        </p>

        <div className="flex items-center gap-3 mt-2 text-sm text-muted-foreground flex-wrap">
          {video.duration ? (
            <span className="flex items-center gap-1">
              <Clock className="w-4 h-4" />
              {formatDuration(video.duration)}
            </span>
          ) : null}
          <StatusBadge
            status={video.status}
            stage={live?.stage ?? video.processing_stage}
            queue={video.queue_position && video.queue_length ? [video.queue_position, video.queue_length] : undefined}
          />
          {video.hidden && <span className="text-xs">Hidden</span>}
        </div>

        {processing && (
          <div className="mt-2">
            <div className="h-2 bg-muted rounded-full overflow-hidden">
              <div className="h-full bg-orange-500 transition-all duration-300" style={{ width: `${pct}%` }} />
            </div>
            <p className="text-xs text-muted-foreground mt-1">{message}</p>
          </div>
        )}

        {video.status === "uploading" && (
          <div className="mt-2">
            <div className="h-2 bg-muted rounded-full overflow-hidden">
              <div className="h-full bg-red-600 transition-all duration-300" style={{ width: `${props.uploadPercent}%` }} />
            </div>
            <p className="text-xs text-muted-foreground mt-1">
              Uploading to YouTube… {props.uploadPercent}%. You can keep using the app; don't quit until it's done.
            </p>
          </div>
        )}

        {/* Any status: a failed re-process keeps the earlier results and says why. */}
        {video.error && (
          <p className="mt-2 text-xs text-destructive line-clamp-3" title={video.error}>
            {video.error}
          </p>
        )}
        {isStuckCommit(video.error) && (
          <button
            onClick={props.onRevealLeftovers}
            className="pointer-events-auto mt-2 px-3 py-1 text-xs bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
          >
            Show leftovers
          </button>
        )}
        {video.status === "too_long" && (
          <p className="mt-2 text-xs text-muted-foreground">
            Longer than {maxMinutes} minutes, so it can't be transcribed yet. Trim it to the sermon and drop the
            shorter file into the folder.
          </p>
        )}
        {video.status === "missing" && (
          <p className="mt-2 text-xs text-muted-foreground">
            The file isn't in the library folder any more. Put it back, or Forget this video.
          </p>
        )}
        {lastUpload && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              openUrl(lastUpload.url);
            }}
            className="pointer-events-auto mt-2 text-xs text-primary hover:underline flex items-center gap-1"
          >
            {video.status === "done" ? "On YouTube" : "A previous version was uploaded"}
            <ExternalLink className="w-3 h-3" />
          </button>
        )}
        {video.status === "done" && lastUpload?.warnings && lastUpload.warnings.length > 0 && (
          <div className="pointer-events-auto">
            <UploadWarnings videoId={video.id} warnings={lastUpload.warnings} compact />
          </div>
        )}
      </div>

      {!pending && (
        <div className="relative flex items-start gap-2">
          {(video.status === "new" || video.status === "error") && (
            <button
              onClick={props.onProcess}
              disabled={!writable}
              className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 disabled:opacity-50 transition-colors"
            >
              {video.status === "error" ? <RotateCcw className="w-4 h-4" /> : <Play className="w-4 h-4" />}
              {video.status === "error" ? "Retry" : "Process"}
            </button>
          )}
          {(video.status === "queued" || processing) && (
            <button
              onClick={props.onCancel}
              className="flex items-center gap-2 px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
            >
              <X className="w-4 h-4" />
              Cancel
            </button>
          )}
          {video.status === "uploading" && (
            <button
              onClick={props.onCancelUpload}
              className="flex items-center gap-2 px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
            >
              <X className="w-4 h-4" />
              Cancel upload
            </button>
          )}
          {reviewable && (
            <button
              onClick={props.onOpen}
              className="flex items-center gap-2 px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
            >
              <Eye className="w-4 h-4" />
              Review
            </button>
          )}
          {video.status === "missing" && (
            <button
              onClick={props.onForget}
              disabled={!writable || video.busy}
              className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
            >
              Forget
            </button>
          )}

          {video.status !== "missing" && (
            <div className="relative">
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  props.onToggleMenu();
                }}
                className="p-2 text-muted-foreground rounded-lg hover:bg-secondary transition-colors"
                title="More"
                aria-label={`More actions for ${video.title}`}
                aria-haspopup="menu"
                aria-expanded={props.menuOpen}
              >
                <MoreHorizontal className="w-5 h-5" />
              </button>
              {props.menuOpen && (
                <div
                  role="menu"
                  onClick={props.onToggleMenu}
                  className="absolute right-0 top-10 z-10 w-48 bg-card border border-border rounded-lg shadow-lg py-1 text-sm"
                >
                  <MenuItem icon={<Search className="w-4 h-4" />} label="Show in Finder" onClick={props.onReveal} />
                  <MenuItem
                    icon={<RotateCcw className="w-4 h-4" />}
                    label="Reset"
                    disabled={!writable || video.busy || video.status === "new" || video.status === "too_long"}
                    onClick={props.onReset}
                  />
                  <MenuItem
                    icon={video.hidden ? <Eye className="w-4 h-4" /> : <EyeOff className="w-4 h-4" />}
                    label={video.hidden ? "Unhide" : "Hide"}
                    disabled={!writable || video.busy}
                    onClick={props.onHide}
                  />
                  <MenuItem
                    icon={<Trash2 className="w-4 h-4" />}
                    label="Move to Trash"
                    danger
                    disabled={!writable || video.busy}
                    onClick={props.onTrash}
                  />
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function MenuItem({
  icon,
  label,
  onClick,
  disabled,
  danger,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      role="menuitem"
      onClick={onClick}
      disabled={disabled}
      className={`w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-secondary disabled:opacity-40 disabled:hover:bg-transparent ${
        danger ? "text-destructive" : ""
      }`}
    >
      {icon}
      {label}
    </button>
  );
}

const STATUS: Record<LibraryStatus, { label: string; className: string; icon: "clock" | "check" | "eye" | "alert" | "spin" }> = {
  copying: { label: "Copying…", className: "bg-muted text-muted-foreground", icon: "spin" },
  unreadable: { label: "Can't read", className: "bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400", icon: "alert" },
  new: { label: "Not processed", className: "bg-muted text-muted-foreground", icon: "clock" },
  too_long: { label: "Too long", className: "bg-yellow-100 text-yellow-700 dark:bg-yellow-900/30 dark:text-yellow-400", icon: "alert" },
  queued: { label: "Queued", className: "bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400", icon: "clock" },
  processing: { label: "Processing", className: "bg-orange-100 text-orange-700 dark:bg-orange-900/30 dark:text-orange-400", icon: "spin" },
  uploading: { label: "Uploading", className: "bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400", icon: "spin" },
  ready_for_review: { label: "Ready for Review", className: "bg-primary/10 text-primary", icon: "eye" },
  done: { label: "Uploaded", className: "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400", icon: "check" },
  error: { label: "Error", className: "bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400", icon: "alert" },
  missing: { label: "Missing", className: "bg-muted text-muted-foreground", icon: "alert" },
};

function StatusBadge({
  status,
  stage,
  queue,
}: {
  status: LibraryStatus;
  stage?: ProcessingStage | null;
  /** Place in line and its length, for a queued video. */
  queue?: [number, number];
}) {
  const config = STATUS[status];
  const label =
    status === "processing"
      ? getProcessingLabel(stage ?? undefined)
      : status === "queued" && queue
        ? `Queued (${queue[0]} of ${queue[1]})`
        : config.label;
  const icon = {
    clock: <Clock className="w-4 h-4" />,
    check: <CheckCircle className="w-4 h-4" />,
    eye: <Eye className="w-4 h-4" />,
    alert: <AlertCircle className="w-4 h-4" />,
    spin: <Loader2 className="w-4 h-4 animate-spin" />,
  }[config.icon];
  return (
    <span className={`flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium ${config.className}`}>
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

function formatAddedDate(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}
