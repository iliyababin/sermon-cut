import type { TemplateConfig } from "./types";
import { sccyDefault, TEMPLATE_VERSION } from "./sccy-default";

export type { TemplateConfig, TemplateData } from "./types";

const templates: Record<string, TemplateConfig> = {
  "sccy-default": sccyDefault,
};

export function getTemplate(id: string): TemplateConfig {
  const template = templates[id];
  if (!template) throw new Error(`Template not found: ${id}`);
  return template;
}

export function getDefaultTemplate(): TemplateConfig {
  return sccyDefault;
}

export { sccyDefault, TEMPLATE_VERSION };
