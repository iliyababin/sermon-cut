import { Image, X } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import type { Settings as SettingsType } from "@/lib/types";
import { LocalImage } from "../LocalImage";
import { FieldError, type EditSetting, type Field } from "./fields";

interface SettingsThumbnailProps {
  settings: SettingsType;
  edit: EditSetting;
  fieldErrors: Partial<Record<Field, string>>;
}

export function SettingsThumbnail({
  settings,
  edit,
  fieldErrors,
}: SettingsThumbnailProps) {
  return (
    <div className="bg-card border border-border rounded-lg p-6">
      <h3 className="text-lg font-semibold mb-4">Thumbnail</h3>

      <div>
        <label className="block text-sm font-medium mb-2">
          Logo Overlay
        </label>
        <p className="text-sm text-muted-foreground mb-3">
          Drawn at the top left of every thumbnail, in place of the SCCY mark. Thumbnails already made are drawn again with it.
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
  );
}
