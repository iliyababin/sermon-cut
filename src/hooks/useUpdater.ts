import { createContext, useCallback, useContext, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { beginUpdate, endUpdate, errorMessage } from "@/lib/tauri";
import { log } from "@/lib/log";

/** The last update check failed: the next failure isn't logged again
 * (the app checks on every launch and from Settings), only the recovery. */
let checkFailing = false;

interface UpdaterState {
  checked: boolean;
  checking: boolean;
  available: boolean;
  downloading: boolean;
  version: string | null;
  progress: number;
  error: string | null;
}

export type Updater = ReturnType<typeof useUpdaterState>;

/** The one updater App creates, shared by the sidebar and Settings. */
export const UpdaterContext = createContext<Updater | null>(null);

export function useUpdater(): Updater {
  const updater = useContext(UpdaterContext);
  if (!updater) throw new Error("useUpdater needs an UpdaterContext provider");
  return updater;
}

/** The updater's state; App creates it once and shares it via context. */
export function useUpdaterState() {
  const [state, setState] = useState<UpdaterState>({
    checked: false,
    checking: false,
    available: false,
    downloading: false,
    version: null,
    progress: 0,
    error: null,
  });
  const [update, setUpdate] = useState<Update | null>(null);

  const checkForUpdates = useCallback(async () => {
    setState((s) => ({ ...s, checking: true, error: null }));
    try {
      const result = await check();
      if (checkFailing) log("info", "Update check works again");
      checkFailing = false;
      log("info", result ? `Update check: version ${result.version} is available` : "Update check: up to date");
      if (result) {
        setUpdate(result);
        setState((s) => ({
          ...s,
          checked: true,
          checking: false,
          available: true,
          version: result.version,
        }));
      } else {
        setState((s) => ({
          ...s,
          checked: true,
          checking: false,
          available: false,
          version: null,
        }));
      }
      return result;
    } catch (err) {
      if (!checkFailing) log("warn", "Update check failed:", err);
      checkFailing = true;
      setState((s) => ({
        ...s,
        checking: false,
        error: err instanceof Error ? err.message : String(err),
      }));
      return null;
    }
  }, []);

  const downloadAndInstall = useCallback(async () => {
    if (!update) return;
    setState((s) => ({ ...s, downloading: true, progress: 0, error: null }));
    // Refused while something is processing, uploading or downloading;
    // otherwise nothing new can start until the relaunch.
    try {
      await beginUpdate();
    } catch (err) {
      setState((s) => ({ ...s, downloading: false, error: errorMessage(err) }));
      return;
    }
    log("info", `Downloading and installing version ${update.version}`);
    try {
      let totalBytes = 0;
      let downloadedBytes = 0;
      await update.downloadAndInstall((event) => {
        if (event.event === "Started" && event.data.contentLength) {
          totalBytes = event.data.contentLength;
        } else if (event.event === "Progress") {
          downloadedBytes += event.data.chunkLength;
          if (totalBytes > 0) {
            setState((s) => ({
              ...s,
              progress: Math.round((downloadedBytes / totalBytes) * 100),
            }));
          }
        }
      });
      log("info", "Update installed; restarting");
      await relaunch();
    } catch (err) {
      log("error", "Update failed:", err);
      await endUpdate().catch(() => {});
      setState((s) => ({
        ...s,
        downloading: false,
        error: err instanceof Error ? err.message : String(err),
      }));
    }
  }, [update]);

  return {
    ...state,
    checkForUpdates,
    downloadAndInstall,
  };
}
