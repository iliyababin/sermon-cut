import type { Settings as SettingsType } from "@/lib/types";
import { FieldError, type EditSetting, type Field } from "./fields";

interface SettingsProcessingProps {
  settings: SettingsType;
  edit: EditSetting;
  flushField: (field: Field) => void;
  fieldErrors: Partial<Record<Field, string>>;
}

export function SettingsProcessing({
  settings,
  edit,
  flushField,
  fieldErrors,
}: SettingsProcessingProps) {
  return (
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
          className="w-full px-4 py-2 border border-input rounded-lg focus:outline-hidden focus:ring-2 focus:ring-ring font-mono text-sm"
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
          className="w-full px-4 py-2 border border-input rounded-lg focus:outline-hidden focus:ring-2 focus:ring-ring"
        />
        <FieldError message={fieldErrors.modal_api_key} />
      </div>
    </div>
  );
}
