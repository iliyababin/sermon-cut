import type { Dispatch, RefObject } from "react";
import { AlertCircle, AlertTriangle, CheckCircle, ExternalLink } from "lucide-react";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import { isStuckCommit, revealCommitBackup } from "@/lib/tauri";
import type { VideoDetail } from "@/lib/types";
import { ErrorBanner } from "../ErrorBanner";
import { UploadWarnings } from "../UploadWarnings";
import type { ReviewEvent, ReviewPhase } from "./reviewMachine";

interface ReviewBannersProps {
  video: VideoDetail;
  phase: ReviewPhase;
  dispatch: Dispatch<ReviewEvent>;
  uploadReady: boolean;
  handleUpload: () => void;
  updatingMetadata: boolean;
  metadataNotice: string | null;
  handleUpdateMetadata: () => void;
  canForgetEarlier: boolean;
  handleForgetEarlier: () => void;
  trimmedHere: boolean;
  trimPanel: RefObject<HTMLDivElement | null>;
  saveError: string | null;
  forgetError: string | null;
  setForgetError: (error: string | null) => void;
  actionError: string | null;
  setActionError: (error: string | null) => void;
  youtubeError: string | null;
  loadYoutube: () => void;
  renderError: string | null;
  renderProgress: string;
  retryRenders: () => void;
}

