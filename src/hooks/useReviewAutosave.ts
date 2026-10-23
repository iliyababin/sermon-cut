import { useEffect, useRef, useState } from "react";
import { useBeforeLeave } from "@/lib/beforeLeave";
import { errorMessage, saveReview } from "@/lib/tauri";
import type { VideoDetail } from "@/lib/types";

const AUTOSAVE_MS = 1000;

interface ReviewAutosaveArgs {
  video: VideoDetail;
  title: string;
  description: string;
  preacher: string;
  singleLine: boolean;
  selectedSource: string | null;
  setSaveError: (error: string | null) => void;
}

export function useReviewAutosave({
  video,
  title,
  description,
  preacher,
  singleLine,
  selectedSource,
  setSaveError,
}: ReviewAutosaveArgs) {
  // Review autosave status, for "Saving… / Saved".
  const [savesInFlight, setSavesInFlight] = useState(0);
  const [savedOnce, setSavedOnce] = useState(false);

  // Autosave edits (debounced), and flush whatever is pending on close.
  const latest = useRef({ title, description, preacher, singleLine, selectedSource });
  latest.current = { title, description, preacher, singleLine, selectedSource };
  // The render inputs the backend has, which it checks render keys against.
  const savedInputs = useRef({
    title: video.review.title,
    preacher: video.review.preacher,
    singleLine: video.review.single_line ?? false,
  });
  const dirty = useRef(false);
  const firstRender = useRef(true);
  // The save in flight, if any; flushing waits for it, so leaving never
  // races a save that already started.
  const saving = useRef<Promise<boolean>>(Promise.resolve(true));
  // Why the last save failed, for whoever is waiting to leave.
  const lastSaveError = useRef<string | null>(null);

  /** Save pending edits; resolves with whether everything is saved. */
  const flush = (): Promise<boolean> => {
    const run = saving.current.then((): boolean | Promise<boolean> => {
      if (!dirty.current) return true;
      dirty.current = false;
      const v = latest.current;
      setSavesInFlight((n) => n + 1);
      return saveReview(
        video.id,
        { title: v.title, description: v.description, preacher: v.preacher, singleLine: v.singleLine },
        v.selectedSource
      )
        .then(() => {
          savedInputs.current = { title: v.title, preacher: v.preacher, singleLine: v.singleLine };
          lastSaveError.current = null;
          setSaveError(null);
          setSavedOnce(true);
          return true;
        })
        .catch((err) => {
          // Still unsaved: the next flush (e.g. Back) tries again.
          dirty.current = true;
          lastSaveError.current = errorMessage(err, "Couldn't save your changes");
          setSaveError(lastSaveError.current);
          return false;
        })
        .finally(() => setSavesInFlight((n) => n - 1));
    });
    saving.current = run;
    return run;
  };

  /** Save the title, speaker and layout now if the backend doesn't have
   * them yet (a render is about to be saved). */
  const saveRenderInputs = async () => {
    const v = latest.current;
    const s = savedInputs.current;
    if (!dirty.current && s.title === v.title && s.preacher === v.preacher && s.singleLine === v.singleLine) return;
    dirty.current = true;
    await flush();
  };

  useEffect(() => {
    if (firstRender.current) {
      firstRender.current = false;
      return;
    }
    dirty.current = true;
    const timer = setTimeout(flush, AUTOSAVE_MS);
    return () => clearTimeout(timer);
  }, [title, description, preacher, singleLine, selectedSource]);

  useEffect(() => () => void flush(), []);

  // Leaving (another screen, another video, closing or quitting the app)
  // waits for the edits to be saved.
  useBeforeLeave(async () => {
    if (!(await flush())) throw new Error(lastSaveError.current ?? "Couldn't save your changes");
  });

  return { flush, saveRenderInputs, lastSaveError, savesInFlight, savedOnce };
}
