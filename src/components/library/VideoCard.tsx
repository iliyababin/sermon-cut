import {
  AlertCircle,
  CheckCircle,
  Clock,
  ExternalLink,
  Eye,
  EyeOff,
  Film,
  Loader2,
  MoreHorizontal,
  Play,
  RotateCcw,
  Search,
  Trash2,
  X,
} from "lucide-react";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import { formatTimeDisplay } from "@/lib/clips";
import { isStuckCommit } from "@/lib/tauri";
import type { LibraryStatus, LibraryVideo, ProcessingStage } from "@/lib/types";
import { formatFileSize } from "@/lib/utils";
import { LocalImage } from "../LocalImage";
import { UploadWarnings } from "../UploadWarnings";

export interface ProcessingProgress {
  video_id: string;
  stage: ProcessingStage;
  progress: number;
  message: string;
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

export function VideoCard(props: VideoCardProps) {
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
          className="absolute inset-0 rounded-lg focus:outline-hidden focus-visible:ring-2 focus-visible:ring-ring"
        />
      )}
      <div className="relative pointer-events-none w-48 h-28 bg-muted rounded-lg overflow-hidden shrink-0">
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
              {formatTimeDisplay(video.duration)}
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

function formatAddedDate(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}
