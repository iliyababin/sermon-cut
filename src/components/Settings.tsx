import { useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { getSettings, saveSettings, nextSettingsSeq, getAppFolders, checkLibraryFolder, youtubeGetAuthStatus, youtubeSignIn, youtubeSignOut, youtubeListPlaylists, errorMessage } from "@/lib/tauri";
import type { Settings as SettingsType, YouTubeAuthStatus, PlaylistInfo } from "@/lib/types";
import { settingsPatch } from "@/lib/settingsPatch";
import { Loader2, CheckCircle } from "lucide-react";
import { useUpdater } from "@/hooks/useUpdater";
import { ErrorBanner } from "./ErrorBanner";
import type { Field } from "./settings/fields";
import { SettingsLibrary } from "./settings/SettingsLibrary";
import { SettingsThumbnail } from "./settings/SettingsThumbnail";
import { SettingsYouTube } from "./settings/SettingsYouTube";
import { SettingsProcessing } from "./settings/SettingsProcessing";
import { SettingsApplication } from "./settings/SettingsApplication";

/** How long typing pauses before a field is saved. */
export const AUTOSAVE_DELAY_MS = 600;

export function Settings() {
  const [settings, setSettings] = useState<SettingsType | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [folders, setFolders] = useState<{ settings: string; tools: string | null } | null>(null);
  const [youtubeAuth, setYoutubeAuth] = useState<YouTubeAuthStatus | null>(null);
  const [youtubeLoading, setYoutubeLoading] = useState(false);
  const [youtubeError, setYoutubeError] = useState<string | null>(null);
  const [playlists, setPlaylists] = useState<PlaylistInfo[]>([]);
  const [playlistsLoading, setPlaylistsLoading] = useState(false);
  const [playlistsError, setPlaylistsError] = useState<string | null>(null);
  const [resetConfirm, setResetConfirm] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [clearConfirm, setClearConfirm] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [folderWarnings, setFolderWarnings] = useState<string[]>([]);
  const [appVersion, setAppVersion] = useState("");
  const updater = useUpdater();

  // Autosave: each field is saved on its own, AUTOSAVE_DELAY_MS after the
  // last change to it (or at once for picks and on blur). Every save gets a
  // newer seq, so the backend ignores a field's value that arrives late.
  const [fieldErrors, setFieldErrors] = useState<Partial<Record<Field, string>>>({});
  const [savesInFlight, setSavesInFlight] = useState(0);
  const [savedOnce, setSavedOnce] = useState(false);
  // As last confirmed by the backend (loaded or saved).
  const saved = useRef<SettingsType | null>(null);
  // As last sent, per field; a save sends a field only if it differs from
  // this. Diffing against `saved` instead would drop on→off→on while the
  // "off" is still in flight: the "on" looks unchanged and is never sent.
  const sent = useRef<SettingsType | null>(null);
  const edited = useRef<SettingsType | null>(null);
  // Fields waiting to be saved; null: until flushField (Enter or blur).
  const timers = useRef(new Map<Field, ReturnType<typeof setTimeout> | null>());
  // Per field: the newest save whose answer was taken.
  const newestApplied = useRef(new Map<Field, number>());
  // Per field: the newest save sent.
  const newestSent = useRef(new Map<Field, number>());

  useEffect(() => {
    edited.current = settings;
  }, [settings]);

  useEffect(() => {
    loadSettings();
    loadYoutubeAuth();
    getVersion().then(setAppVersion).catch(() => {});
    const pending = timers.current;
    return () => {
      // Leaving with edits still waiting: save them now.
      for (const [field, timer] of pending) {
        if (timer) clearTimeout(timer);
        void saveField(field);
      }
      pending.clear();
    };
  }, []);

  const libraryFolder = settings?.output_folder ?? "";
  useEffect(() => {
    if (!libraryFolder) {
      setFolderWarnings([]);
      return;
    }
    checkLibraryFolder(libraryFolder).then(setFolderWarnings).catch(() => setFolderWarnings([]));
  }, [libraryFolder]);

  async function loadSettings() {
    try {
      setLoading(true);
      setLoadError(null);
      const [loadedSettings, appFolders] = await Promise.all([
        getSettings(),
        getAppFolders(),
      ]);
      saved.current = loadedSettings;
      sent.current = loadedSettings;
      // Set now, not in the effect: a click can land before effects run.
      edited.current = loadedSettings;
      setSettings(loadedSettings);
      setFolders(appFolders);
    } catch (err) {
      setLoadError(errorMessage(err, "Couldn't load the settings"));
    } finally {
      setLoading(false);
    }
  }

  /** Change one field and save it after `delay` ms (0: now; Infinity: on
   * flushField). */
  function edit<F extends Field>(field: F, value: SettingsType[F], delay = AUTOSAVE_DELAY_MS) {
    setSettings((s) => (s ? { ...s, [field]: value } : s));
    const base = edited.current ?? settings;
    if (base) edited.current = { ...base, [field]: value };
    setFieldErrors(({ [field]: _, ...rest }) => rest as typeof fieldErrors);
    const previous = timers.current.get(field);
    if (previous) clearTimeout(previous);
    timers.current.delete(field);
    if (delay <= 0) {
      void saveField(field);
    } else if (delay === Infinity) {
      timers.current.set(field, null);
    } else {
      timers.current.set(
        field,
        setTimeout(() => {
          timers.current.delete(field);
          void saveField(field);
        }, delay)
      );
    }
  }

  /** Save a field waiting for its timer right away (e.g. on blur). */
  function flushField(field: Field) {
    if (!timers.current.has(field)) return;
    const timer = timers.current.get(field);
    if (timer) clearTimeout(timer);
    timers.current.delete(field);
    void saveField(field);
  }

  async function saveField(field: Field) {
    const last = sent.current;
    const current = edited.current;
    if (!last || !current) return;
    const patch = settingsPatch(last, { ...last, [field]: current[field] });
    if (Object.keys(patch).length === 0) return;
    const seq = nextSettingsSeq();
    const value = current[field];
    sent.current = { ...last, [field]: value };
    newestSent.current.set(field, seq);
    setSavesInFlight((n) => n + 1);
    try {
      const updated = await saveSettings(patch, seq);
      if (seq < (newestApplied.current.get(field) ?? 0)) return;
      newestApplied.current.set(field, seq);
      // Each save owns its field. The answer's other fields may predate a
      // save of theirs that the backend applied first.
      saved.current = { ...(saved.current ?? updated), [field]: updated[field] };
      if (newestSent.current.get(field) === seq && sent.current) {
        sent.current = { ...sent.current, [field]: updated[field] };
      }
      // Show the value as saved (e.g. trimmed), unless it was edited since.
      setSettings((s) =>
        s && s[field] === value ? { ...s, [field]: updated[field] } : s
      );
      setSavedOnce(true);
    } catch (err) {
      setFieldErrors((e) => ({ ...e, [field]: errorMessage(err, "Couldn't save this setting") }));
      // Roll back to the value last confirmed, unless a newer save of this
      // field is on its way or the field was edited since (that edit is
      // still to be saved).
      if (newestSent.current.get(field) === seq && !timers.current.has(field) && saved.current) {
        const confirmed = saved.current[field];
        if (sent.current) sent.current = { ...sent.current, [field]: confirmed };
        if (edited.current) edited.current = { ...edited.current, [field]: confirmed };
        setSettings((s) => (s && s[field] === value ? { ...s, [field]: confirmed } : s));
      }
    } finally {
      setSavesInFlight((n) => n - 1);
    }
  }

  async function loadYoutubeAuth() {
    try {
      const status = await youtubeGetAuthStatus();
      setYoutubeAuth(status);
      if (status.is_authenticated) {
        loadPlaylists();
      }
    } catch (err) {
      setYoutubeError(errorMessage(err, "Couldn't check the YouTube sign-in"));
    }
  }

  async function loadPlaylists() {
    try {
      setPlaylistsLoading(true);
      setPlaylistsError(null);
      const list = await youtubeListPlaylists();
      setPlaylists(list);
    } catch (err) {
      setPlaylistsError(errorMessage(err, "Couldn't load your playlists"));
    } finally {
      setPlaylistsLoading(false);
    }
  }

  // Clicking again while waiting for the browser starts over; the backend
  // cancels the earlier sign-in, and only the latest one updates the screen.
  const signInRequest = useRef(0);
  async function handleYoutubeSignIn() {
    const request = ++signInRequest.current;
    try {
      setYoutubeLoading(true);
      setYoutubeError(null);
      const status = await youtubeSignIn();
      if (request !== signInRequest.current) return;
      setYoutubeAuth(status);
      if (status.is_authenticated) {
        loadPlaylists();
      }
      await reloadSignIn();
    } catch (err) {
      if (request !== signInRequest.current) return;
      setYoutubeError(errorMessage(err, "Couldn't sign in to YouTube"));
    } finally {
      if (request === signInRequest.current) setYoutubeLoading(false);
    }
  }

  async function handleYoutubeSignOut() {
    try {
      setYoutubeLoading(true);
      setYoutubeError(null);
      await youtubeSignOut();
      setYoutubeAuth({ is_authenticated: false, channel_name: null, channel_id: null });
      setPlaylists([]);
      await reloadSignIn();
    } catch (err) {
      setYoutubeError(errorMessage(err, "Couldn't sign out of YouTube"));
    } finally {
      setYoutubeLoading(false);
    }
  }

  // Sign-in changed on the backend: reload the settings, keeping any edit
  // that hasn't been saved yet.
  async function reloadSignIn() {
    try {
      const fresh = await getSettings();
      saved.current = fresh;
      sent.current = fresh;
      setSettings((s) => {
        if (!s) return fresh;
        const next = { ...fresh };
        for (const field of timers.current.keys()) {
          (next as Record<Field, unknown>)[field] = s[field];
        }
        return next;
      });
    } catch (err) {
      setActionError(errorMessage(err, "Couldn't reload the settings"));
    }
  }

  if (!loading && loadError) {
    return (
      <div className="p-6 max-w-2xl mx-auto">
        <h2 className="text-2xl font-bold mb-6">Settings</h2>
        <ErrorBanner message={loadError} onRetry={loadSettings} />
      </div>
    );
  }

  if (loading || !settings) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader2 className="w-8 h-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="p-6 max-w-2xl mx-auto">
      <div className="flex items-baseline justify-between mb-6">
        <h2 className="text-2xl font-bold">Settings</h2>
        <span className="text-sm text-muted-foreground flex items-center gap-1" aria-live="polite">
          {savesInFlight > 0 ? (
            <>
              <Loader2 className="w-4 h-4 animate-spin" />
              Saving…
            </>
          ) : savedOnce ? (
            <>
              <CheckCircle className="w-4 h-4" />
              Saved
            </>
          ) : null}
        </span>
      </div>

      <div className="space-y-6">
        {/* Download Quality */}
        <SettingsLibrary
          settings={settings}
          edit={edit}
          flushField={flushField}
          fieldErrors={fieldErrors}
          folderWarnings={folderWarnings}
        />

        {/* Thumbnail Logo */}
        <SettingsThumbnail
          settings={settings}
          edit={edit}
          fieldErrors={fieldErrors}
        />

        {/* YouTube Account */}
        <SettingsYouTube
          settings={settings}
          edit={edit}
          fieldErrors={fieldErrors}
          youtubeAuth={youtubeAuth}
          youtubeLoading={youtubeLoading}
          youtubeError={youtubeError}
          handleYoutubeSignIn={handleYoutubeSignIn}
          handleYoutubeSignOut={handleYoutubeSignOut}
          playlists={playlists}
          playlistsLoading={playlistsLoading}
          playlistsError={playlistsError}
          loadPlaylists={loadPlaylists}
        />

        {/* Modal API */}
        <SettingsProcessing
          settings={settings}
          edit={edit}
          flushField={flushField}
          fieldErrors={fieldErrors}
        />

        {/* App Info */}
        <SettingsApplication
          appVersion={appVersion}
          folders={folders}
          setActionError={setActionError}
          updater={updater}
          resetConfirm={resetConfirm}
          setResetConfirm={setResetConfirm}
          resetting={resetting}
          setResetting={setResetting}
          loadSettings={loadSettings}
          loadYoutubeAuth={loadYoutubeAuth}
          clearConfirm={clearConfirm}
          setClearConfirm={setClearConfirm}
          clearing={clearing}
          setClearing={setClearing}
        />

        {actionError && (
          <ErrorBanner message={actionError} onDismiss={() => setActionError(null)} />
        )}
      </div>
    </div>
  );
}
