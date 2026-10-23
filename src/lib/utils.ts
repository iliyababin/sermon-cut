import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
import { balanceLines } from "./titleSplit";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * Split a title into lines for the stacked thumbnail: one or two words stay
 * on one line, longer titles go on two lines of similar width. (The
 * template shrinks, and if need be re-splits into three, to fit.)
 */
export function splitTitleIntoLines(title: string): string[] {
  const words = title.trim().split(/\s+/).filter(Boolean);
  if (words.length <= 2) return [words.join(" ")];
  return balanceLines(words.join(" "), 2);
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

/** "1.2 GB", "350 MB": a file size for people. */
export function formatFileSize(bytes: number): string {
  const units = ["bytes", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit++;
  }
  const digits = unit === 0 || value >= 100 ? 0 : 1;
  return `${value.toFixed(digits)} ${units[unit]}`;
}

/**
 * File names that more than one library item shares (compared the way the
 * backend does, NFC), so their cards can show what tells them apart.
 */
export function duplicateFileNames(videos: { file_name: string }[]): Set<string> {
  const counts = new Map<string, number>();
  for (const v of videos) {
    const key = v.file_name.normalize("NFC");
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return new Set([...counts].filter(([, n]) => n > 1).map(([name]) => name));
}
