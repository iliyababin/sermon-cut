import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * Split a title string into lines for stacked thumbnail display.
 * Aims for 2-3 lines with roughly balanced word distribution.
 */
export function splitTitleIntoLines(title: string): string[] {
  const words = title.trim().split(/\s+/);
  if (words.length <= 2) return [words.join(" ")];

  // Split into 2 lines, shorter top line, longer bottom
  const mid = Math.floor(words.length / 2);
  const splitAt = Math.max(1, mid);
  return [
    words.slice(0, splitAt).join(" "),
    words.slice(splitAt).join(" "),
  ];
}

/**
 * Map a generated thumbnail option to its crop-only `_raw` variant.
 *
 * The Rust pipeline writes both `thumbnail_option_N.jpg` (colour graded, logo
 * burned in) and `thumbnail_option_N_raw.jpg` (plain crop). The template
 * system draws its own logo and text, so it needs the raw one — otherwise the
 * logo is drawn twice. The raw files are written whenever more than one
 * candidate is requested, and the library always asks for ten.
 *
 * Paths that aren't generated options (e.g. custom uploads) are returned as-is.
 */
export function rawThumbnailVariant(path: string): string {
  return /thumbnail_option_\d+\.(jpg|jpeg)$/i.test(path)
    ? path.replace(/\.(jpg|jpeg)$/i, "_raw.jpg")
    : path;
}
