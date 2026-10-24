import { useCallback, useEffect, useRef, useState } from "react";
import { ErrorBanner } from "./ErrorBanner";
import type { DownloadProgress } from "@/lib/types";
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
  dismissNotices,
  takeOverLibrary,
  cancelDownload,
  cancelUpload,
  errorMessage,
} from "@/lib/tauri";
import { ask } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import type { LibraryVideo, LibraryView } from "@/lib/types";
import { duplicateFileNames } from "@/lib/utils";
import { DownloadCard } from "./library/DownloadCard";
import { LibraryToolbar } from "./library/LibraryToolbar";
import { VideoCard, type ProcessingProgress } from "./library/VideoCard";
import {
  Film,
  Loader2,
  AlertCircle,
  X,
  HardDrive,
  Lock,
  Info,
} from "lucide-react";

interface UploadProgress {
  video_id: string;
  progress: number;
  status: string;
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
      <LibraryToolbar
        library={library}
        nextToReview={nextToReview}
        onVideoSelect={onVideoSelect}
        hiddenCount={hiddenCount}
        showHidden={showHidden}
        setShowHidden={setShowHidden}
        run={run}
      />

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
      <span className="shrink-0 mt-0.5">{icon}</span>
      <div className="flex-1 min-w-0 wrap-break-word">{children}</div>
      {action && <div className="shrink-0">{action}</div>}
    </div>
  );
}
