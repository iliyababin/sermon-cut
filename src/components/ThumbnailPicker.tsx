import { useState } from "react";
import { Image, RefreshCw, Check, Loader2 } from "lucide-react";
import { LocalImage } from "./LocalImage";

interface ThumbnailPickerProps {
  thumbnailOptions: string[];
  processing: boolean;
  onSelect: (path: string) => void;
  onRegenerate: () => void;
}

export function ThumbnailPicker({
  thumbnailOptions,
  processing,
  onSelect,
  onRegenerate,
}: ThumbnailPickerProps) {
  const [selectedIndex, setSelectedIndex] = useState<number>(0);
  const [showDebug, setShowDebug] = useState(false);

  const handleUseSelected = () => {
    if (thumbnailOptions[selectedIndex]) {
      onSelect(thumbnailOptions[selectedIndex]);
    }
  };

  // Get debug path for a thumbnail option
  const getDebugPath = (path: string) => {
    return path.replace(/\.(jpg|jpeg)$/i, "_debug.jpg");
  };

  // Get display path based on debug toggle
  const getDisplayPath = (path: string) => {
    return showDebug ? getDebugPath(path) : path;
  };

  return (
    <div className="max-w-4xl mx-auto">
      <div className="text-center mb-6">
        <Image className="w-12 h-12 mx-auto mb-3 text-muted-foreground" />
        <h3 className="text-xl font-semibold mb-2">Choose Thumbnail</h3>
        <p className="text-muted-foreground">
          Select the best thumbnail for your video from the AI-generated options below.
        </p>

        {/* Debug toggle */}
        <div className="flex items-center justify-center gap-2 mt-3">
          <button
            onClick={() => setShowDebug(false)}
            className={`px-3 py-1.5 rounded-md text-sm font-medium transition-colors ${
              !showDebug
                ? "bg-primary text-primary-foreground"
                : "bg-muted text-muted-foreground hover:text-foreground"
            }`}
          >
            Final
          </button>
          <button
            onClick={() => setShowDebug(true)}
            className={`px-3 py-1.5 rounded-md text-sm font-medium transition-colors ${
              showDebug
                ? "bg-primary text-primary-foreground"
                : "bg-muted text-muted-foreground hover:text-foreground"
            }`}
          >
            Debug (Pose)
          </button>
        </div>
        {showDebug && (
          <p className="text-xs text-muted-foreground mt-2">
            Green = pose skeleton & bounding box, Blue = crop area
          </p>
        )}
      </div>

      {/* Thumbnail Grid */}
      <div className="grid grid-cols-2 md:grid-cols-3 gap-4 mb-6">
        {thumbnailOptions.map((path, index) => (
          <button
            key={`${path}-${showDebug}`}
            onClick={() => setSelectedIndex(index)}
            className={`relative rounded-lg overflow-hidden border-2 transition-all ${
              selectedIndex === index
                ? "border-primary ring-2 ring-primary/20"
                : "border-border hover:border-muted-foreground/50"
            }`}
          >
            <LocalImage
              key={getDisplayPath(path)}
              path={getDisplayPath(path)}
              alt={`Thumbnail option ${index + 1}`}
              className="w-full aspect-video object-cover"
            />

            {/* Selection indicator */}
            {selectedIndex === index && (
              <div className="absolute top-2 right-2 w-6 h-6 bg-primary rounded-full flex items-center justify-center">
                <Check className="w-4 h-4 text-primary-foreground" />
              </div>
            )}

            {/* Option number */}
            <div className="absolute bottom-2 left-2 px-2 py-1 bg-black/60 rounded text-white text-xs font-medium">
              Option {index + 1}
            </div>
          </button>
        ))}
      </div>

      {/* Selected preview */}
      {thumbnailOptions[selectedIndex] && (
        <div className="mb-6 p-4 bg-muted rounded-lg">
          <p className="text-sm font-medium mb-2">
            {showDebug ? "Debug View" : "Selected Thumbnail"}
          </p>
          <div className="rounded-lg overflow-hidden border border-border">
            <LocalImage
              key={getDisplayPath(thumbnailOptions[selectedIndex])}
              path={getDisplayPath(thumbnailOptions[selectedIndex])}
              alt={showDebug ? "Debug view" : "Selected thumbnail"}
              className="w-full h-auto"
            />
          </div>
          <p className="text-xs text-muted-foreground mt-2 font-mono break-all">
            {getDisplayPath(thumbnailOptions[selectedIndex])}
          </p>
        </div>
      )}

      {/* Action buttons */}
      <div className="flex gap-3 justify-center">
        <button
          onClick={onRegenerate}
          disabled={processing}
          className="px-6 py-3 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 flex items-center gap-2 transition-colors"
        >
          {processing ? (
            <>
              <Loader2 className="w-5 h-5 animate-spin" />
              Generating...
            </>
          ) : (
            <>
              <RefreshCw className="w-5 h-5" />
              Regenerate Options
            </>
          )}
        </button>

        <button
          onClick={handleUseSelected}
          disabled={processing || !thumbnailOptions[selectedIndex]}
          className="px-6 py-3 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 disabled:opacity-50 flex items-center gap-2 transition-colors"
        >
          <Check className="w-5 h-5" />
          Use Selected
        </button>
      </div>
    </div>
  );
}
