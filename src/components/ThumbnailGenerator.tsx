import { useState, useEffect, useRef } from "react";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  ImagePlus,
  Loader2,
  Download,
  RefreshCw,
  ZoomIn,
  ZoomOut,
  Maximize2,
  RotateCcw,
} from "lucide-react";
import { LocalImage } from "./LocalImage";
import { useCanvasEditor } from "@/hooks/useCanvasEditor";
import { getDefaultTemplate } from "@/templates";
import { renderSingleThumbnail } from "@/lib/thumbnailRenderer";
import { splitTitleIntoLines } from "@/lib/utils";
import {
  readImageBase64,
  cropStandaloneThumbnail,
  saveStandaloneThumbnail,
  copyFile,
} from "@/lib/tauri";

export function ThumbnailGenerator() {
  const [imagePath, setImagePath] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [speakerName, setSpeakerName] = useState("");
  const [singleLine, setSingleLine] = useState(false);
  const [rendering, setRendering] = useState(false);
  const [savedPath, setSavedPath] = useState<string | null>(null);
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

  // Load image into canvas when imagePath changes
  useEffect(() => {
    if (!imagePath) return;
    (async () => {
      try {
        const base64 = await readImageBase64(imagePath);
        loadImage(base64);
        setError(null);
      } catch (err) {
        console.error("Failed to load image:", err);
        setError("Failed to load image");
      }
    })();
  }, [imagePath, loadImage]);

  const handlePickImage = async () => {
    const selected = await open({
      multiple: false,
      filters: [{ name: "Images", extensions: ["jpg", "jpeg", "png", "webp"] }],
    });
    if (selected) {
      setImagePath(selected);
      setSavedPath(null);
    }
  };

  const handleGenerate = async () => {
    if (!imagePath || !title.trim()) return;

    const cropRect = getCropRect();
    if (!cropRect) {
      setError("No image loaded for cropping");
      return;
    }

    setRendering(true);
    setError(null);
    setSavedPath(null);

    try {
      // 1. Crop the image server-side
      const croppedPath = await cropStandaloneThumbnail(imagePath, cropRect);

      // 2. Render the template overlay on the cropped image
      const template = getDefaultTemplate();
      const titleLines = singleLine ? [title.trim()] : splitTitleIntoLines(title.trim());
      const imageData = await renderSingleThumbnail(
        template,
        croppedPath,
        titleLines,
        speakerName.trim(),
      );

      // 3. Save the final composited thumbnail (not tied to a library video)
      const path = await saveStandaloneThumbnail(imageData);
      setSavedPath(path);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRendering(false);
    }
  };

  const handleSaveAs = async () => {
    if (!savedPath) return;

    const destination = await save({
      defaultPath: "thumbnail.jpg",
      filters: [{ name: "JPEG Image", extensions: ["jpg", "jpeg"] }],
    });

    if (destination) {
      try {
        await copyFile(savedPath, destination);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    }
  };

  const canGenerate = !!imagePath && !!title.trim() && !rendering && imageLoaded;
  const zoomPercent = Math.round(zoom * 100);

  return (
    <div className="p-6 max-w-5xl mx-auto">
      <h2 className="text-2xl font-bold mb-6">Thumbnail Generator</h2>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        {/* Left column: form */}
        <div className="space-y-4">
          {/* Background image picker + crop canvas */}
          <div>
            <label className="block text-sm font-medium mb-1">Background Image</label>
            {imagePath ? (
              <>
                {/* Canvas crop editor */}
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
                      <Loader2 className="w-5 h-5 animate-spin text-muted-foreground" />
                    </div>
                  )}
                </div>

                {/* Zoom controls */}
                <div className="flex items-center gap-3 mt-2">
                  <button
                    onClick={zoomOut}
                    className="p-1.5 hover:bg-secondary rounded transition-colors"
                    disabled={!imageLoaded}
                  >
                    <ZoomOut className="w-4 h-4" />
                  </button>
                  <input
                    type="range"
                    min="100"
                    max="500"
                    value={zoomPercent}
                    onChange={(e) => setZoom(Number(e.target.value) / 100)}
                    className="flex-1 h-2 bg-secondary rounded-lg appearance-none cursor-pointer"
                    disabled={!imageLoaded}
                  />
                  <button
                    onClick={zoomIn}
                    className="p-1.5 hover:bg-secondary rounded transition-colors"
                    disabled={!imageLoaded}
                  >
                    <ZoomIn className="w-4 h-4" />
                  </button>
                  <span className="text-xs text-muted-foreground w-12 text-right">
                    {zoomPercent}%
                  </span>
                </div>

                {/* Pan controls + change image */}
                <div className="flex items-center gap-2 mt-1">
                  <button
                    onClick={center}
                    disabled={!imageLoaded}
                    className="flex items-center gap-1.5 px-2.5 py-1.5 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors text-sm"
                  >
                    <Maximize2 className="w-3.5 h-3.5" />
                    Center
                  </button>
                  <button
                    onClick={reset}
                    disabled={!imageLoaded}
                    className="flex items-center gap-1.5 px-2.5 py-1.5 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors text-sm"
                  >
                    <RotateCcw className="w-3.5 h-3.5" />
                    Reset
                  </button>
                  <button
                    onClick={handlePickImage}
                    className="ml-auto flex items-center gap-1.5 px-2.5 py-1.5 text-sm text-muted-foreground hover:text-foreground transition-colors"
                  >
                    <ImagePlus className="w-3.5 h-3.5" />
                    Change Image
                  </button>
                </div>
              </>
            ) : (
              <button
                onClick={handlePickImage}
                className="w-full border-2 border-dashed border-border rounded-lg p-4 hover:border-primary/50 transition-colors flex flex-col items-center justify-center gap-2 text-muted-foreground hover:text-foreground"
                style={{ aspectRatio: "16/9" }}
              >
                <ImagePlus className="w-8 h-8" />
                <span className="text-sm">Click to select an image</span>
              </button>
            )}
          </div>

          {/* Title */}
          <div>
            <label className="block text-sm font-medium mb-1">Title</label>
            <input
              type="text"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="Enter sermon title..."
              className="w-full px-3 py-2 rounded-lg border border-border bg-background focus:outline-none focus:ring-2 focus:ring-primary"
            />
            <div className="flex items-center justify-between mt-1">
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={singleLine}
                  onChange={(e) => setSingleLine(e.target.checked)}
                  className="rounded"
                />
                Single line
              </label>
              {title.trim() && (
                <p className="text-xs text-muted-foreground">
                  {singleLine ? title.trim() : splitTitleIntoLines(title.trim()).join(" / ")}
                </p>
              )}
            </div>
          </div>

          {/* Speaker Name */}
          <div>
            <label className="block text-sm font-medium mb-1">Speaker Name</label>
            <input
              type="text"
              value={speakerName}
              onChange={(e) => setSpeakerName(e.target.value)}
              placeholder="Enter speaker name..."
              className="w-full px-3 py-2 rounded-lg border border-border bg-background focus:outline-none focus:ring-2 focus:ring-primary"
            />
          </div>

          {/* Buttons */}
          <div className="flex gap-2">
            <button
              onClick={handleGenerate}
              disabled={!canGenerate}
              className="flex-1 px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed flex items-center justify-center gap-2"
            >
              {rendering ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Rendering...
                </>
              ) : savedPath ? (
                <>
                  <RefreshCw className="w-4 h-4" />
                  Regenerate
                </>
              ) : (
                "Generate"
              )}
            </button>

            {savedPath && (
              <button
                onClick={handleSaveAs}
                className="px-4 py-2 border border-border rounded-lg hover:bg-secondary flex items-center gap-2"
              >
                <Download className="w-4 h-4" />
                Save As...
              </button>
            )}
          </div>

          {error && (
            <p className="text-sm text-destructive">{error}</p>
          )}
        </div>

        {/* Right column: preview */}
        <div>
          <label className="block text-sm font-medium mb-1">Preview</label>
          <div className="aspect-video bg-muted rounded-lg overflow-hidden flex items-center justify-center">
            {savedPath ? (
              <LocalImage
                path={savedPath}
                alt="Generated thumbnail"
                className="w-full h-full object-contain"
              />
            ) : (
              <p className="text-muted-foreground text-sm">
                {imagePath ? "Click Generate to preview" : "Select an image to get started"}
              </p>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
