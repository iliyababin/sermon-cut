/**
 * Splitting a sermon title into lines of similar width. Widths are
 * estimated per character (in ems of a serif face), so the split is the
 * same everywhere and doesn't depend on fonts having loaded: it's part of
 * the render key.
 */

const NARROW = new Set("ijlI.,;:'!|`’");
const SLIM = new Set("frtJ()-");
const WIDE = new Set("mwMW@%");

/** Rough width of `text` in ems of Baskervville. */
export function estimateWidth(text: string): number {
  let w = 0;
  for (const ch of text) {
    if (ch === " ") w += 0.25;
    else if (NARROW.has(ch)) w += 0.28;
    else if (SLIM.has(ch)) w += 0.38;
    else if (WIDE.has(ch)) w += 0.85;
    else if (ch !== ch.toLowerCase()) w += 0.68;
    else w += 0.52;
  }
  return w;
}

/**
 * `text` in `lines` lines (fewer if it has fewer words), breaking between
 * words so the widest line is as narrow as possible. Ties go to the more
 * even split, then to the shorter top line.
 */
export function balanceLines(text: string, lines: number, measure: (s: string) => number = estimateWidth): string[] {
  const words = text.trim().split(/\s+/).filter(Boolean);
  const n = Math.max(1, Math.min(lines, words.length));
  if (n === 1) return words.length ? [words.join(" ")] : [];

  let best: { breaks: number[]; max: number; spread: number } | null = null;
  // Every way to choose n-1 break points among the gaps between words.
  const choose = (start: number, left: number, breaks: number[]) => {
    if (left === 0) {
      const bounds = [0, ...breaks, words.length];
      const widths = bounds.slice(1).map((end, i) => measure(words.slice(bounds[i], end).join(" ")));
      const max = Math.max(...widths);
      const spread = widths.reduce((s, w) => s + (max - w) ** 2, 0);
      const better =
        !best ||
        max < best.max - 1e-9 ||
        (Math.abs(max - best.max) < 1e-9 && spread < best.spread - 1e-9);
      if (better) best = { breaks: [...breaks], max, spread };
      return;
    }
    for (let b = start; b <= words.length - left; b++) choose(b + 1, left - 1, [...breaks, b]);
  };
  choose(1, n - 1, []);

  const bounds = [0, ...best!.breaks, words.length];
  return bounds.slice(1).map((end, i) => words.slice(bounds[i], end).join(" "));
}
