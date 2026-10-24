import { HardDrive } from "lucide-react";
import type { StorageView } from "@/lib/types";
import { formatFileSize } from "@/lib/utils";

/** Fewer videos than this fit in the free space: say space is getting low. */
const LOW_ROOM = 10;
/** Fewer than this: say it's nearly full. */
const VERY_LOW_ROOM = 4;

/** What the summary shows, worked out from the storage figures. */
export function storageFigures(storage: StorageView) {
  const library = storage.videos.reduce((sum, v) => sum + v.original + v.files, 0) + storage.other;
  const free = storage.disk_free ?? 0;
  const total = storage.disk_total ?? 0;
  const elsewhere = Math.max(0, total - free - library);
  // A typical video: the processed ones (recording plus Sermon Cut's
  // files), else the recordings alone.
  const processed = storage.videos.filter((v) => v.original > 0 && v.files > 0);
  const sample = processed.length > 0 ? processed : storage.videos.filter((v) => v.original > 0);
  const perVideo = sample.length > 0 ? sample.reduce((sum, v) => sum + v.original + v.files, 0) / sample.length : null;
  const roomFor = perVideo ? Math.floor(free / perVideo) : null;
  const tone: "ok" | "low" | "very_low" =
    roomFor === null ? "ok" : roomFor < VERY_LOW_ROOM ? "very_low" : roomFor < LOW_ROOM ? "low" : "ok";
  return { library, free, total, elsewhere, perVideo, roomFor, tone };
}

const pct = (part: number, whole: number) => (whole > 0 ? `${Math.max(0, Math.min(100, (part / whole) * 100))}%` : "0%");

export function StorageSummary({ storage, folder }: { storage: StorageView; folder: string }) {
  if (storage.disk_total === null || storage.disk_free === null) return null;
  const { library, free, total, elsewhere, perVideo, roomFor, tone } = storageFigures(storage);
  const where = folder.startsWith("/Volumes/") ? "on this drive" : "on this Mac";
  const freeColor = tone === "very_low" ? "text-destructive" : tone === "low" ? "text-amber-700" : "text-foreground";

  return (
    <section aria-label="Storage" className="mb-4 p-4 bg-card border border-border rounded-lg">
      <div className="flex items-center gap-3">
        <div className="w-10 h-10 shrink-0 rounded-full bg-muted flex items-center justify-center">
          <HardDrive className="w-5 h-5 text-muted-foreground" />
        </div>
        <div className="flex-1 min-w-0">
          <p className="leading-tight">
            <span className={`text-lg font-semibold ${freeColor}`}>{formatFileSize(free)} free</span>
            <span className="text-sm text-muted-foreground"> of {formatFileSize(total)} {where}</span>
          </p>
          {perVideo !== null && roomFor !== null && (
            <p className="text-xs text-muted-foreground mt-0.5">
              A video takes about {formatFileSize(perVideo)} with its files, so there's room for about{" "}
              {roomFor === 1 ? "1 more" : `${roomFor} more`}.
            </p>
          )}
        </div>
        <div className="text-right shrink-0">
          <p className="text-sm font-medium">{formatFileSize(library)}</p>
          <p className="text-xs text-muted-foreground">
            {storage.videos.length === 1 ? "1 video" : `${storage.videos.length} videos`} in the library
          </p>
        </div>
      </div>

      <div
        className="mt-3 h-2.5 flex rounded-full overflow-hidden bg-muted"
        role="img"
        aria-label={`Sermon library ${formatFileSize(library)}, everything else ${formatFileSize(elsewhere)}, ${formatFileSize(free)} free`}
      >
        <div className="h-full bg-primary" style={{ width: pct(library, total) }} />
        <div className="h-full bg-slate-300" style={{ width: pct(elsewhere, total) }} />
      </div>

      <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
        <Legend swatch="bg-primary" label="Sermon library" size={library} />
        <Legend swatch="bg-slate-300" label="Everything else" size={elsewhere} />
        <Legend swatch="bg-muted border border-slate-300" label="Free" size={free} />
      </div>

      {tone !== "ok" && (
        <p className={`mt-3 text-xs ${tone === "very_low" ? "text-destructive" : "text-amber-700"}`}>
          {tone === "very_low" ? "Nearly out of space." : "Space is getting low."} Uploaded videos can go to the Trash
          from their ⋯ menu; empty the Trash to get the space back.
        </p>
      )}
    </section>
  );
}

function Legend({ swatch, label, size }: { swatch: string; label: string; size: number }) {
  return (
    <span className="flex items-center gap-1.5">
      <span className={`inline-block w-2.5 h-2.5 rounded-full ${swatch}`} aria-hidden="true" />
      {label} <span className="text-foreground font-medium">{formatFileSize(size)}</span>
    </span>
  );
}
