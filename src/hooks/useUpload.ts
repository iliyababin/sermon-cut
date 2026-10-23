import { useEffect, useReducer, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  cancelUpload,
  errorMessage,
  forgetUploadSession,
  UPLOAD_CANCELLED,
  UPLOAD_CANCELLED_MAYBE_FINISHED,
  UPLOAD_CHECK_FAILED,
  UPLOAD_UNFINISHED,
  youtubeUpdateMetadata,
  youtubeUploadVideo,
} from "@/lib/tauri";
import { youtubeTitle } from "@/lib/youtube";
import { initialReviewState, reviewReducer } from "@/components/review/reviewMachine";
import type { EarlierUpload, ThumbnailOption, UploadPrivacy, UploadWarning, VideoDetail } from "@/lib/types";

const PRIVACY_LABELS: Record<UploadPrivacy, string> = {
  public: "Public",
  unlisted: "Unlisted",
  private: "Private",
};

interface UploadArgs {
  video: VideoDetail;
  title: string;
  preacher: string;
  privacy: UploadPrivacy;
  playlistId: string;
  youtubeDescription: string;
  keys: Record<string, string>;
  selectedOption: ThumbnailOption | null;
  startTime: number;
  endTime: number;
  uploadReady: boolean;
  flush: () => Promise<boolean>;
  setSaveError: (error: string | null) => void;
}

