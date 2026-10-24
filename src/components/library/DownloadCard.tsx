import { Download, Loader2, X } from "lucide-react";
import type { DownloadItem, DownloadProgress } from "@/lib/types";

export function DownloadCard({ item, progress, onCancel }: { item: DownloadItem; progress?: DownloadProgress; onCancel: () => void }) {
  const merging = progress?.status === "processing";
  const pct = progress?.progress ?? 0;
  return (
    <div className="flex gap-4 p-4 bg-card border border-border rounded-lg">
      <div className="w-48 h-28 bg-muted rounded-lg flex items-center justify-center shrink-0">
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
