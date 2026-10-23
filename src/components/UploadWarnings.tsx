import { useState } from "react";
import { AlertTriangle, Loader2, RefreshCw } from "lucide-react";
import { errorMessage, retryThumbnail } from "@/lib/tauri";
import type { UploadWarning } from "@/lib/types";

interface UploadWarningsProps {
  videoId: string;
  warnings: UploadWarning[];
  /** The warnings left after "Set thumbnail again" worked. */
  onChange?: (warnings: UploadWarning[]) => void;
  compact?: boolean;
}

/** What didn't work after an upload, with "Set thumbnail again" for a
 * thumbnail that wasn't set (by the stored YouTube id, so the video isn't
 * uploaded twice). */
export function UploadWarnings({ videoId, warnings, onChange, compact }: UploadWarningsProps) {
  const [retrying, setRetrying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (warnings.length === 0) return null;

  const handleRetry = async () => {
    setRetrying(true);
    setError(null);
    try {
      onChange?.(await retryThumbnail(videoId));
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setRetrying(false);
    }
  };

  return (
    <div
      className={`rounded-lg bg-yellow-100 text-yellow-800 dark:bg-yellow-900/30 dark:text-yellow-300 ${
        compact ? "mt-2 p-2 text-xs" : "p-3 text-sm"
      }`}
      onClick={(e) => e.stopPropagation()}
    >
      <ul className="space-y-1">
        {warnings.map((w) => (
          <li key={w.kind} className="flex items-start gap-2">
            <AlertTriangle className="w-4 h-4 flex-shrink-0 mt-0.5" />
            <span className="flex-1">{w.message}</span>
            {w.kind === "thumbnail" && (
              <button
                onClick={handleRetry}
                disabled={retrying}
                className="flex items-center gap-1 px-2 py-1 rounded-md bg-yellow-200 hover:bg-yellow-300 dark:bg-yellow-800/50 disabled:opacity-50 whitespace-nowrap"
              >
                {retrying ? <Loader2 className="w-3 h-3 animate-spin" /> : <RefreshCw className="w-3 h-3" />}
                Set thumbnail again
              </button>
            )}
          </li>
        ))}
      </ul>
      {error && <p className="mt-1 text-destructive">{error}</p>}
    </div>
  );
}
