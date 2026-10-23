import type { Settings, SettingsPatch } from "./types";

/** The fields a user can edit in Settings; everything else is the app's. */
export const EDITABLE_SETTINGS = [
  "download_quality",
  "output_folder",
  "modal_api_url",
  "modal_api_key",
  "logo_path",
  "youtube_playlist_id",
] as const satisfies readonly (keyof Settings & keyof SettingsPatch)[];

/** What changed between the saved settings and the edited ones, as a patch.
 * Cleared optional fields are sent as "". */
export function settingsPatch(saved: Settings, edited: Settings): SettingsPatch {
  const patch: SettingsPatch = {};
  for (const key of EDITABLE_SETTINGS) {
    const value = edited[key] ?? "";
    if (value !== (saved[key] ?? "")) patch[key] = value;
  }
  return patch;
}
