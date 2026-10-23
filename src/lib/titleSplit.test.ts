import { describe, expect, it } from "vitest";
import { balanceLines, estimateWidth } from "./titleSplit";
import { splitTitleIntoLines } from "./utils";

describe("balanceLines", () => {
  it("balances by width, not word count", () => {
    // Word count would split 2/2 ("I Am / Wonderfully Made"); by width
    // the widest line is narrower than that.
    const [top, bottom] = balanceLines("I Am Wonderfully Made", 2);
    expect(Math.max(estimateWidth(top), estimateWidth(bottom))).toBeLessThanOrEqual(
      Math.max(estimateWidth("I Am"), estimateWidth("Wonderfully Made")),
    );
    expect(balanceLines("Go Be Unstoppable", 2)).toEqual(["Go Be", "Unstoppable"]);
  });

  it("splits a very long title into three even lines", () => {
    const lines = balanceLines("Walking in the Spirit When Life Feels Overwhelming and Nothing Makes Sense", 3);
    expect(lines).toHaveLength(3);
    expect(lines.join(" ")).toBe("Walking in the Spirit When Life Feels Overwhelming and Nothing Makes Sense");
    const widths = lines.map(estimateWidth);
    expect(Math.max(...widths) / Math.min(...widths)).toBeLessThan(1.4);
  });

  it("never makes more lines than words", () => {
    expect(balanceLines("Faith Alone", 3)).toEqual(["Faith", "Alone"]);
    expect(balanceLines("Faith", 2)).toEqual(["Faith"]);
    expect(balanceLines("   ", 2)).toEqual([]);
  });

  it("uses the measure it's given", () => {
    const byLength = (s: string) => s.length;
    expect(balanceLines("aaaa b c d", 2, byLength)).toEqual(["aaaa", "b c d"]);
  });
});

describe("splitTitleIntoLines", () => {
  it("keeps two words on one line and balances longer titles", () => {
    expect(splitTitleIntoLines("Faith Alone")).toEqual(["Faith Alone"]);
    expect(splitTitleIntoLines("The Hope We Have")).toEqual(["The Hope", "We Have"]);
    expect(splitTitleIntoLines("  The   Hope We Have ")).toEqual(["The Hope", "We Have"]);
  });
});
