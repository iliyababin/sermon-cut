import type { EDITABLE_SETTINGS } from "@/lib/settingsPatch";
import type { Settings as SettingsType } from "@/lib/types";
import { ErrorBanner } from "../ErrorBanner";

export type Field = (typeof EDITABLE_SETTINGS)[number];

export type EditSetting = <F extends Field>(field: F, value: SettingsType[F], delay?: number) => void;

export function FieldError({ message }: { message?: string }) {
  if (!message) return null;
  return (
    <ErrorBanner className="mt-2" message={message} />
  );
}