export function useUpload({
  video,
  title,
  preacher,
  privacy,
  playlistId,
  youtubeDescription,
  keys,
  selectedOption,
  startTime,
  endTime,
  uploadReady,
  flush,
  setSaveError,
}: UploadArgs) {
  // Upload flow (see reviewMachine). The upload runs in the background; if
  // this screen was reopened while one is going, it picks it up from the
  // library's state.
  const [review, dispatch] = useReducer(reviewReducer, video, (v) =>
    initialReviewState({ status: v.status, uploadProgress: v.upload_progress, uploads: v.uploads })
  );
  const phase = review.phase;

  // Listen for upload progress
  useEffect(() => {
    const unlisten = listen<{
      video_id: string;
      progress: number;
      status: string;
      url: string | null;
      youtube_id?: string | null;
      error: string | null;
      warnings?: UploadWarning[] | null;
      earlier?: EarlierUpload | null;
      before_cancel?: boolean;
    }>("youtube-upload-progress", (event) => {
      const e = event.payload;
      if (e.video_id !== video.id) return;
      if (e.status === "done" && e.url) {
        dispatch({
          type: "succeeded",
          url: e.url,
          youtubeId: e.youtube_id ?? null,
          warnings: e.warnings ?? [],
          earlier: e.earlier ?? null,
          beforeCancel: e.before_cancel ?? false,
        });
      } else if (e.status === "cancelled") {
        dispatch({ type: "cancelled", maybeFinished: e.error === UPLOAD_CANCELLED_MAYBE_FINISHED });
      } else if (e.status === "error") {
        dispatch({ type: "failed", message: e.error || "Upload failed" });
      } else {
        dispatch({ type: "progress", percent: e.progress });
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // The upload is confirmed when the machine says so, once, naming what
  // goes up and how.
  useEffect(() => {
    if (phase.kind !== "confirming") return;
    let current = true;
    const again = review.uploadCount > 0;
    const warn = again || !!video.duplicate_upload;
    const lines = [
      `Upload "${youtubeTitle(title)}" by ${preacher.trim()} as ${PRIVACY_LABELS[privacy]}?`,
    ];
    if (again) {
      lines.push("This video has already been uploaded to YouTube. This makes a new, separate YouTube video.");
    }
    if (video.duplicate_upload) {
      lines.push(
        `The same video file is in the library under another name and was already uploaded: ${video.duplicate_upload}`
      );
    }
    ask(lines.join("\n\n"), {
      title: again ? "Upload again" : "Upload to YouTube",
      kind: warn ? "warning" : "info",
      okLabel: again ? "Upload Again" : "Upload",
      cancelLabel: "Cancel",
    })
      .then((yes) => current && dispatch({ type: yes ? "confirm" : "cancelConfirm" }))
      .catch(() => current && dispatch({ type: "cancelConfirm" }));
    return () => {
      current = false;
    };
  }, [phase.kind]);

  // Each upload the machine starts is sent exactly once: save the edits,
  // then hand it to the backend.
  const uploadRequest = phase.kind === "uploading" ? phase.request : null;
  const startedRequest = useRef<number | null>(null);
  useEffect(() => {
    if (uploadRequest === null || startedRequest.current === uploadRequest) return;
    startedRequest.current = uploadRequest;
    setMetadataNotice(null);

    (async () => {
      try {
        await flush();

        const source = selectedOption?.id;
        const outcome = await youtubeUploadVideo(
          video.id,
          youtubeTitle(title),
          youtubeDescription,
          source,
          source ? keys[source] : undefined,
          privacy,
          startTime,   // trim start for caption adjustment
          endTime,     // trim end for caption adjustment
          playlistId
        );
        // The "done" event reports this too; the machine counts it once.
        dispatch({
          type: "succeeded",
          url: outcome.url,
          youtubeId: outcome.video_id,
          warnings: outcome.warnings,
          earlier: outcome.earlier ?? null,
          beforeCancel: outcome.before_cancel ?? false,
        });
      } catch (err) {
        const message = errorMessage(err);
        if (message === UPLOAD_CANCELLED || message === UPLOAD_CANCELLED_MAYBE_FINISHED) {
          dispatch({ type: "cancelled", maybeFinished: message === UPLOAD_CANCELLED_MAYBE_FINISHED });
          return;
        }
        console.error("Upload failed:", err);
        dispatch({ type: "failed", message });
      }
    })();
  }, [uploadRequest]);

  const handleUpload = () => dispatch({ type: "upload", ready: uploadReady });

  // The earlier upload attempt can't be checked with YouTube, again and
  // again (e.g. it was made from another account), or YouTube has all of
  // it but didn't finish it: offer to forget it.
  const [checkFailures, setCheckFailures] = useState(0);
  useEffect(() => {
    if (phase.kind === "error") setCheckFailures((n) => (phase.message === UPLOAD_CHECK_FAILED ? n + 1 : 0));
    else if (phase.kind === "done") setCheckFailures(0);
  }, [phase]);
  const unfinished = phase.kind === "error" && phase.message === UPLOAD_UNFINISHED;
  const canForgetEarlier = checkFailures >= 2 || unfinished;
  const [forgetError, setForgetError] = useState<string | null>(null);
  const handleForgetEarlier = async () => {
    const why = unfinished
      ? "YouTube has the whole video from the earlier upload attempt but hasn't finished it."
      : "Sermon Cut couldn't check the earlier upload attempt with YouTube (for example, it was made while signed in to another account).";
    const yes = await ask(
      `${why}\n\nForget it and start a new upload? If that earlier upload did finish, this makes a second, duplicate video on YouTube.`,
      { title: "Forget the earlier upload attempt", kind: "warning", okLabel: "Forget and Upload Anew", cancelLabel: "Cancel" }
    );
    if (!yes) return;
    try {
      setForgetError(null);
      await forgetUploadSession(video.id);
      setCheckFailures(0);
      handleUpload();
    } catch (err) {
      setForgetError(errorMessage(err, "Couldn't forget the earlier upload attempt"));
    }
  };

  // An earlier upload had finished: give that video the current title and
  // description (videos.update).
  const [updatingMetadata, setUpdatingMetadata] = useState(false);
  const [metadataNotice, setMetadataNotice] = useState<string | null>(null);
  const handleUpdateMetadata = async () => {
    if (phase.kind !== "done" || !phase.youtubeId) return;
    setUpdatingMetadata(true);
    setMetadataNotice(null);
    try {
      await flush();
      await youtubeUpdateMetadata(video.id, phase.youtubeId, youtubeTitle(title), youtubeDescription);
      setMetadataNotice("The title and description on YouTube are updated.");
      dispatch({ type: "dismissEarlier" });
    } catch (err) {
      setMetadataNotice(`Couldn't update the title and description: ${errorMessage(err)}`);
    } finally {
      setUpdatingMetadata(false);
    }
  };

  // The upload stops; its session is kept, so Upload continues it.
  const handleCancelUpload = () => {
    dispatch({ type: "cancelRequested" });
    cancelUpload(video.id).catch((err) => setSaveError(errorMessage(err, "Couldn't cancel the upload")));
  };

  return {
    review,
    dispatch,
    phase,
    handleUpload,
    canForgetEarlier,
    forgetError,
    setForgetError,
    handleForgetEarlier,
    updatingMetadata,
    metadataNotice,
    handleUpdateMetadata,
    handleCancelUpload,
  };
}
