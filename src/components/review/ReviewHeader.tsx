import type { RefObject } from "react";
import { ArrowLeft, Loader2, RotateCcw, Youtube } from "lucide-react";
import type { PlaylistInfo, UploadPrivacy, VideoDetail, YouTubeAuthStatus } from "@/lib/types";
import { canStartUpload, type ReviewPhase, type ReviewState } from "./reviewMachine";

interface ReviewHeaderProps {
  video: VideoDetail;
  handleBack: () => void;
  handleReprocess: () => void;
  isReprocessing: boolean;
  phase: ReviewPhase;
  review: ReviewState;
  youtubeAuth: YouTubeAuthStatus | null;
  privacy: UploadPrivacy;
  setPrivacy: (privacy: UploadPrivacy) => void;
  playlistId: string;
  setPlaylistId: (playlistId: string) => void;
  playlists: PlaylistInfo[];
  choicesTouched: RefObject<boolean>;
  handleUpload: () => void;
  uploadReady: boolean;
  uploadBlocker: string | null;
  thumbnailsReady: boolean;
  handleCancelUpload: () => void;
}

export function ReviewHeader({
  video,
  handleBack,
  handleReprocess,
  isReprocessing,
  phase,
  review,
  youtubeAuth,
  privacy,
  setPrivacy,
  playlistId,
  setPlaylistId,
  playlists,
  choicesTouched,
  handleUpload,
  uploadReady,
  uploadBlocker,
  thumbnailsReady,
  handleCancelUpload,
}: ReviewHeaderProps) {
  return (
    <div className="p-4 border-b border-border flex items-center justify-between">
      <div className="flex items-center gap-4">
        <button
          onClick={handleBack}
          aria-label="Back"
          title="Back (Esc)"
          className="p-2 hover:bg-secondary rounded-lg transition-colors"
        >
          <ArrowLeft className="w-5 h-5" />
        </button>
        <div>
          <h2 className="font-semibold">{video.title}</h2>
          <p className="text-sm text-muted-foreground">{video.channel || video.file_name}</p>
        </div>
      </div>

      <div className="flex items-center gap-2">
        <button
          onClick={handleReprocess}
          disabled={isReprocessing || phase.kind === "uploading" || phase.kind === "confirming"}
          title={
            phase.kind === "uploading"
              ? "Wait for the upload to finish"
              : "Re-transcribe and regenerate all suggestions"
          }
          className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 flex items-center gap-2 transition-colors"
        >
          {isReprocessing ? (
            <Loader2 className="w-4 h-4 animate-spin" />
          ) : (
            <RotateCcw className="w-4 h-4" />
          )}
          Reprocess
        </button>
        {youtubeAuth?.is_authenticated && phase.kind !== "done" && (
          <>
            <select
              aria-label="Privacy"
              value={privacy}
              disabled={!canStartUpload(phase)}
              onChange={(e) => {
                choicesTouched.current = true;
                setPrivacy(e.target.value as UploadPrivacy);
              }}
              className="px-3 py-2 border border-input rounded-lg bg-background text-sm disabled:opacity-50"
            >
              <option value="public">Public</option>
              <option value="unlisted">Unlisted</option>
              <option value="private">Private</option>
            </select>
            <select
              aria-label="Playlist"
              value={playlistId}
              disabled={!canStartUpload(phase)}
              onChange={(e) => {
                choicesTouched.current = true;
                setPlaylistId(e.target.value);
              }}
              className="px-3 py-2 border border-input rounded-lg bg-background text-sm max-w-48 disabled:opacity-50"
            >
              <option value="">No playlist</option>
              {playlistId && !playlists.some((p) => p.id === playlistId) && (
                <option value={playlistId}>Default playlist</option>
              )}
              {playlists.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.title}
                </option>
              ))}
            </select>
            <button
              onClick={handleUpload}
              disabled={!canStartUpload(phase) || !uploadReady}
              title={uploadBlocker ?? "Upload (⌘Enter)"}
              aria-describedby={uploadBlocker ? "upload-blocker" : undefined}
              className="px-4 py-2 bg-red-600 text-white rounded-lg hover:bg-red-700 disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2 transition-colors"
            >
              {phase.kind === "uploading" ? (
                <>
                  <Loader2 className="w-4 h-4 animate-spin" />
                  {phase.progress}%
              </>
            ) : !thumbnailsReady ? (
              <>
                <Loader2 className="w-4 h-4 animate-spin" />
                Updating thumbnail…
              </>
            ) : (
              <>
                <Youtube className="w-4 h-4" />
                Upload
              </>
            )}
          </button>
          {uploadBlocker && canStartUpload(phase) && (
            <span id="upload-blocker" role="note" className="text-xs text-muted-foreground max-w-40">
              {uploadBlocker}
            </span>
          )}
          {phase.kind === "uploading" && (
            <button
              onClick={handleCancelUpload}
              disabled={review.cancelling}
              className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
            >
              {review.cancelling ? "Cancelling…" : "Cancel upload"}
            </button>
          )}
          </>
        )}
      </div>
    </div>
  );
}
