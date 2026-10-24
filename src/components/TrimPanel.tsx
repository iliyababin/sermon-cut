import { useEffect, useRef, useState } from "react";
import { ErrorBanner } from "./ErrorBanner";
import { convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Loader2, Scissors, Crosshair, Play } from "lucide-react";
import { frameStill, setTrim, errorMessage, isStuckCommit, revealCommitBackup } from "@/lib/tauri";
import { formatPrecise, lineAt, nudge, MIN_TRIM_SECS } from "@/lib/trim";
import type { TranscriptSegment } from "@/lib/types";
import { LocalImage } from "./LocalImage";

interface TrimPanelProps {
  videoId: string;
  /** The original file, which the trim is cut from. */
  sourcePath: string;
  /** Length of the original, if known. */
  duration: number | null;
  /** Where the trimmed video starts and ends now. */
  start: number;
  end: number;
  segments: TranscriptSegment[];
  /** E.g. while the video uploads. */
  disabled?: boolean;
  onTrimmed: (start: number, end: number) => void;
}

type Side = "start" | "end";

const NUDGES = [-5, -1, 1, 5];

/** Set where the sermon starts and ends: a preview of the original with
 * nudges, or stills of it when it can't play here (e.g. MKV or WebM). */
export function TrimPanel({ videoId, sourcePath, duration, start, end, segments, disabled, onTrimmed }: TrimPanelProps) {
  const [pending, setPending] = useState({ start, end });
  const [playable, setPlayable] = useState(true);
  const [applying, setApplying] = useState(false);
  const [progress, setProgress] = useState<{ progress: number; message: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  // After an apply: where the cut really starts.
  const [result, setResult] = useState<{ requested: number; actual: number } | null>(null);
  const videoRef = useRef<HTMLVideoElement>(null);
  const max = duration ?? Math.max(end, pending.end);

  useEffect(() => setPending({ start, end }), [start, end]);

  useEffect(() => {
    if (!applying) return;
    const unlisten = listen<{ video_id: string; progress: number; message: string }>("trim-progress", (e) => {
      if (e.payload.video_id === videoId) setProgress(e.payload);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [applying, videoId]);

  const set = (side: Side, time: number) => {
    setResult(null);
    setPending((p) => {
      const next = { ...p, [side]: nudge(time, 0, 0, max) };
      return next;
    });
  };
  const seek = (time: number) => {
    const v = videoRef.current;
    if (v) v.currentTime = time;
  };

  const changed = Math.abs(pending.start - start) > 0.05 || Math.abs(pending.end - end) > 0.05;
  const valid = pending.end - pending.start >= MIN_TRIM_SECS;

  const apply = async () => {
    setApplying(true);
    setError(null);
    setProgress(null);
    try {
      const cut = await setTrim(videoId, pending.start, pending.end);
      setResult({ requested: pending.start, actual: cut.start });
      onTrimmed(cut.start, cut.end);
    } catch (err) {
      setError(errorMessage(err, "Couldn't trim the video"));
    } finally {
      setApplying(false);
      setProgress(null);
    }
  };

  return (
    <div className="bg-card border border-border rounded-lg p-4 space-y-4">
      <div className="flex items-baseline justify-between">
        <h3 className="font-semibold">Start and end</h3>
        <span className="text-xs text-muted-foreground">
          Length {formatPrecise(Math.max(0, pending.end - pending.start))}
        </span>
      </div>

      {playable ? (
        <video
          ref={videoRef}
          src={convertFileSrc(sourcePath)}
          controls
          preload="metadata"
          aria-label="Original video"
          onLoadedMetadata={() => seek(start)}
          onError={() => setPlayable(false)}
          className="w-full aspect-video bg-black rounded-lg"
        />
      ) : (
        <p className="text-xs text-muted-foreground">
          This video can't play here, so stills of the chosen points are shown instead.
        </p>
      )}

      <div className="grid md:grid-cols-2 gap-4">
        {(["start", "end"] as Side[]).map((side) => (
          <TrimPoint
            key={side}
            label={side === "start" ? "Start" : "End"}
            time={pending[side]}
            videoId={videoId}
            line={lineAt(segments, pending[side])?.text ?? null}
            playable={playable}
            disabled={disabled || applying}
            onNudge={(delta) => set(side, nudge(pending[side], delta, 0, max))}
            onUseCurrent={() => videoRef.current && set(side, videoRef.current.currentTime)}
            onShow={() => seek(pending[side])}
          />
        ))}
      </div>

      {!valid && <p className="text-sm text-destructive">The end must be at least a second after the start.</p>}
      {error && (
        <ErrorBanner
          message={error}
          onReveal={isStuckCommit(error) ? () => revealCommitBackup(videoId).catch(() => {}) : undefined}
        />
      )}
      {result && !changed && (
        <p className="text-sm text-muted-foreground" aria-live="polite">
          Starts at {formatPrecise(result.actual)}
          {Math.abs(result.actual - result.requested) > 0.05 && " (the nearest clean cut)"}
        </p>
      )}

      <div className="flex items-center justify-end gap-2">
        {applying && progress && (
          <span className="text-xs text-muted-foreground mr-auto">
            {progress.message} {progress.progress}%
          </span>
        )}
        <button
          onClick={() => {
            setPending({ start, end });
            setResult(null);
          }}
          disabled={!changed || applying}
          className="px-3 py-1.5 text-sm bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
        >
          Undo changes
        </button>
        <button
          onClick={apply}
          disabled={!changed || !valid || applying || disabled}
          title={disabled ? "Wait for the upload to finish" : undefined}
          className="px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 disabled:opacity-50 flex items-center gap-1.5 transition-colors"
        >
          {applying ? <Loader2 className="w-3 h-3 animate-spin" /> : <Scissors className="w-3 h-3" />}
          {applying ? "Trimming…" : "Trim video"}
        </button>
      </div>
    </div>
  );
}

function TrimPoint({
  label,
  time,
  videoId,
  line,
  playable,
  disabled,
  onNudge,
  onUseCurrent,
  onShow,
}: {
  label: string;
  time: number;
  videoId: string;
  line: string | null;
  playable: boolean;
  disabled?: boolean;
  onNudge: (delta: number) => void;
  onUseCurrent: () => void;
  onShow: () => void;
}) {
  // Stills only when the video can't play; fetched once the point settles.
  const [still, setStill] = useState<string | null>(null);
  useEffect(() => {
    if (playable) return;
    let current = true;
    const timer = setTimeout(() => {
      frameStill(videoId, time)
        .then((path) => current && setStill(path))
        .catch(() => current && setStill(null));
    }, 400);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [playable, videoId, time]);

  return (
    <div className="space-y-2">
      <div className="flex items-baseline justify-between">
        <span className="text-sm font-medium">{label}</span>
        <span className="font-mono text-sm" aria-label={`${label} time`}>
          {formatPrecise(time)}
        </span>
      </div>
      {!playable && (
        <div className="aspect-video bg-black rounded-lg overflow-hidden">
          {still && <LocalImage path={still} alt={`${label} frame`} className="w-full h-full object-contain" />}
        </div>
      )}
      <div className="flex flex-wrap gap-1">
        {NUDGES.map((d) => (
          <button
            key={d}
            onClick={() => onNudge(d)}
            disabled={disabled}
            aria-label={`${label} ${d > 0 ? "+" : "−"}${Math.abs(d)} s`}
            className="px-2 py-1 text-xs bg-secondary text-secondary-foreground rounded hover:bg-secondary/80 disabled:opacity-50 font-mono"
          >
            {d > 0 ? "+" : "−"}
            {Math.abs(d)}s
          </button>
        ))}
        {playable && (
          <>
            <button
              onClick={onUseCurrent}
              disabled={disabled}
              className="px-2 py-1 text-xs bg-secondary text-secondary-foreground rounded hover:bg-secondary/80 disabled:opacity-50 flex items-center gap-1"
            >
              <Crosshair className="w-3 h-3" />
              Use current frame
            </button>
            <button
              onClick={onShow}
              className="px-2 py-1 text-xs bg-secondary text-secondary-foreground rounded hover:bg-secondary/80 flex items-center gap-1"
            >
              <Play className="w-3 h-3" />
              Show
            </button>
          </>
        )}
      </div>
      <p className="text-xs text-muted-foreground italic min-h-4">{line ? `“${line.trim()}”` : ""}</p>
    </div>
  );
}
