import type { TemplateConfig } from "./types";
import { sccyDefault, TEMPLATE_VERSION } from "./sccy-default";

export type { TemplateConfig, TemplateData } from "./types";

export function getDefaultTemplate(): TemplateConfig {
  return sccyDefault;
}

export { sccyDefault, TEMPLATE_VERSION };
