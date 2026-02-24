import { useEffect, useState } from "react";
import { getSettings, saveSettings, getAppDataDir, resetState, youtubeGetAuthStatus, youtubeSignIn, youtubeSignOut, youtubeListPlaylists } from "@/lib/tauri";
import type { Settings as SettingsType, YouTubeAuthStatus, PlaylistInfo } from "@/lib/types";
import { Save, Loader2, FolderOpen, CheckCircle, Image, X, Youtube, LogOut, AlertTriangle, RefreshCw, Download } from "lucide-react";
import { useUpdater } from "@/hooks/useUpdater";
import { open } from "@tauri-apps/plugin-dialog";
import { LocalImage } from "./LocalImage";

export function Settings() {
  const [settings, setSettings] = useState<SettingsType | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [appDataDir, setAppDataDir] = useState<string>("");
  const [youtubeAuth, setYoutubeAuth] = useState<YouTubeAuthStatus | null>(null);
  const [youtubeLoading, setYoutubeLoading] = useState(false);
  const [youtubeError, setYoutubeError] = useState<string | null>(null);
  const [playlists, setPlaylists] = useState<PlaylistInfo[]>([]);
  const [playlistsLoading, setPlaylistsLoading] = useState(false);
  const [resetConfirm, setResetConfirm] = useState(false);
  const [resetting, setResetting] = useState(false);
  const updater = useUpdater();

  useEffect(() => {
    loadSettings();
    loadYoutubeAuth();
  }, []);

  async function loadSettings() {
    try {
      setLoading(true);
      const [loadedSettings, dataDir] = await Promise.all([
        getSettings(),
        getAppDataDir(),
      ]);
      setSettings(loadedSettings);
      setAppDataDir(dataDir);
    } catch (err) {
      console.error("Failed to load settings:", err);
    } finally {
      setLoading(false);
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
      console.error("Failed to load YouTube auth status:", err);
    }
  }

  async function loadPlaylists() {
    try {
      setPlaylistsLoading(true);
      const list = await youtubeListPlaylists();
      setPlaylists(list);
    } catch (err) {
      console.error("Failed to load playlists:", err);
    } finally {
      setPlaylistsLoading(false);
    }
  }

  async function handleYoutubeSignIn() {
    try {
      setYoutubeLoading(true);
      setYoutubeError(null);
      console.log("[Settings] Starting YouTube sign in...");
      const status = await youtubeSignIn();
      console.log("[Settings] Sign in complete:", status);
      setYoutubeAuth(status);
      if (status.is_authenticated) {
        loadPlaylists();
      }
    } catch (err) {
      console.error("Failed to sign in to YouTube:", err);
      setYoutubeError(err instanceof Error ? err.message : String(err));
    } finally {
      setYoutubeLoading(false);
    }
  }

  async function handleYoutubeSignOut() {
    try {
      setYoutubeLoading(true);
      await youtubeSignOut();
      setYoutubeAuth({ is_authenticated: false, channel_name: null, channel_id: null });
    } catch (err) {
      console.error("Failed to sign out of YouTube:", err);
    } finally {
      setYoutubeLoading(false);
    }
  }

  async function handleSave() {
    if (!settings) return;

    try {
      setSaving(true);
      await saveSettings(settings);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (err) {
      console.error("Failed to save settings:", err);
    } finally {
      setSaving(false);
    }
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
      <h2 className="text-2xl font-bold mb-6">Settings</h2>

      <div className="space-y-6">
        {/* Download Quality */}
        <div className="bg-card border border-border rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4">Download</h3>

          <div>
            <label className="block text-sm font-medium mb-2">
              Default Quality
            </label>
            <select
              value={settings.download_quality}
              onChange={(e) =>
                setSettings({ ...settings, download_quality: e.target.value })
              }
              className="w-full px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring bg-background"
            >
              <option value="best">Best Available</option>
              <option value="2160p">4K (2160p)</option>
              <option value="1440p">1440p</option>
              <option value="1080p">1080p</option>
              <option value="720p">720p</option>
              <option value="480p">480p</option>
            </select>
          </div>

          <div className="mt-4">
            <label className="block text-sm font-medium mb-2">
              Output Folder
            </label>
            <div className="flex gap-2">
              <input
                type="text"
                value={settings.output_folder}
                onChange={(e) =>
                  setSettings({ ...settings, output_folder: e.target.value })
                }
                placeholder="Leave empty for default"
                className="flex-1 px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring"
              />
              <button
                onClick={async () => {
                  const selected = await open({ directory: true });
                  if (selected && typeof selected === "string") {
                    setSettings({ ...settings, output_folder: selected });
                  }
                }}
                className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 transition-colors"
              >
                <FolderOpen className="w-5 h-5" />
              </button>
            </div>
            <p className="mt-2 text-sm text-muted-foreground">
              Default: ~/Videos/sermon-cut
            </p>
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
                    onClick={() => setSettings({ ...settings, logo_path: null })}
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
                  if (selected) {
                    setSettings({ ...settings, logo_path: selected as string });
                  }
                }}
                className="w-full px-4 py-8 border-2 border-dashed border-border rounded-lg hover:border-muted-foreground/50 transition-colors flex flex-col items-center gap-2 text-muted-foreground"
              >
                <Image className="w-8 h-8" />
                <span>Click to select logo image</span>
                <span className="text-xs">PNG with transparency recommended</span>
              </button>
            )}
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
              disabled={youtubeLoading}
              className="w-full px-4 py-3 bg-red-600 text-white rounded-lg hover:bg-red-700 disabled:opacity-50 flex items-center justify-center gap-2 transition-colors"
            >
              {youtubeLoading ? (
                <>
                  <Loader2 className="w-5 h-5 animate-spin" />
                  Connecting...
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
              <label className="block text-sm font-medium mb-2">
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
                  value={settings.youtube_playlist_id ?? ""}
                  onChange={(e) =>
                    setSettings({
                      ...settings,
                      youtube_playlist_id: e.target.value || null,
                    })
                  }
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
            </div>
          )}
        </div>

        {/* Modal API */}
        <div className="bg-card border border-border rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4">Transcription API</h3>

          <div>
            <label className="block text-sm font-medium mb-2">
              Modal API URL
            </label>
            <input
              type="text"
              value={settings.modal_api_url}
              onChange={(e) =>
                setSettings({ ...settings, modal_api_url: e.target.value })
              }
              className="w-full px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring font-mono text-sm"
            />
          </div>

          <div className="mt-4">
            <label className="block text-sm font-medium mb-2">
              API Key (Optional)
            </label>
            <input
              type="password"
              value={settings.modal_api_key ?? ""}
              onChange={(e) =>
                setSettings({
                  ...settings,
                  modal_api_key: e.target.value || null,
                })
              }
              placeholder="Leave empty if not required"
              className="w-full px-4 py-2 border border-input rounded-lg focus:outline-none focus:ring-2 focus:ring-ring"
            />
          </div>
        </div>

        {/* App Info */}
        <div className="bg-card border border-border rounded-lg p-6">
          <h3 className="text-lg font-semibold mb-4">Application</h3>

          <div className="space-y-2 text-sm">
            <div className="flex justify-between">
              <span className="text-muted-foreground">Version</span>
              <span>0.1.0</span>
            </div>
            <div className="flex justify-between">
              <span className="text-muted-foreground">Data Directory</span>
              <span className="font-mono text-xs">{appDataDir}</span>
            </div>
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
            <h4 className="text-sm font-semibold mb-2">Reset Application State</h4>
            <p className="text-sm text-muted-foreground mb-3">
              This will clear all saved videos and reset settings to defaults. Downloaded files on disk will not be deleted.
            </p>

            {!resetConfirm ? (
              <button
                onClick={() => setResetConfirm(true)}
                className="px-4 py-2 bg-destructive/10 text-destructive rounded-lg hover:bg-destructive/20 transition-colors text-sm"
              >
                Reset State...
              </button>
            ) : (
              <div className="p-4 bg-destructive/10 border border-destructive/30 rounded-lg space-y-3">
                <div className="flex items-start gap-2">
                  <AlertTriangle className="w-5 h-5 text-destructive shrink-0 mt-0.5" />
                  <p className="text-sm text-destructive">
                    Are you sure? This will remove all videos from the library and reset all settings. This cannot be undone.
                  </p>
                </div>
                <div className="flex gap-2">
                  <button
                    onClick={async () => {
                      try {
                        setResetting(true);
                        await resetState();
                        await loadSettings();
                        setResetConfirm(false);
                      } catch (err) {
                        console.error("Failed to reset state:", err);
                      } finally {
                        setResetting(false);
                      }
                    }}
                    disabled={resetting}
                    className="px-4 py-2 bg-destructive text-destructive-foreground rounded-lg hover:bg-destructive/90 disabled:opacity-50 flex items-center gap-2 text-sm transition-colors"
                  >
                    {resetting ? (
                      <Loader2 className="w-4 h-4 animate-spin" />
                    ) : null}
                    Yes, Reset Everything
                  </button>
                  <button
                    onClick={() => setResetConfirm(false)}
                    disabled={resetting}
                    className="px-4 py-2 bg-secondary text-secondary-foreground rounded-lg hover:bg-secondary/80 disabled:opacity-50 text-sm transition-colors"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            )}
          </div>
        </div>

        {/* Save Button */}
        <div className="flex justify-end">
          <button
            onClick={handleSave}
            disabled={saving}
            className="px-6 py-3 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 disabled:opacity-50 flex items-center gap-2 transition-colors"
          >
            {saving ? (
              <>
                <Loader2 className="w-5 h-5 animate-spin" />
                Saving...
              </>
            ) : saved ? (
              <>
                <CheckCircle className="w-5 h-5" />
                Saved!
              </>
            ) : (
              <>
                <Save className="w-5 h-5" />
                Save Settings
              </>
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
