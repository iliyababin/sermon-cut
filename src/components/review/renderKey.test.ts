import { describe, expect, it } from "vitest";
import { renderKey, renderKeyFor } from "./renderKey";

const option = { id: "thumbnail_option_1.jpg", version: "100" };
const inputs = { title: "The Hope We Have", preacher: "Pastor X", singleLine: false };

describe("renderKey", () => {
  it("is stable and plain ASCII", () => {
    expect(renderKey(option, inputs)).toBe(renderKey({ ...option }, { ...inputs }));
    expect(renderKey(option, inputs)).toMatch(/^[0-9a-f]+$/);
  });

  it("changes with anything that changes the render", () => {
    const base = renderKey(option, inputs);
    expect(renderKey({ ...option, id: "thumbnail_option_2.jpg" }, inputs)).not.toBe(base);
    expect(renderKey({ ...option, version: "101" }, inputs)).not.toBe(base);
    expect(renderKey(option, { ...inputs, title: "The Hope" })).not.toBe(base);
    expect(renderKey(option, { ...inputs, preacher: "Pastor Y" })).not.toBe(base);
    expect(renderKey(option, { ...inputs, singleLine: true })).not.toBe(base);
  });

  it("ignores whitespace around the speaker", () => {
    expect(renderKey(option, { ...inputs, preacher: " Pastor X " })).toBe(renderKey(option, inputs));
  });

  it("matches the backend's key (manifest::render_key)", () => {
    // The same vector is in src-tauri/src/library/manifest.rs.
    const key = renderKeyFor(
      4,
      { id: "custom_thumbnail_1.jpg", version: "1730000000000" },
      { title: 'Héllo "world"\n\t😀 \\ </>', preacher: "\u00a0Pastor Ü\ufeff ", singleLine: true },
    );
    expect(key).toBe(VECTOR);
  });
});

const VECTOR = "1a4ef5393622d8";
