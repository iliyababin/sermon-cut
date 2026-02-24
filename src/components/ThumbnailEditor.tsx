import { useState, useEffect, useCallback, useRef } from "react";
import { useCanvasEditor } from "@/hooks/useCanvasEditor";
import { readImageBase64, processCustomThumbnail } from "@/lib/tauri";
import { open } from "@tauri-apps/plugin-dialog";
import { LocalImage } from "./LocalImage";
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
  thumbnailOptions: string[];
  selectedThumbnail: string | null;
  onSave: (newThumbnailPath: string) => void;
  onClose: () => void;
}

export function ThumbnailEditor({
  videoId,
  thumbnailOptions,
  selectedThumbnail,
  onSave,
  onClose,
}: ThumbnailEditorProps) {
  const [sourceType, setSourceType] = useState<"ai" | "custom">("ai");
  const [selectedAiIndex, setSelectedAiIndex] = useState(0);
  const [customImagePath, setCustomImagePath] = useState<string | null>(null);
  const [applyColorGrading, setApplyColorGrading] = useState(true);
  const [applyLogoOverlay, setApplyLogoOverlay] = useState(true);
  const [isSaving, setIsSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canvasContainerRef = useRef<HTMLDivElement>(null);

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
    handlers,
  } = useCanvasEditor({ aspectRatio: 16 / 9 });

  // Set canvas size on mount and resize
  useEffect(() => {
    const canvas = canvasRef.current;
    const container = canvasContainerRef.current;
    if (!canvas || !container) return;

    const updateSize = () => {
      const rect = container.getBoundingClientRect();
      const width = rect.width;
      const height = width / (16 / 9);
      canvas.width = width;
      canvas.height = height;
    };

    updateSize();

    const observer = new ResizeObserver(updateSize);
    observer.observe(container);

    return () => observer.disconnect();
  }, [canvasRef]);

  // Check if a thumbnail path is an AI-generated one (has a raw variant)
  const isAiThumbnail = (path: string): boolean => {
    return /thumbnail_option_\d+\.(jpg|jpeg)$/i.test(path);
  };

  // Get the raw version path for an AI thumbnail (without color grading/logo)
  const getRawPath = (processedPath: string): string => {
    // thumbnail_option_1.jpg -> thumbnail_option_1_raw.jpg
    return processedPath.replace(/\.(jpg|jpeg)$/i, "_raw.jpg");
  };

  // Load initial image
  useEffect(() => {
    const loadInitialImage = async () => {
      let pathToLoad: string | null = null;
      let tryRawFallback = false;

      if (sourceType === "custom" && customImagePath) {
        pathToLoad = customImagePath;
      } else if (sourceType === "ai" && thumbnailOptions.length > 0) {
        const processedPath = thumbnailOptions[selectedAiIndex];
        // Only try the raw version for AI-generated thumbnails, not custom ones
        if (isAiThumbnail(processedPath)) {
          pathToLoad = getRawPath(processedPath);
          tryRawFallback = true;
        } else {
          pathToLoad = processedPath;
        }
      }

      if (pathToLoad) {
        try {
          const base64 = await readImageBase64(pathToLoad);
          loadImage(base64);
          setError(null);
        } catch (err) {
          console.error("Failed to load image:", err);
          // If raw version doesn't exist, fall back to processed version
          if (tryRawFallback) {
            try {
              const base64 = await readImageBase64(thumbnailOptions[selectedAiIndex]);
              loadImage(base64);
              setError("Using processed thumbnail (raw version not found)");
            } catch {
              setError("Failed to load image");
            }
          } else {
            setError("Failed to load image");
          }
        }
      }
    };

    loadInitialImage();
  }, [sourceType, selectedAiIndex, customImagePath, thumbnailOptions, loadImage]);

  // Initialize selected AI index based on current selection
  useEffect(() => {
    if (selectedThumbnail) {
      const index = thumbnailOptions.indexOf(selectedThumbnail);
      if (index >= 0) {
        setSelectedAiIndex(index);
      }
    }
  }, [selectedThumbnail, thumbnailOptions]);

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
    if (!cropRect) {
      setError("No image loaded");
      return;
    }

    // Use raw version for AI thumbnails, custom path for uploads
    let sourcePath: string;
    if (sourceType === "custom" && customImagePath) {
      sourcePath = customImagePath;
    } else if (thumbnailOptions[selectedAiIndex]) {
      const selected = thumbnailOptions[selectedAiIndex];
      // Only use raw version for AI-generated thumbnails (not custom ones)
      sourcePath = isAiThumbnail(selected) ? getRawPath(selected) : selected;
    } else {
      setError("No source image selected");
      return;
    }

    try {
      setIsSaving(true);
      setError(null);

      const newThumbnailPath = await processCustomThumbnail(
        videoId,
        sourcePath,
        cropRect,
        applyColorGrading,
        applyLogoOverlay
      );

      onSave(newThumbnailPath);
    } catch (err) {
      console.error("Failed to process thumbnail:", err);
      setError(err instanceof Error ? err.message : "Failed to process thumbnail");
    } finally {
      setIsSaving(false);
    }
  }, [
    videoId,
    sourceType,
    customImagePath,
    thumbnailOptions,
    selectedAiIndex,
    getCropRect,
    applyColorGrading,
    applyLogoOverlay,
    onSave,
  ]);

  const zoomPercent = Math.round(zoom * 100);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/80">
      <div className="bg-card border border-border rounded-xl shadow-xl w-full max-w-4xl max-h-[90vh] overflow-hidden flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <h2 className="text-lg font-semibold">Edit Thumbnail</h2>
          <button
            onClick={onClose}
            className="p-2 hover:bg-secondary rounded-lg transition-colors"
          >
            <X className="w-5 h-5" />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-auto p-6 space-y-6">
          {/* Canvas Preview */}
          <div
            ref={canvasContainerRef}
            className="relative bg-black rounded-lg overflow-hidden"
            style={{ aspectRatio: "16/9" }}
          >
            <canvas
              ref={canvasRef}
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
            <span className="text-sm text-muted-foreground w-12">Zoom:</span>
            <button
              onClick={zoomOut}
              className="p-2 hover:bg-secondary rounded-lg transition-colors"
              disabled={!imageLoaded}
            >
              <ZoomOut className="w-4 h-4" />
            </button>
            <input
              type="range"
              min="10"
              max="500"
              value={zoomPercent}
              onChange={(e) => setZoom(Number(e.target.value) / 100)}
              className="flex-1 h-2 bg-secondary rounded-lg appearance-none cursor-pointer"
              disabled={!imageLoaded}
            />
            <button
              onClick={zoomIn}
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
            <label className="text-sm font-medium">Source:</label>
            <div className="flex flex-wrap gap-2">
              {/* Upload Custom Button */}
              <button
                onClick={handleUploadCustom}
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
              {thumbnailOptions.map((path, index) => (
                <button
                  key={path}
                  onClick={() => handleSelectAiOption(index)}
                  className={`relative aspect-video w-24 rounded-lg border-2 overflow-hidden transition-colors ${
                    sourceType === "ai" && selectedAiIndex === index
                      ? "border-primary ring-2 ring-primary/30"
                      : "border-transparent hover:border-muted-foreground/30"
                  }`}
                >
                  <LocalImage
                    path={path}
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
            <label className="flex items-center gap-3 cursor-pointer">
              <input
                type="checkbox"
                checked={applyLogoOverlay}
                onChange={(e) => setApplyLogoOverlay(e.target.checked)}
                className="w-4 h-4 rounded border-input"
              />
              <span className="text-sm">Include logo overlay</span>
            </label>
          </div>

          {/* Error */}
          {error && (
            <div className="p-3 bg-destructive/10 text-destructive rounded-lg text-sm">
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
            disabled={isSaving || !imageLoaded}
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
