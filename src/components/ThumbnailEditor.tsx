import { useState, useEffect, useCallback, useRef } from "react";
import { useModalDialog } from "@/hooks/useModalDialog";
import { useCanvasEditor, type ImageLoad } from "@/hooks/useCanvasEditor";
import { readImageBase64, processCustomThumbnail } from "@/lib/tauri";
import { open } from "@tauri-apps/plugin-dialog";
import { LocalImage } from "./LocalImage";
import type { ThumbnailOption } from "@/lib/types";
import {
  X,
  ZoomIn,
  ZoomOut,
  Maximize2,
  RotateCcw,
  Upload,
  Loader2,
  Check,
} from "lucide-react";

interface ThumbnailEditorProps {
  videoId: string;
  thumbnailOptions: ThumbnailOption[];
  /** The selected option's id. */
  selectedSource: string | null;
  onSave: (newThumbnailPath: string) => void;
  onClose: () => void;
}

export function ThumbnailEditor({
  videoId,
  thumbnailOptions,
  selectedSource,
  onSave,
  onClose,
}: ThumbnailEditorProps) {
  const [sourceType, setSourceType] = useState<"ai" | "custom">("ai");
  const [selectedAiIndex, setSelectedAiIndex] = useState(() =>
    Math.max(0, thumbnailOptions.findIndex((o) => o.id === selectedSource))
  );
  const [customImagePath, setCustomImagePath] = useState<string | null>(null);
  // The file being loaded for the current selection; Apply waits until it's
  // the one on the canvas.
  const [wantedPath, setWantedPath] = useState<string | null>(null);
  const [applyColorGrading, setApplyColorGrading] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const {
    canvasRef,
    loadImage,
    zoom,
    setZoom,
    zoomIn,
    zoomOut,
    center,
    reset,
    getCropRect,
    imageLoaded,
    loadedKey,
    handlers,
  } = useCanvasEditor({ aspectRatio: 16 / 9 });
  // The image on the canvas, once it's the selected one; the crop is cut
  // from this file.
  const displayedPath = loadedKey !== null && loadedKey === wantedPath ? loadedKey : null;

  // Load the chosen source: a generated option's plain crop (no logo or
  // grading burned in) when there is one, else the option itself.
  useEffect(() => {
    let cancelled = false;
    const candidates: string[] = [];
    if (sourceType === "custom" && customImagePath) {
      candidates.push(customImagePath);
    } else if (sourceType === "ai" && thumbnailOptions[selectedAiIndex]) {
      const option = thumbnailOptions[selectedAiIndex];
      candidates.push(option.render_from);
      if (option.path !== option.render_from) candidates.push(option.path);
    }
    setWantedPath(null);
    if (candidates.length === 0) return;

    let load: ImageLoad | null = null;
    (async () => {
      for (const [i, path] of candidates.entries()) {
        try {
          const base64 = await readImageBase64(path);
          if (cancelled) return;
          setWantedPath(path);
          load = loadImage(base64, path);
          const result = await load.done;
          if (result === "superseded" || cancelled) return;
          if (result === "loaded") {
            setError(i === 0 ? null : "Using the processed thumbnail (the plain crop wasn't found)");
            return;
          }
        } catch (err) {
          console.error("Failed to load image:", err);
        }
      }
      if (!cancelled) {
        setWantedPath(null);
        setError("Failed to load image");
      }
    })();
    return () => {
      cancelled = true;
      load?.cancel();
    };
  }, [sourceType, selectedAiIndex, customImagePath, thumbnailOptions, loadImage]);

  const handleUploadCustom = useCallback(async () => {
    try {
      const selected = await open({
        multiple: false,
        filters: [
          {
            name: "Images",
            extensions: ["png", "jpg", "jpeg", "webp"],
          },
        ],
      });

      if (selected && typeof selected === "string") {
        setCustomImagePath(selected);
        setSourceType("custom");
      }
    } catch (err) {
      console.error("Failed to open file dialog:", err);
    }
  }, []);

  const handleSelectAiOption = useCallback((index: number) => {
    setSelectedAiIndex(index);
    setSourceType("ai");
  }, []);

  const handleApplyAndSave = useCallback(async () => {
    const cropRect = getCropRect();
    if (!cropRect || !displayedPath) {
      setError("No image loaded");
      return;
    }

    try {
      setIsSaving(true);
      setError(null);

      // The crop was made on the displayed image, so it's cut from that one.
      const newThumbnailPath = await processCustomThumbnail(
        videoId,
        displayedPath,
        cropRect,
        applyColorGrading
      );

      onSave(newThumbnailPath);
    } catch (err) {
      console.error("Failed to process thumbnail:", err);
      setError(err instanceof Error ? err.message : String(err || "Failed to process thumbnail"));
    } finally {
      setIsSaving(false);
    }
  }, [videoId, displayedPath, getCropRect, applyColorGrading, onSave]);

  const zoomPercent = Math.round(zoom * 100);
  const dialogRef = useRef<HTMLDivElement>(null);
  useModalDialog(dialogRef, onClose);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/80">
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="thumbnail-editor-title"
        tabIndex={-1}
        className="bg-card border border-border rounded-xl shadow-xl w-full max-w-4xl max-h-[90vh] overflow-hidden flex flex-col"
      >
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <h2 id="thumbnail-editor-title" className="text-lg font-semibold">Edit Thumbnail</h2>
          <button
            onClick={onClose}
            aria-label="Close"
            title="Close (Esc)"
            className="p-2 hover:bg-secondary rounded-lg transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-auto p-6 space-y-6">
          {/* Canvas Preview */}
          <div
            className="relative bg-black rounded-lg overflow-hidden"
            style={{ aspectRatio: "16/9" }}
          >
            <canvas
              ref={canvasRef}
              aria-label="Crop preview: drag to move the image"
              className="w-full h-full cursor-grab active:cursor-grabbing"
              style={{ display: "block" }}
              {...handlers}
            />
            {!imageLoaded && (
              <div className="absolute inset-0 flex items-center justify-center">
                <div className="text-muted-foreground text-sm">
                  Select an image to edit
                </div>
              </div>
            )}
          </div>

          {/* Zoom Controls */}
          <div className="flex items-center gap-4">
            <span className="text-sm text-muted-foreground w-12" aria-hidden="true">Zoom:</span>
            <button
              onClick={zoomOut}
              aria-label="Zoom out"
              className="p-2 hover:bg-secondary rounded-lg transition-colors"
              disabled={!imageLoaded}
            >
              <ZoomOut className="w-4 h-4" />
            </button>
            <input
              type="range"
              min="100"
              max="500"
              value={zoomPercent}
              aria-label="Zoom"
              onChange={(e) => setZoom(Number(e.target.value) / 100)}
              className="flex-1 h-2 bg-secondary rounded-lg appearance-none cursor-pointer"
              disabled={!imageLoaded}
            />
            <button
              onClick={zoomIn}
              aria-label="Zoom in"
              className="p-2 hover:bg-secondary rounded-lg transition-colors"
              disabled={!imageLoaded}
            >
              <ZoomIn className="w-4 h-4" />
            </button>
            <span className="text-sm text-muted-foreground w-16 text-right">
              {zoomPercent}%
            </span>
          </div>

          {/* Pan Controls */}
          <div className="flex items-center gap-2">
            <button
              onClick={center}
              disabled={!imageLoaded}
              className="flex items-center gap-2 px-3 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
            >
              <Maximize2 className="w-4 h-4" />
              Center
            </button>
            <button
              onClick={reset}
              disabled={!imageLoaded}
              className="flex items-center gap-2 px-3 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
            >
              <RotateCcw className="w-4 h-4" />
              Reset
            </button>
          </div>

          {/* Source Selection */}
          <div className="space-y-3">
            <span id="thumbnail-editor-source" className="text-sm font-medium">Source:</span>
            <div role="group" aria-labelledby="thumbnail-editor-source" className="flex flex-wrap gap-2">
              {/* Upload Custom Button */}
              <button
                onClick={handleUploadCustom}
                aria-label={customImagePath ? "Your image (choose another)" : "Choose your own image"}
                aria-pressed={sourceType === "custom" && !!customImagePath}
                className={`relative aspect-video w-24 rounded-lg border-2 transition-colors flex flex-col items-center justify-center gap-1 ${
                  sourceType === "custom" && customImagePath
                    ? "border-primary ring-2 ring-primary/30"
                    : "border-dashed border-muted-foreground/30 hover:border-muted-foreground/50"
                }`}
              >
                {customImagePath ? (
                  <>
                    <LocalImage
                      path={customImagePath}
                      alt="Custom"
                      className="w-full h-full object-cover rounded-md"
                    />
                    {sourceType === "custom" && (
                      <div className="absolute top-1 right-1 bg-primary text-primary-foreground rounded-full p-0.5">
                        <Check className="w-3 h-3" />
                      </div>
                    )}
                  </>
                ) : (
                  <>
                    <Upload className="w-4 h-4 text-muted-foreground" />
                    <span className="text-xs text-muted-foreground">Upload</span>
                  </>
                )}
              </button>

              {/* AI Options */}
              {thumbnailOptions.map((option, index) => (
                <button
                  key={option.id}
                  onClick={() => handleSelectAiOption(index)}
                  aria-label={`AI Option ${index + 1}`}
                  aria-pressed={sourceType === "ai" && selectedAiIndex === index}
                  className={`relative aspect-video w-24 rounded-lg border-2 overflow-hidden transition-colors ${
                    sourceType === "ai" && selectedAiIndex === index
                      ? "border-primary ring-2 ring-primary/30"
                      : "border-transparent hover:border-muted-foreground/30"
                  }`}
                >
                  <LocalImage
                    path={option.path}
                    alt={`AI Option ${index + 1}`}
                    className="w-full h-full object-cover"
                  />
                  {sourceType === "ai" && selectedAiIndex === index && (
                    <div className="absolute top-1 right-1 bg-primary text-primary-foreground rounded-full p-0.5">
                      <Check className="w-3 h-3" />
                    </div>
                  )}
                </button>
              ))}
            </div>
          </div>

          {/* Options */}
          <div className="space-y-3">
            <label className="flex items-center gap-3 cursor-pointer">
              <input
                type="checkbox"
                checked={applyColorGrading}
                onChange={(e) => setApplyColorGrading(e.target.checked)}
                className="w-4 h-4 rounded border-input"
              />
              <span className="text-sm">Apply color grading</span>
            </label>
          </div>

          {/* Error */}
          {error && (
            <div role="alert" className="p-3 bg-destructive/10 text-destructive rounded-lg text-sm">
              {error}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-3 p-4 border-t border-border">
          <button
            onClick={onClose}
            className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={handleApplyAndSave}
            disabled={isSaving || !displayedPath}
            className="px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 disabled:opacity-50 flex items-center gap-2 transition-colors"
          >
            {isSaving ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Processing...
              </>
            ) : (
              "Apply & Save"
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
