import { AlertTriangle, Download, FolderOpen, Loader2, RefreshCw } from "lucide-react";
import { clearLibraryData, errorMessage, openFolder, resetState } from "@/lib/tauri";
import type { Updater } from "@/hooks/useUpdater";

interface SettingsApplicationProps {
  appVersion: string;
  folders: { settings: string; tools: string | null } | null;
  setActionError: (error: string | null) => void;
  updater: Updater;
  resetConfirm: boolean;
  setResetConfirm: (confirm: boolean) => void;
  resetting: boolean;
  setResetting: (resetting: boolean) => void;
  loadSettings: () => Promise<void>;
  loadYoutubeAuth: () => Promise<void>;
  clearConfirm: boolean;
  setClearConfirm: (confirm: boolean) => void;
  clearing: boolean;
  setClearing: (clearing: boolean) => void;
}

export function SettingsApplication({
  appVersion,
  folders,
  setActionError,
  updater,
  resetConfirm,
  setResetConfirm,
  resetting,
  setResetting,
  loadSettings,
  loadYoutubeAuth,
  clearConfirm,
  setClearConfirm,
  clearing,
  setClearing,
}: SettingsApplicationProps) {
  return (
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
