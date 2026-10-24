import { describe, expect, it } from "vitest";
import css from "./index.css?raw";

describe("index.css", () => {
  it("gives every element only colours html2canvas can read", () => {
    // Rules on * reach the thumbnail template, and html2canvas fails the
    // whole render on oklch() (Tailwind 4's palette) or color-mix().
    const base = css.slice(css.indexOf("@layer base"));
    expect(base).not.toMatch(/oklch|color-mix|var\(--color-gray/);
  });

  it("keeps the reset in the base layer, so utility classes still win", () => {
    const unlayered = css.replace(/@layer base \{[\s\S]*?\n\}/g, "");
    expect(unlayered).not.toMatch(/^\* \{/m);
  });
});
