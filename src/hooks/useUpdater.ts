import { useState, useCallback } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

interface UpdaterState {
  checked: boolean;
  checking: boolean;
  available: boolean;
  downloading: boolean;
  version: string | null;
  progress: number;
  error: string | null;
}

export function useUpdater() {
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
      await relaunch();
    } catch (err) {
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
