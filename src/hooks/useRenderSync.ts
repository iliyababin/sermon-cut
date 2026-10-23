import { useEffect, useRef, useState } from "react";
import { errorMessage, getVideo, saveRenderedThumbnail } from "@/lib/tauri";
import type { ThumbnailOption, ThumbnailRender } from "@/lib/types";
import { TEMPLATE_VERSION } from "@/templates";

export type RenderMap = Record<string, { path: string; key: string }>;

export function rendersById(renders: ThumbnailRender[]): RenderMap {
  return Object.fromEntries(renders.map((r) => [r.source, { path: r.path, key: r.render_key }]));
}

export const RENDER_DEBOUNCE_MS = 1500;

interface RenderSyncArgs {
  videoId: string;
  options: ThumbnailOption[];
  /** The current render key of each option, by id. */
  keys: Record<string, string>;
  /** Whether renders can be made now (a title and a speaker). */
  enabled: boolean;
  /** The stored renders the screen opened with. */
  initial: ThumbnailRender[];
  /** Draw the template over one option with the inputs `keys` were made
   * from; resolves with the JPEG bytes. */
  render: (option: ThumbnailOption) => Promise<Uint8Array>;
  /** Runs before each render is saved (e.g. saving the inputs it was made
   * with, which the backend checks the key against). */
  beforeSave?: () => Promise<unknown>;
}

/**
 * Keeps each thumbnail option's template render current: renders the stale
 * ones (debounced while typing) and saves them through the backend, which
 * records each one in video.json as it saves it and deletes the render it
 * replaced.
 *
 * So a render the backend saved is never dropped here: its result is taken
 * even when the run was cancelled meanwhile (cancelling only stops the next
 * render from starting), and when a run ends the stored renders are read
 * back. Nothing is written once the screen is gone or shows another video.
 */
export function useRenderSync({ videoId, options, keys, enabled, initial, render, beforeSave }: RenderSyncArgs) {
  const [renders, setRendersState] = useState<RenderMap>(() => rendersById(initial));
  const [progress, setProgress] = useState("");
  const [error, setError] = useState<string | null>(null);
  // Bumped by retry() to render the stale thumbnails again.
  const [attempt, setAttempt] = useState(0);

  const mounted = useRef(true);
  const shownVideo = useRef(videoId);
  shownVideo.current = videoId;
  const isShown = (id: string) => mounted.current && shownVideo.current === id;
  // Bumped on every local change to `renders`, so a read-back that started
  // before it doesn't overwrite it.
  const changes = useRef(0);
  // Each run's number; the last one to start rendering owns the progress text.
  const runs = useRef(0);
  const progressOwner = useRef(0);
  const beforeSaveRef = useRef(beforeSave);
  beforeSaveRef.current = beforeSave;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const setRenders = (next: RenderMap | ((prev: RenderMap) => RenderMap)) => {
    changes.current++;
    setRendersState(next);
  };

  const stale = enabled ? options.filter((o) => renders[o.id]?.key !== keys[o.id]) : [];
  const staleIds = stale.map((o) => o.id).join("|");

  useEffect(() => {
    if (stale.length === 0) return;
    let cancelled = false;
    const id = videoId;
    const run = ++runs.current;
    const todo = stale.map((o) => ({ option: o, key: keys[o.id] }));
    // As of now, so each render matches the key it's saved with.
    const draw = render;

    // Debounced so typing in the title field doesn't start a render per
    // keystroke; right away when nothing has been rendered yet.
    const delay = Object.keys(renders).length === 0 ? 0 : RENDER_DEBOUNCE_MS;
    const timeout = setTimeout(async () => {
      progressOwner.current = run;
      let saved = false;
      try {
        if (isShown(id)) setError(null);
        for (let i = 0; i < todo.length && !cancelled; i++) {
          if (isShown(id)) setProgress(`Updating thumbnails ${i + 1}/${todo.length}...`);
          const { option, key } = todo[i];
          const bytes = await draw(option);
          if (cancelled) break;
          await beforeSaveRef.current?.();
          if (cancelled) break;
          const path = await saveRenderedThumbnail(id, option.id, key, TEMPLATE_VERSION, bytes);
          saved = true;
          // Taken even if cancelled meanwhile: the backend has recorded it
          // and deleted the render it replaced.
          if (isShown(id)) setRenders((prev) => ({ ...prev, [option.id]: { path, key } }));
        }
      } catch (err) {
        console.error("Failed to render thumbnails:", err);
        if (!cancelled && isShown(id)) setError(errorMessage(err, "Couldn't update the thumbnails"));
      } finally {
        if (isShown(id) && progressOwner.current === run) setProgress("");
        if (saved && isShown(id)) {
          const seen = changes.current;
          getVideo(id)
            .then((fresh) => {
              if (isShown(id) && changes.current === seen) setRenders(rendersById(fresh.renders));
            })
            .catch((err) => console.error("Failed to read back the renders:", err));
        }
      }
    }, delay);

    return () => {
      cancelled = true;
      clearTimeout(timeout);
    };
    // staleIds + keys capture everything a render depends on.
  }, [staleIds, keys, videoId, attempt]);

  return {
    renders,
    /** Replace the renders with what the library has (e.g. after regenerating). */
    replaceRenders: (list: ThumbnailRender[]) => setRenders(rendersById(list)),
    stale,
    progress,
    error,
    retry: () => setAttempt((n) => n + 1),
  };
}
