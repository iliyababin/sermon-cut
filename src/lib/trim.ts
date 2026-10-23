import type { TranscriptSegment } from "./types";

/** Seconds as "12:03.4" or "1:02:03.4" (tenths, rounded down). */
export function formatPrecise(seconds: number): string {
  const tenths = Math.max(0, Math.floor(seconds * 10 + 1e-6));
  const h = Math.floor(tenths / 36000);
  const m = Math.floor((tenths % 36000) / 600);
  const s = Math.floor((tenths % 600) / 10);
  const t = tenths % 10;
  const ss = `${s.toString().padStart(2, "0")}.${t}`;
  return h > 0 ? `${h}:${m.toString().padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** What is being said at `time`: the segment around it, else the last one
 * that started before it. */
export function lineAt(segments: TranscriptSegment[], time: number): TranscriptSegment | null {
  let before: TranscriptSegment | null = null;
  for (const seg of segments) {
    if (seg.start <= time && time < seg.end) return seg;
    if (seg.start <= time && (!before || seg.start > before.start)) before = seg;
  }
  return before;
}

/** `time` moved by `delta`, kept within min..max. */
export function nudge(time: number, delta: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, Math.round((time + delta) * 1000) / 1000));
}

/** The shortest trim the backend accepts. */
export const MIN_TRIM_SECS = 1;
