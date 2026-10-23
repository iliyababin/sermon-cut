import type { EarlierUpload, UploadWarning } from "@/lib/types";

/**
 * The review screen's upload flow as a reducer:
 *
 *   editing → confirming → uploading → done | error | cancelled
 *
 * Every step is a state, so a second click while one is in progress has
 * nothing to act on: Upload only starts from editing, error or cancelled.
 * Whether the thumbnails are up to date is derived by the screen and passed
 * in with the click (`ready`), never stored here.
 */

export type ReviewPhase =
  | { kind: "editing" }
  /** Asking to confirm the upload (title, speaker, privacy, and whether
   * it was uploaded before). `back`: where a "no" returns to (the
   * earlier-upload notice), editing when unset. */
  | { kind: "confirming"; back?: ReviewPhase }
  /** `request` identifies the upload this screen started (the screen runs
   * it once per request); null when it was started before the screen
   * opened and is only being followed. */
  | { kind: "uploading"; progress: number; request: number | null }
  /** `warnings`: what didn't work after the upload (thumbnail, captions,
   * playlist). `earlier`: the video is an earlier upload that had finished
   * (the new one wasn't made), until the notice about it is closed.
   * `beforeCancel`: it finished before a cancel could stop it. */
  | {
      kind: "done";
      url: string;
      youtubeId: string | null;
      warnings: UploadWarning[];
      earlier: EarlierUpload | null;
      beforeCancel: boolean;
    }
  | { kind: "error"; message: string }
  /** `maybeFinished`: cancelled during the last chunk, and whether it
   * arrived is unknown; the next upload checks first. */
  | { kind: "cancelled"; maybeFinished: boolean };

export interface ReviewState {
  phase: ReviewPhase;
  /** Uploads of this video so far, for the confirm question. */
  uploadCount: number;
  nextRequest: number;
  /** Cancel was clicked for the running upload; waiting for it to stop. */
  cancelling: boolean;
}

export type ReviewEvent =
  /** Upload clicked. Ignored unless `ready` (thumbnails current, speaker set). */
  | { type: "upload"; ready: boolean }
  | { type: "confirm" }
  | { type: "cancelConfirm" }
  | { type: "progress"; percent: number }
  | {
      type: "succeeded";
      url: string;
      youtubeId?: string | null;
      warnings?: UploadWarning[];
      earlier?: EarlierUpload | null;
      beforeCancel?: boolean;
    }
  | { type: "failed"; message: string }
  | { type: "cancelled"; maybeFinished?: boolean }
  /** Cancel clicked during an upload. */
  | { type: "cancelRequested" }
  /** The done upload's warnings changed (a thumbnail was set again). */
  | { type: "warnings"; warnings: UploadWarning[] }
  /** Dismiss the success banner to upload again. */
  | { type: "uploadAgain" }
  /** Close the notice about an earlier upload, keeping its link. */
  | { type: "dismissEarlier" }
  /** "Upload a new copy" / "Upload the new cut": straight to the confirm
   * question (which says it makes a separate video). */
  | { type: "uploadNewCopy"; ready: boolean };

export interface InitialReview {
  status: string;
  uploadProgress: number | null;
  uploads: { url: string; video_id?: string | null; warnings?: UploadWarning[] }[];
}

export function initialReviewState(video: InitialReview): ReviewState {
  const last = video.uploads[video.uploads.length - 1];
  let phase: ReviewPhase = { kind: "editing" };
  if (video.status === "uploading") {
    phase = { kind: "uploading", progress: video.uploadProgress ?? 0, request: null };
  } else if (video.status === "done" && last) {
    phase = {
      kind: "done",
      url: last.url,
      youtubeId: last.video_id ?? null,
      warnings: last.warnings ?? [],
      earlier: null,
      beforeCancel: false,
    };
  }
  return { phase, uploadCount: video.uploads.length, nextRequest: 1, cancelling: false };
}

