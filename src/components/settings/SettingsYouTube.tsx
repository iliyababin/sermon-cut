import { Loader2, LogOut, Youtube } from "lucide-react";
import type { PlaylistInfo, Settings as SettingsType, YouTubeAuthStatus } from "@/lib/types";
import { ErrorBanner } from "../ErrorBanner";
import { FieldError, type EditSetting, type Field } from "./fields";

interface SettingsYouTubeProps {
  settings: SettingsType;
  edit: EditSetting;
  fieldErrors: Partial<Record<Field, string>>;
  youtubeAuth: YouTubeAuthStatus | null;
  youtubeLoading: boolean;
  youtubeError: string | null;
  handleYoutubeSignIn: () => void;
  handleYoutubeSignOut: () => void;
  playlists: PlaylistInfo[];
  playlistsLoading: boolean;
  playlistsError: string | null;
  loadPlaylists: () => void;
}

export function SettingsYouTube({
  settings,
  edit,
  fieldErrors,
  youtubeAuth,
  youtubeLoading,
  youtubeError,
  handleYoutubeSignIn,
  handleYoutubeSignOut,
  playlists,
  playlistsLoading,
  playlistsError,
  loadPlaylists,
}: SettingsYouTubeProps) {
  return (
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
        <ErrorBanner className="mt-3" message={youtubeError} />
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
            <ErrorBanner className="mt-2" message={playlistsError} onRetry={loadPlaylists} />
          )}
          <FieldError message={fieldErrors.youtube_playlist_id} />
        </div>
      )}
    </div>
  );
}
