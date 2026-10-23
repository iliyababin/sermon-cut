import { describe, expect, it } from "vitest";
import { buildYoutubeDescription, youtubeChapters, youtubeTitle, MAX_DESCRIPTION_BYTES } from "./youtube";

const bytes = (s: string) => new TextEncoder().encode(s).length;
const ch = (time: number, title: string) => ({ time, title });
const chapters = [ch(100, "Welcome"), ch(160, "The Word"), ch(400, "Prayer")];

describe("youtubeTitle", () => {
  it("strips < and > and caps at 100 characters", () => {
    expect(youtubeTitle("  <Hope>  in\nHim ")).toBe("Hope in Him");
    const long = "é".repeat(150);
    expect(Array.from(youtubeTitle(long))).toHaveLength(100);
  });
});

describe("youtubeChapters", () => {
  it("keeps the trim's chapters, timed from its start, the first at 0:00", () => {
    expect(youtubeChapters([ch(50, "Before"), ...chapters, ch(900, "After")], 90, 600)).toEqual([
      ch(0, "Welcome"),
      ch(70, "The Word"),
      ch(310, "Prayer"),
    ]);
  });

  it("drops chapters closer than 10 s, and needs at least 3", () => {
    expect(youtubeChapters([ch(0, "A"), ch(5, "B"), ch(20, "C"), ch(40, "D")], 0, 100)).toEqual([
      ch(0, "A"),
      ch(20, "C"),
      ch(40, "D"),
    ]);
    expect(youtubeChapters([ch(0, "A"), ch(5, "B"), ch(20, "C")], 0, 100)).toEqual([]);
    // The last chapter must last 10 s too.
    expect(youtubeChapters([ch(0, "A"), ch(20, "B"), ch(95, "C")], 0, 100)).toEqual([]);
  });
});

describe("buildYoutubeDescription", () => {
  const base = { chapters, startTime: 90, endTime: 600, footer: "Website: https://x" };

  it("puts the text, chapters and footer together", () => {
    expect(buildYoutubeDescription({ ...base, description: "About <God>." })).toBe(
      "About God.\n\nChapters:\n0:00 Welcome\n1:10 The Word\n5:10 Prayer\n\n---\nWebsite: https://x"
    );
  });

  it("leaves chapters out when there are too few", () => {
    expect(buildYoutubeDescription({ ...base, chapters: chapters.slice(0, 2), description: "Hi" })).toBe(
      "Hi\n\n---\nWebsite: https://x"
    );
  });

  it("shortens the text, never the chapters, to fit 5000 bytes", () => {
    const description = "ü".repeat(4000); // 8000 bytes
    const out = buildYoutubeDescription({ ...base, description });
    expect(bytes(out)).toBeLessThanOrEqual(MAX_DESCRIPTION_BYTES);
    expect(bytes(out)).toBeGreaterThan(MAX_DESCRIPTION_BYTES - 4);
    expect(out).toContain("ü…\n\nChapters:\n0:00 Welcome\n1:10 The Word\n5:10 Prayer\n\n---\nWebsite: https://x");
    expect(out.endsWith("Website: https://x")).toBe(true);
  });

  it("drops the footer before cutting chapters", () => {
    const many = Array.from({ length: 110 }, (_, i) => ch(i * 20, `Point number ${i} of the sermon`));
    const footer = "x".repeat(1500);
    const out = buildYoutubeDescription({ description: "Text", chapters: many, startTime: 0, endTime: 5000, footer });
    expect(bytes(out)).toBeLessThanOrEqual(MAX_DESCRIPTION_BYTES);
    expect(out).not.toContain("---");
    expect(out.endsWith("36:20 Point number 109 of the sermon")).toBe(true);
    // Chapters that can't fit at all are left out whole, not cut.
    const huge = Array.from({ length: 300 }, (_, i) => ch(i * 20, `Point number ${i} of the sermon`));
    const none = buildYoutubeDescription({ description: "Text", chapters: huge, startTime: 0, endTime: 9000 });
    expect(none).toBe("Text");
  });
});
