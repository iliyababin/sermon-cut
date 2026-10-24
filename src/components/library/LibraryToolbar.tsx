import { Eye, EyeOff, FolderOpen } from "lucide-react";
import { openLibraryFolder } from "@/lib/tauri";
import type { LibraryVideo, LibraryView } from "@/lib/types";

interface LibraryToolbarProps {
  library: LibraryView;
  nextToReview: LibraryVideo | undefined;
  onVideoSelect: (video: LibraryVideo) => void;
  hiddenCount: number;
  showHidden: boolean;
  setShowHidden: (show: boolean) => void;
  run: (action: () => Promise<unknown>) => void;
}

export function LibraryToolbar({
  library,
  nextToReview,
  onVideoSelect,
  hiddenCount,
  showHidden,
  setShowHidden,
  run,
}: LibraryToolbarProps) {
  return (
    <div className="flex items-start justify-between gap-4 mb-6">
      <div className="min-w-0">
        <h2 className="text-2xl font-bold">Video Library</h2>
        <p className="text-sm text-muted-foreground truncate" title={library.folder}>
          {library.folder}
        </p>
      </div>
      <div className="flex items-center gap-2 shrink-0">
        {nextToReview && (
          <button
            onClick={() => onVideoSelect(nextToReview)}
            title={`Open "${nextToReview.title}", the video waiting longest`}
            className="flex items-center gap-2 px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition-colors"
          >
            <Eye className="w-5 h-5" />
            Review next
          </button>
        )}
        {hiddenCount > 0 && (
          <button
            onClick={() => setShowHidden(!showHidden)}
            className="flex items-center gap-2 px-3 py-2 text-sm text-muted-foreground rounded-lg hover:bg-secondary transition-colors"
          >
            {showHidden ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
            {showHidden ? "Hide hidden" : `Show hidden (${hiddenCount})`}
          </button>
        )}
        <button
          onClick={() => run(openLibraryFolder)}
          disabled={!library.available}
          className="flex items-center gap-2 px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
        >
          <FolderOpen className="w-5 h-5" />
          Open Folder
        </button>
      </div>
    </div>
  );
}
