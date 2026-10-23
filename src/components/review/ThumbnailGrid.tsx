import { CheckCircle, Loader2, Pencil, RefreshCw } from "lucide-react";
import type { ThumbnailOption } from "@/lib/types";
import { LocalImage } from "../LocalImage";
import type { ReviewPhase } from "./reviewMachine";

interface ThumbnailGridProps {
  thumbnailOptions: ThumbnailOption[];
  selectedOption: ThumbnailOption | null;
  setSelectedSource: (id: string) => void;
  shownPath: (option: ThumbnailOption) => string;
  flush: () => Promise<boolean>;
  setIsEditorOpen: (open: boolean) => void;
  handleRegenerateThumbnails: () => void;
  isRegeneratingThumbnails: boolean;
  phase: ReviewPhase;
  singleLine: boolean;
  setSingleLine: (singleLine: boolean) => void;
  renderProgress: string;
}

export function ThumbnailGrid({
  thumbnailOptions,
  selectedOption,
  setSelectedSource,
  shownPath,
  flush,
  setIsEditorOpen,
  handleRegenerateThumbnails,
  isRegeneratingThumbnails,
  phase,
  singleLine,
  setSingleLine,
  renderProgress,
}: ThumbnailGridProps) {
  return (
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
  );
}
