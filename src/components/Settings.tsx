import { useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { getSettings, saveSettings, nextSettingsSeq, getAppFolders, openFolder, resetState, clearLibraryData, checkLibraryFolder, youtubeGetAuthStatus, youtubeSignIn, youtubeSignOut, youtubeListPlaylists, errorMessage } from "@/lib/tauri";
import type { Settings as SettingsType, YouTubeAuthStatus, PlaylistInfo } from "@/lib/types";
import { EDITABLE_SETTINGS, settingsPatch } from "@/lib/settingsPatch";
import { Loader2, FolderOpen, CheckCircle, Image, X, Youtube, LogOut, AlertTriangle, RefreshCw, Download } from "lucide-react";
import { useUpdater } from "@/hooks/useUpdater";
import { open } from "@tauri-apps/plugin-dialog";
import { LocalImage } from "./LocalImage";

type Field = (typeof EDITABLE_SETTINGS)[number];

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
  // newer seq, so the backend ignores one that arrives late.
  const [fieldErrors, setFieldErrors] = useState<Partial<Record<Field, string>>>({});
  const [savesInFlight, setSavesInFlight] = useState(0);
  const [savedOnce, setSavedOnce] = useState(false);
  // As last loaded or saved; a save sends only what differs from it.
  const saved = useRef<SettingsType | null>(null);
  const edited = useRef<SettingsType | null>(null);
  // Fields waiting to be saved; null: until flushField (Enter or blur).
  const timers = useRef(new Map<Field, ReturnType<typeof setTimeout> | null>());
  const newestApplied = useRef(0);

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
    if (edited.current) edited.current = { ...edited.current, [field]: value };
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
    const base = saved.current;
    const current = edited.current;
    if (!base || !current) return;
    const patch = settingsPatch(base, { ...base, [field]: current[field] });
    if (Object.keys(patch).length === 0) return;
    const seq = nextSettingsSeq();
    setSavesInFlight((n) => n + 1);
    try {
      const updated = await saveSettings(patch, seq);
      if (seq < newestApplied.current) return;
      newestApplied.current = seq;
      saved.current = updated;
      // Show the value as saved (e.g. trimmed), unless it was edited since.
      const sent = current[field];
      setSettings((s) =>
        s && s[field] === sent ? { ...s, [field]: updated[field] } : s
      );
      setSavedOnce(true);
    } catch (err) {
      setFieldErrors((e) => ({ ...e, [field]: errorMessage(err, "Couldn't save this setting") }));
      if (field === "output_folder") {
        // A refused folder change was put back; show the folder in use.
        try {
          const current = await getSettings();
          saved.current = current;
          setSettings((s) => (s ? { ...s, output_folder: current.output_folder } : current));
        } catch {
          // Keep the edit on screen.
        }
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
        <div className="p-3 bg-destructive/10 text-destructive rounded-lg text-sm flex items-center justify-between gap-3">
          <span>{loadError}</span>
          <button onClick={loadSettings} className="px-3 py-1 bg-secondary text-secondary-foreground rounded-lg text-sm">
            Try Again
          </button>
        </div>
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
        <div className="bg-card border border-border rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4">Download</h3>

          <div>
            <label htmlFor="settings-quality" className="block text-sm font-medium mb-2">
              Default Quality
            </label>
            <select
              id="settings-quality"
              value={settings.download_quality}
              onChange={(e) => edit("download_quality", e.target.value, 0)}
              className="w-full px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring bg-background"
            >
              <option value="best">Best Available</option>
              <option value="2160p">4K (2160p)</option>
              <option value="1440p">1440p</option>
              <option value="1080p">1080p</option>
              <option value="720p">720p</option>
              <option value="480p">480p</option>
            </select>
            <FieldError message={fieldErrors.download_quality} />
          </div>

          <div className="mt-4">
            <label htmlFor="settings-folder" className="block text-sm font-medium mb-2">
              Library Folder
            </label>
            <p className="text-sm text-muted-foreground mb-2">
              Every video in this folder shows up in the library. Drop recordings in here; downloads from YouTube are saved here too.
            </p>
            <div className="flex gap-2">
              {/* A typed path is saved on Enter or leaving the field, not
                  while typing: each save switches the library. */}
              <input
                id="settings-folder"
                type="text"
                value={settings.output_folder}
                onChange={(e) => edit("output_folder", e.target.value, Infinity)}
                onBlur={() => flushField("output_folder")}
                onKeyDown={(e) => {
                  if (e.key === "Enter") flushField("output_folder");
                }}
                placeholder="Leave empty for default"
                className="flex-1 px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring"
              />
              <button
                onClick={async () => {
                  const selected = await open({ directory: true });
                  if (selected && typeof selected === "string") {
                    edit("output_folder", selected, 0);
                  }
                }}
                title="Choose folder"
                className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
              >
                <FolderOpen className="w-5 h-5" />
              </button>
            </div>
            <p className="mt-2 text-sm text-muted-foreground">
              Default: Movies/sermon-cut on Mac, Videos/sermon-cut on Windows and Linux
            </p>
            <FieldError message={fieldErrors.output_folder} />
            {folderWarnings.map((warning, i) => (
              <div key={i} className="mt-2 flex items-start gap-2 p-3 text-sm rounded-lg bg-yellow-100 text-yellow-800 dark:bg-yellow-900/30 dark:text-yellow-300">
                <AlertTriangle className="w-4 h-4 shrink-0 mt-0.5" />
                <span>{warning}</span>
              </div>
            ))}
          </div>
        </div>

        {/* Thumbnail Logo */}
        <div className="bg-card border border-border rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4">Thumbnail</h3>

          <div>
            <label className="block text-sm font-medium mb-2">
              Logo Overlay
            </label>
            <p className="text-sm text-muted-foreground mb-3">
              Add a logo to generated thumbnails. A gradient fade will be applied on the left side for visibility.
            </p>

            {settings.logo_path ? (
              <div className="space-y-3">
                <div className="flex items-center gap-4 p-3 bg-muted rounded-lg">
                  <div className="w-16 h-16 rounded border border-border overflow-hidden bg-black flex items-center justify-center">
                    <LocalImage
                      path={settings.logo_path}
                      alt="Logo preview"
                      className="max-w-full max-h-full object-contain"
                    />
                  </div>
                  <div className="flex-1 min-w-0">
                    <p className="text-sm font-medium truncate">{settings.logo_path.split('/').pop()}</p>
                    <p className="text-xs text-muted-foreground truncate">{settings.logo_path}</p>
                  </div>
                  <button
                    onClick={() => edit("logo_path", null, 0)}
                    className="p-2 hover:bg-secondary rounded-lg transition-colors"
                    title="Remove logo"
                  >
                    <X className="w-4 h-4" />
                  </button>
                </div>
              </div>
            ) : (
              <button
                onClick={async () => {
                  const selected = await open({
                    multiple: false,
                    filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "webp"] }],
                  });
                  if (selected && typeof selected === "string") {
                    edit("logo_path", selected, 0);
                  }
                }}
                className="w-full px-4 py-8 border-2 border-dashed border-border rounded-lg hover:border-muted-foreground/50 transition-colors flex flex-col items-center gap-2 text-muted-foreground"
              >
                <Image className="w-8 h-8" />
                <span>Click to select logo image</span>
                <span className="text-xs">PNG with transparency recommended</span>
              </button>
            )}
            <FieldError message={fieldErrors.logo_path} />
          </div>
        </div>

        {/* YouTube Account */}
        <div className="bg-card border border-border rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4">YouTube Account</h3>
          <p className="text-sm text-muted-foreground mb-4">
            Connect your YouTube account to upload processed videos directly.
          </p>

          {youtubeAuth?.is_authenticated ? (
            <div className="flex items-center justify-between p-4 bg-muted rounded-lg">
              <div className="flex items-center gap-3">
                <div className="w-10 h-10 bg-red-600 rounded-full flex items-center justify-center">
                  <Youtube className="w-5 h-5 text-white" />
                </div>
                <div>
                  <p className="font-medium">{youtubeAuth.channel_name}</p>
                  <p className="text-sm text-muted-foreground">Connected</p>
                </div>
              </div>
              <button
                onClick={handleYoutubeSignOut}
                disabled={youtubeLoading}
                className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 flex items-center gap-2 transition-colors"
              >
                {youtubeLoading ? (
                  <Loader2 className="w-4 h-4 animate-spin" />
                ) : (
                  <LogOut className="w-4 h-4" />
                )}
                Sign Out
              </button>
            </div>
          ) : (
            <button
              onClick={handleYoutubeSignIn}
              title={youtubeLoading ? "Finish signing in in the browser, or click to start again" : undefined}
              className="w-full px-4 py-3 bg-red-600 text-white rounded-lg hover:bg-red-700 disabled:opacity-50 flex items-center justify-center gap-2 transition-colors"
            >
              {youtubeLoading ? (
                <>
                  <Loader2 className="w-5 h-5 animate-spin" />
                  Waiting for the browser… (click to start again)
                </>
              ) : (
                <>
                  <Youtube className="w-5 h-5" />
                  Sign in with YouTube
                </>
              )}
            </button>
          )}

          {youtubeError && (
            <div className="mt-3 p-3 bg-destructive/10 text-destructive rounded-lg text-sm">
              {youtubeError}
            </div>
          )}

          {youtubeAuth?.is_authenticated && (
            <div className="mt-4">
              <label htmlFor="settings-playlist" className="block text-sm font-medium mb-2">
                Upload Playlist
              </label>
              <p className="text-sm text-muted-foreground mb-2">
                Automatically add uploaded videos to a playlist.
              </p>
              {playlistsLoading ? (
                <div className="flex items-center gap-2 text-sm text-muted-foreground py-2">
                  <Loader2 className="w-4 h-4 animate-spin" />
                  Loading playlists...
                </div>
              ) : (
                <select
                  id="settings-playlist"
                  value={settings.youtube_playlist_id ?? ""}
                  onChange={(e) => edit("youtube_playlist_id", e.target.value || null, 0)}
                  className="w-full px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring bg-background"
                >
                  <option value="">None</option>
                  {playlists.map((pl) => (
                    <option key={pl.id} value={pl.id}>
                      {pl.title}
                    </option>
                  ))}
                </select>
              )}
              {playlistsError && (
                <div className="mt-2 flex items-center justify-between gap-3 p-3 bg-destructive/10 text-destructive rounded-lg text-sm">
                  <span>{playlistsError}</span>
                  <button onClick={loadPlaylists} className="px-3 py-1 bg-secondary text-secondary-foreground rounded-lg text-sm">
                    Try Again
                  </button>
                </div>
              )}
              <FieldError message={fieldErrors.youtube_playlist_id} />
            </div>
          )}
        </div>

        {/* Modal API */}
        <div className="bg-card border border-border rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4">Transcription API</h3>

          <div>
            <label htmlFor="settings-modal-url" className="block text-sm font-medium mb-2">
              Modal API URL
            </label>
            <input
              id="settings-modal-url"
              type="text"
              value={settings.modal_api_url}
              onChange={(e) => edit("modal_api_url", e.target.value)}
              onBlur={() => flushField("modal_api_url")}
              className="w-full px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring font-mono text-sm"
            />
            <FieldError message={fieldErrors.modal_api_url} />
          </div>

          <div className="mt-4">
            <label htmlFor="settings-modal-key" className="block text-sm font-medium mb-2">
              API Key (Optional)
            </label>
            <input
              id="settings-modal-key"
              type="password"
              value={settings.modal_api_key ?? ""}
              onChange={(e) => edit("modal_api_key", e.target.value || null)}
              onBlur={() => flushField("modal_api_key")}
              placeholder="Leave empty if not required"
              className="w-full px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring"
            />
            <FieldError message={fieldErrors.modal_api_key} />
          </div>
        </div>

        {/* App Info */}
        <div className="bg-card border border-border rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4">Application</h3>

          <div className="space-y-2 text-sm">
            <div className="flex justify-between">
              <span className="text-muted-foreground">Version</span>
              <span>{appVersion}</span>
            </div>
            {folders && (
              <>
                <FolderRow
                  label="Settings folder"
                  path={folders.settings}
                  onOpen={() => openFolder(folders.settings).catch((err) => setActionError(errorMessage(err)))}
                />
                {folders.tools && (
                  <FolderRow
                    label="Tools folder"
                    path={folders.tools}
                    onOpen={() => openFolder(folders.tools!).catch((err) => setActionError(errorMessage(err)))}
                  />
                )}
              </>
            )}
          </div>

          <div className="mt-4">
            {updater.available ? (
              <div className="space-y-3">
                <div className="flex items-center justify-between">
                  <span className="text-sm font-medium text-green-600">
                    Update v{updater.version} available
                  </span>
                  <button
                    onClick={updater.downloadAndInstall}
                    disabled={updater.downloading}
                    className="px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 disabled:opacity-50 flex items-center gap-2 text-sm transition-colors"
                  >
                    {updater.downloading ? (
                      <>
                        <Loader2 className="w-4 h-4 animate-spin" />
                        Installing... {updater.progress}%
                      </>
                    ) : (
                      <>
                        <Download className="w-4 h-4" />
                        Download & Install
                      </>
                    )}
                  </button>
                </div>
                {updater.downloading && (
                  <div className="w-full bg-secondary rounded-full h-2">
                    <div
                      className="bg-primary h-2 rounded-full transition-all"
                      style={{ width: `${updater.progress}%` }}
                    />
                  </div>
                )}
              </div>
            ) : (
              <button
                onClick={updater.checkForUpdates}
                disabled={updater.checking}
                className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 flex items-center gap-2 text-sm transition-colors"
              >
                {updater.checking ? (
                  <>
                    <Loader2 className="w-4 h-4 animate-spin" />
                    Checking...
                  </>
                ) : (
                  <>
                    <RefreshCw className="w-4 h-4" />
                    Check for Updates
                  </>
                )}
              </button>
            )}
            {updater.error && (
              <p className="mt-2 text-sm text-destructive">{updater.error}</p>
            )}
            {updater.checked && !updater.available && !updater.checking && !updater.error && (
              <p className="mt-2 text-sm text-muted-foreground">
                You're on the latest version.
              </p>
            )}
          </div>

          <div className="mt-6 pt-6 border-t border-border">
            <h4 className="text-sm font-semibold mb-2">Reset Settings</h4>
            <p className="text-sm text-muted-foreground mb-3">
              Puts every setting back to its default and signs out of YouTube. Your videos and the library are not touched.
            </p>

            {!resetConfirm ? (
              <button
                onClick={() => setResetConfirm(true)}
                className="px-4 py-2 bg-destructive/10 text-destructive rounded-lg hover:bg-destructive/20 transition-colors text-sm"
              >
                Reset Settings...
              </button>
            ) : (
              <ConfirmBox
                message="Reset all settings to their defaults and sign out of YouTube?"
                confirmLabel="Yes, Reset Settings"
                busy={resetting}
                onCancel={() => setResetConfirm(false)}
                onConfirm={async () => {
                  try {
                    setResetting(true);
                    setActionError(null);
                    await resetState();
                    await loadSettings();
                    await loadYoutubeAuth();
                    setResetConfirm(false);
                  } catch (err) {
                    setActionError(errorMessage(err, "Failed to reset settings"));
                  } finally {
                    setResetting(false);
                  }
                }}
              />
            )}
          </div>

          <div className="mt-6 pt-6 border-t border-border">
            <h4 className="text-sm font-semibold mb-2">Clear Library Data</h4>
            <p className="text-sm text-muted-foreground mb-3">
              Deletes Sermon Cut's transcripts, trimmed videos and thumbnails (the hidden .sermon-cut folder inside the library). Your video files stay where they are and show up as not processed.
            </p>

            {!clearConfirm ? (
              <button
                onClick={() => setClearConfirm(true)}
                className="px-4 py-2 bg-destructive/10 text-destructive rounded-lg hover:bg-destructive/20 transition-colors text-sm"
              >
                Clear Library Data...
              </button>
            ) : (
              <ConfirmBox
                message="Delete all transcripts, trimmed videos, thumbnails and upload history for this library? This cannot be undone. Your video files are kept."
                confirmLabel="Yes, Clear Library Data"
                busy={clearing}
                onCancel={() => setClearConfirm(false)}
                onConfirm={async () => {
                  try {
                    setClearing(true);
                    await clearLibraryData();
                    setClearConfirm(false);
                  } catch (err) {
                    setActionError(errorMessage(err, "Failed to clear library data"));
                  } finally {
                    setClearing(false);
                  }
                }}
              />
            )}
          </div>
        </div>

        {actionError && (
          <div className="p-3 bg-destructive/10 text-destructive rounded-lg text-sm">{actionError}</div>
        )}
      </div>
    </div>
  );
}