export function ReviewBanners({
  video,
  phase,
  dispatch,
  uploadReady,
  handleUpload,
  updatingMetadata,
  metadataNotice,
  handleUpdateMetadata,
  canForgetEarlier,
  handleForgetEarlier,
  trimmedHere,
  trimPanel,
  saveError,
  forgetError,
  setForgetError,
  actionError,
  setActionError,
  youtubeError,
  loadYoutube,
  renderError,
  renderProgress,
  retryRenders,
}: ReviewBannersProps) {
  return (
    <>
      {/* Upload Success Banner */}
      {phase.kind === "done" && (
        <div className="mx-4 mt-4 p-4 bg-green-100 dark:bg-green-900/30 rounded-lg">
          <div className="flex items-center justify-between">
            <div className="flex flex-col gap-2">
              <div className="flex items-center gap-2">
                <CheckCircle className="w-5 h-5 text-green-600" />
                <span className="font-medium text-green-800 dark:text-green-400">
                  {phase.earlier?.new_cut
                    ? "The previous cut is already on YouTube"
                    : phase.earlier
                      ? "Already on YouTube"
                      : phase.beforeCancel
                        ? "Uploaded to YouTube: it finished before it could be cancelled"
                        : "Uploaded to YouTube"}
                </span>
              </div>
            </div>
            <div className="flex items-center gap-2">
              {!phase.earlier && (
                <button
                  onClick={() => dispatch({ type: "uploadAgain" })}
                  className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
                >
                  Upload Again
                </button>
              )}
              <button
                onClick={() => openUrl(phase.url)}
                className="flex items-center gap-2 px-4 py-2 bg-green-600 text-white rounded-lg hover:bg-green-700 transition-colors"
              >
                Open on YouTube
                <ExternalLink className="w-4 h-4" />
              </button>
            </div>
          </div>
          {phase.earlier && (
            <div role="status" className="mt-3 text-sm text-green-900 dark:text-green-300 flex flex-col gap-2">
              <p>
                {phase.earlier.new_cut
                  ? "An upload of the cut before your last trim had already finished, so that is the video on YouTube. The new cut hasn't been uploaded."
                  : "An earlier upload of this video had already finished, with the title and description it had then. Nothing was uploaded again."}
              </p>
              <div className="flex flex-wrap items-center gap-2">
                {!phase.earlier.new_cut && phase.youtubeId && (
                  <button
                    onClick={handleUpdateMetadata}
                    disabled={updatingMetadata}
                    className="px-3 py-1.5 bg-green-600 text-white rounded-lg hover:bg-green-700 disabled:opacity-50 transition-colors"
                  >
                    {updatingMetadata ? "Updating…" : "Update title & description on that video"}
                  </button>
                )}
                <button
                  onClick={() => dispatch({ type: "uploadNewCopy", ready: uploadReady })}
                  disabled={!uploadReady || updatingMetadata}
                  className="px-3 py-1.5 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
                >
                  {phase.earlier.new_cut ? "Upload the new cut as a new video" : "Upload a new copy"}
                </button>
                <button
                  onClick={() => dispatch({ type: "dismissEarlier" })}
                  disabled={updatingMetadata}
                  className="px-3 py-1.5 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
                >
                  Close
                </button>
              </div>
            </div>
          )}
          {metadataNotice && (
            <p role="status" className="mt-3 text-sm text-green-900 dark:text-green-300">
              {metadataNotice}
            </p>
          )}
          {phase.warnings.length > 0 && (
            <div className="mt-3">
              <UploadWarnings
                videoId={video.id}
                warnings={phase.warnings}
                onChange={(warnings) => dispatch({ type: "warnings", warnings })}
              />
            </div>
          )}
        </div>
      )}

      {phase.kind === "cancelled" && (
        <div className="mx-4 mt-4 p-3 bg-muted rounded-lg text-sm text-muted-foreground">
          {phase.maybeFinished
            ? "Upload cancelled, but it may have finished just before. Uploading again checks with YouTube first."
            : "Upload cancelled. Upload again to continue where it stopped."}
        </div>
      )}

      {/* Upload Error Banner */}
      {phase.kind === "error" && (
        <div className="mx-4 mt-4 p-4 bg-destructive/10 rounded-lg flex items-center justify-between">
          <div className="flex items-center gap-3">
            <AlertCircle className="w-6 h-6 text-destructive" />
            <span className="text-destructive">{phase.message}</span>
          </div>
          <div className="flex items-center gap-2">
            {canForgetEarlier && (
              <button
                onClick={handleForgetEarlier}
                disabled={!uploadReady}
                className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 transition-colors"
              >
                Forget the earlier upload attempt
              </button>
            )}
            <button
              onClick={handleUpload}
              disabled={!uploadReady}
              className="px-4 py-2 bg-red-600 text-white rounded-lg hover:bg-red-700 disabled:opacity-50 transition-colors"
            >
              Retry
            </button>
          </div>
        </div>
      )}

      {(video.transcription?.analysis_error || video.transcription?.no_sermon_reason) && !trimmedHere && (
        <div
          role="status"
          className="mx-4 mt-4 p-3 bg-yellow-100 text-yellow-800 dark:bg-yellow-900/30 dark:text-yellow-300 rounded-lg text-sm flex items-center gap-2"
        >
          <AlertTriangle className="w-4 h-4 shrink-0" />
          <span className="flex-1">
            Couldn't find the sermon automatically — set the start and end below
            {video.transcription?.no_sermon_reason && (
              <span className="block text-xs opacity-80">{video.transcription.no_sermon_reason}</span>
            )}
          </span>
          <button
            onClick={() => trimPanel.current?.scrollIntoView?.({ behavior: "smooth" })}
            className="px-3 py-1 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
          >
            Set start and end
          </button>
        </div>
      )}
      {saveError && <ErrorBanner className="mx-4 mt-4" message={saveError} />}
      {forgetError && <ErrorBanner className="mx-4 mt-4" message={forgetError} onDismiss={() => setForgetError(null)} />}
      {actionError && (
        <ErrorBanner
          className="mx-4 mt-4"
          message={actionError}
          onReveal={isStuckCommit(actionError) ? () => revealCommitBackup(video.id).catch(() => {}) : undefined}
          onDismiss={() => setActionError(null)}
        />
      )}
      {youtubeError && <ErrorBanner className="mx-4 mt-4" message={youtubeError} onRetry={loadYoutube} />}
      {renderError && !renderProgress && (
        <ErrorBanner
          className="mx-4 mt-4"
          message={`Couldn't update the thumbnails: ${renderError}. Uploading uses the plain image.`}
          onRetry={retryRenders}
        />
      )}
    </>
  );
}
