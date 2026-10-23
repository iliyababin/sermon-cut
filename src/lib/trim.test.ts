import { describe, expect, it } from "vitest";
import { formatPrecise, lineAt, nudge } from "./trim";

describe("formatPrecise", () => {
  it("shows tenths of a second", () => {
    expect(formatPrecise(0)).toBe("0:00.0");
    expect(formatPrecise(723.46)).toBe("12:03.4");
    expect(formatPrecise(3723.4)).toBe("1:02:03.4");
    expect(formatPrecise(59.99)).toBe("0:59.9");
  });
});

describe("lineAt", () => {
  const segments = [
    { start: 0, end: 4, text: "Welcome" },
    { start: 5, end: 9, text: "Let's pray" },
  ];
  it("finds the line being said", () => {
    expect(lineAt(segments, 6)?.text).toBe("Let's pray");
    expect(lineAt(segments, 0)?.text).toBe("Welcome");
  });
  it("falls back to the last line before a pause", () => {
    expect(lineAt(segments, 4.5)?.text).toBe("Welcome");
    expect(lineAt(segments, 100)?.text).toBe("Let's pray");
  });
  it("is empty before anything is said", () => {
    expect(lineAt([{ start: 3, end: 4, text: "x" }], 1)).toBeNull();
    expect(lineAt([], 1)).toBeNull();
  });
});

describe("nudge", () => {
  it("moves within the video", () => {
    expect(nudge(10, 5, 0, 60)).toBe(15);
    expect(nudge(3, -5, 0, 60)).toBe(0);
    expect(nudge(58, 5, 0, 60)).toBe(60);
    expect(nudge(0.1, 0.2, 0, 60)).toBe(0.3);
  });
});
