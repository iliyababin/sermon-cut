/** Small helpers for building template DOM without HTML strings. */

export type Style = Partial<Record<keyof CSSStyleDeclaration, string>>;

/** An element with inline styles and children (strings become text nodes). */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  style: Style = {},
  children: (Node | string)[] = [],
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  Object.assign(node.style, style);
  for (const child of children) node.append(child);
  return node;
}

const SVG_NS = "http://www.w3.org/2000/svg";

/** An SVG element with attributes and children. */
export function svg(
  tag: string,
  attrs: Record<string, string | number> = {},
  children: (Node | string)[] = [],
): SVGElement {
  const node = document.createElementNS(SVG_NS, tag) as SVGElement;
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
  for (const child of children) node.append(child);
  return node;
}

/**
 * A CSS `url("...")` for any URL. Characters that could end the string or
 * the url() (quotes, brackets, backslashes, whitespace) are percent-encoded,
 * which leaves the URL meaning the same file, so no CSS escaping is needed.
 */
export function cssUrl(url: string): string {
  const safe = url.replace(/["'()\\\s]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0")}`);
  return `url("${safe}")`;
}