function FolderRow({ label, path, onOpen }: { label: string; path: string; onOpen: () => void }) {
  return (
    <div className="flex justify-between items-center gap-4">
      <span className="text-muted-foreground shrink-0">{label}</span>
      <span className="flex items-center gap-2 min-w-0">
        <span className="font-mono text-xs truncate" title={path}>
          {path}
        </span>
        <button onClick={onOpen} title={`Open ${label.toLowerCase()}`} className="p-1 rounded hover:bg-secondary shrink-0">
          <FolderOpen className="w-4 h-4" />
        </button>
      </span>
    </div>
  );
}

function FieldError({ message }: { message?: string }) {
  if (!message) return null;
  return (
    <div role="alert" className="mt-2 p-3 bg-destructive/10 text-destructive rounded-lg text-sm">
      {message}
    </div>
  );
}

function ConfirmBox({
  message,
  confirmLabel,
  busy,
  onConfirm,
  onCancel,
}: {
  message: string;
  confirmLabel: string;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="p-4 bg-destructive/10 border border-destructive/30 rounded-lg space-y-3">
      <div className="flex items-start gap-2">
        <AlertTriangle className="w-5 h-5 text-destructive shrink-0 mt-0.5" />
        <p className="text-sm text-destructive">{message}</p>
      </div>
      <div className="flex gap-2">
        <button
          onClick={onConfirm}
          disabled={busy}
          className="px-4 py-2 bg-destructive text-destructive-foreground rounded-lg hover:bg-destructive/90 disabled:opacity-50 flex items-center gap-2 text-sm transition-colors"
        >
          {busy ? <Loader2 className="w-4 h-4 animate-spin" /> : null}
          {confirmLabel}
        </button>
        <button
          onClick={onCancel}
          disabled={busy}
          className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 text-sm transition-colors"
        >
          Cancel
        </button>
      </div>
    </div>
  );
}