/** Whether Upload can be clicked in this phase. */
export function canStartUpload(phase: ReviewPhase): boolean {
  return phase.kind === "editing" || phase.kind === "error" || phase.kind === "cancelled";
}

function startUploading(state: ReviewState): ReviewState {
  return {
    ...state,
    phase: { kind: "uploading", progress: 0, request: state.nextRequest },
    nextRequest: state.nextRequest + 1,
    cancelling: false,
  };
}

export function reviewReducer(state: ReviewState, event: ReviewEvent): ReviewState {
  const { phase } = state;
  switch (event.type) {
    case "upload":
      if (!canStartUpload(phase) || !event.ready) return state;
      // Always confirmed first: an upload can't be taken back.
      return { ...state, phase: { kind: "confirming" } };
    case "confirm":
      return phase.kind === "confirming" ? startUploading(state) : state;
    case "cancelConfirm":
      return phase.kind === "confirming" ? { ...state, phase: phase.back ?? { kind: "editing" } } : state;
    case "progress":
      // Progress for an upload this screen didn't start (reopened mid-way,
      // or started from elsewhere) is followed too.
      if (phase.kind === "uploading") {
        return { ...state, phase: { ...phase, progress: Math.max(phase.progress, event.percent) } };
      }
      if (phase.kind === "editing" || phase.kind === "error" || phase.kind === "cancelled") {
        return { ...state, phase: { kind: "uploading", progress: event.percent, request: null }, cancelling: false };
      }
      return state;
    case "succeeded":
      // Both the command's result and the final event report success;
      // only the first counts.
      if (phase.kind !== "uploading") return state;
      return {
        ...state,
        phase: {
          kind: "done",
          url: event.url,
          youtubeId: event.youtubeId ?? null,
          warnings: event.warnings ?? [],
          earlier: event.earlier ?? null,
          beforeCancel: event.beforeCancel ?? false,
        },
        uploadCount: state.uploadCount + 1,
        cancelling: false,
      };
    case "failed":
      return phase.kind === "uploading"
        ? { ...state, phase: { kind: "error", message: event.message }, cancelling: false }
        : state;
    case "cancelled":
      return phase.kind === "uploading"
        ? { ...state, phase: { kind: "cancelled", maybeFinished: event.maybeFinished ?? false }, cancelling: false }
        : state;
    case "cancelRequested":
      return phase.kind === "uploading" && !state.cancelling ? { ...state, cancelling: true } : state;
    case "warnings":
      return phase.kind === "done" ? { ...state, phase: { ...phase, warnings: event.warnings } } : state;
    case "uploadAgain":
      return phase.kind === "done" ? { ...state, phase: { kind: "editing" } } : state;
    case "dismissEarlier":
      return phase.kind === "done" ? { ...state, phase: { ...phase, earlier: null } } : state;
    case "uploadNewCopy":
      return phase.kind === "done" && event.ready ? { ...state, phase: { kind: "confirming", back: phase } } : state;
  }
}

export interface ShortcutKey {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  /** Focus is in a text field, select or other editable element. */
  inField: boolean;
  /** An input method is composing text (the key belongs to it). */
  composing: boolean;
  /** A dialog or the thumbnail editor is open. */
  blocked: boolean;
}

/**
 * The review screen's keyboard shortcuts: ⌘Enter (Ctrl+Enter) uploads and
 * Esc goes back. Nothing happens while a dialog or the editor is open, or
 * while an input method is composing. In a field Esc only leaves the field
 * ("blur"), but ⌘Enter still uploads, since it never types a newline.
 */
export function reviewShortcut(e: ShortcutKey): "upload" | "back" | "blur" | null {
  if (e.blocked || e.composing) return null;
  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) return "upload";
  if (e.key === "Escape" && !e.metaKey && !e.ctrlKey) return e.inField ? "blur" : "back";
  return null;
}
