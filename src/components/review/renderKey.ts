import { TEMPLATE_VERSION } from "@/templates";
import type { ThumbnailOption } from "@/lib/types";

export interface RenderInputs {
  titleLines: string[];
  preacher: string;
  singleLine: boolean;
}

/** Everything that goes into a template render of `option`, hashed. Two
 * renders with the same key look the same, so a stored one is reused. */
export function renderKey(option: Pick<ThumbnailOption, "id" | "version">, inputs: RenderInputs): string {
  return hash(
    JSON.stringify([
      TEMPLATE_VERSION,
      inputs.titleLines,
      inputs.preacher.trim(),
      inputs.singleLine,
      option.id,
      option.version,
    ]),
  );
}

/** cyrb53: a fast 53-bit string hash, as hex. Not cryptographic. */
function hash(str: string): string {
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let i = 0; i < str.length; i++) {
    const ch = str.charCodeAt(i);
    h1 = Math.imul(h1 ^ ch, 2654435761);
    h2 = Math.imul(h2 ^ ch, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(16);
}
