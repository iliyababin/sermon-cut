import type { Chapter } from "./types";
import { formatTimeDisplay } from "./clips";

/** YouTube's limits, and its rules for chapters to show up. */
export const MAX_DESCRIPTION_BYTES = 5000;
export const MAX_TITLE_CHARS = 100;
export const MIN_CHAPTERS = 3;
export const MIN_CHAPTER_SECS = 10;

const ELLIPSIS = "…";
const encoder = new TextEncoder();

function byteLength(s: string): number {
  return encoder.encode(s).length;
}

/** YouTube rejects < and > in titles and descriptions. */
function stripAngles(s: string): string {
  return s.replace(/[<>]/g, "");
}

/** The title as YouTube will take it: no < or >, one line, at most 100
 * characters. */
export function youtubeTitle(title: string): string {
  const clean = stripAngles(title).replace(/\s+/g, " ").trim();
  return Array.from(clean).slice(0, MAX_TITLE_CHARS).join("").trim();
}

/**
 * The chapters of the trimmed video, timed from its start. The chapter in
 * effect where the trim starts becomes 0:00 and keeps its title (or, if the
 * trim starts before every chapter, the first one moves to 0:00). After
 * that come the chapters inside the trim, each at least 10 s after the one
 * before and (when the length is known) 10 s before the end; 0:00 is
 * required, so a chapter within 10 s of it is the one dropped. Fewer than 3
 * left means none, since YouTube would ignore them anyway.
 */
export function youtubeChapters(chapters: Chapter[], startTime: number, endTime: number): Chapter[] {
  const length = endTime - startTime;
  const clean = chapters
    .map((ch) => ({ time: ch.time, title: stripAngles(ch.title).replace(/\s+/g, " ").trim() }))
    .filter((ch) => ch.title)
    .sort((a, b) => a.time - b.time);
  const opening = clean.filter((ch) => ch.time <= startTime).pop();
  const inside = clean
    .filter((ch) => ch.time > startTime && (length <= 0 || ch.time < endTime))
    .map((ch) => ({ time: ch.time - startTime, title: ch.title }));
  if (opening) {
    inside.unshift({ time: 0, title: opening.title });
  } else if (inside.length > 0) {
    inside[0] = { ...inside[0], time: 0 };
  } else {
    return [];
  }

  const kept: Chapter[] = [];
  for (const ch of inside) {
    const last = kept[kept.length - 1];
    if (last && ch.time - last.time < MIN_CHAPTER_SECS) continue;
    if (length > 0 && length - ch.time < MIN_CHAPTER_SECS && kept.length > 0) continue;
    kept.push(ch);
  }
  return kept.length >= MIN_CHAPTERS ? kept : [];
}

/** At most `max` UTF-8 bytes of `s`, cut on a character boundary, with an
 * ellipsis (counted in `max`) when anything was cut. */
function truncateBytes(s: string, max: number): string {
  if (byteLength(s) <= max) return s;
  const room = max - byteLength(ELLIPSIS);
  if (room <= 0) return "";
  let out = "";
  let used = 0;
  for (const ch of s) {
    const n = byteLength(ch);
    if (used + n > room) break;
    out += ch;
    used += n;
  }
  return out.trimEnd() + ELLIPSIS;
}

export interface DescriptionInput {
  /** What the user wrote. */
  description: string;
  /** The transcript's chapters, in source-video time. */
  chapters: Chapter[];
  startTime: number;
  endTime: number;
  /** Appended under a "---" line (the church's links). */
  footer?: string;
}

/**
 * The description sent to YouTube: the text, then the chapters, then the
 * footer, within YouTube's 5000-byte limit. When it's too long the text is
 * shortened; the chapters are never cut (if even they don't fit with the
 * footer, the footer goes first).
 */
export function buildYoutubeDescription(input: DescriptionInput): string {
  const body = stripAngles(input.description).trim();
  const chapters = youtubeChapters(input.chapters, input.startTime, input.endTime);
  const chapterBlock = chapters.length
    ? "Chapters:\n" + chapters.map((ch) => `${formatTimeDisplay(ch.time)} ${ch.title}`).join("\n")
    : "";
  const footer = input.footer ? stripAngles(input.footer).trim() : "";
  const footerBlock = footer ? `---\n${footer}` : "";

  const SEP = "\n\n";
  for (const tailParts of [[chapterBlock, footerBlock], [chapterBlock], []]) {
    const tail = tailParts.filter(Boolean).join(SEP);
    const full = [body, tail].filter(Boolean).join(SEP);
    if (byteLength(full) <= MAX_DESCRIPTION_BYTES) return full;
    const room = MAX_DESCRIPTION_BYTES - byteLength(tail) - (tail ? byteLength(SEP) : 0);
    if (room < 0) continue;
    const cut = truncateBytes(body, room);
    return [cut, tail].filter(Boolean).join(SEP);
  }
  return truncateBytes(body, MAX_DESCRIPTION_BYTES);
}
