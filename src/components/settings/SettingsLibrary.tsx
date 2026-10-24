import { AlertTriangle, FolderOpen } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import type { Settings as SettingsType } from "@/lib/types";
import { FieldError, type EditSetting, type Field } from "./fields";

interface SettingsLibraryProps {
  settings: SettingsType;
  edit: EditSetting;
  flushField: (field: Field) => void;
  fieldErrors: Partial<Record<Field, string>>;
  folderWarnings: string[];
}

export function SettingsLibrary({
  settings,
  edit,
  flushField,
  fieldErrors,
  folderWarnings,
}: SettingsLibraryProps) {
  return (
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
          className="w-full px-4 py-2 border border-input rounded-lg focus:outline-hidden focus:ring-2 focus:ring-ring bg-background"
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
            className="flex-1 px-4 py-2 border border-input rounded-lg focus:outline-hidden focus:ring-2 focus:ring-ring"
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

      <div className="mt-4">
        <label className="flex items-start gap-3 cursor-pointer">
          <input
            type="checkbox"
            checked={settings.auto_process}
            onChange={(e) => edit("auto_process", e.target.checked, 0)}
            className="mt-1 rounded"
          />
          <span>
            <span className="block text-sm font-medium">Process new videos automatically</span>
            <span className="block text-sm text-muted-foreground">
              Videos that show up in the library folder from now on are transcribed and trimmed right away. Videos already there are left for you to start.
            </span>
          </span>
        </label>
        <FieldError message={fieldErrors.auto_process} />
      </div>
    </div>
  );
}
