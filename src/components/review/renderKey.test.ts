import { describe, expect, it } from "vitest";
import { renderKey } from "./renderKey";

const option = { id: "thumbnail_option_1.jpg", version: "100" };
const inputs = { titleLines: ["The Hope", "We Have"], preacher: "Pastor X", singleLine: false };

describe("renderKey", () => {
  it("is stable and plain ASCII", () => {
    expect(renderKey(option, inputs)).toBe(renderKey({ ...option }, { ...inputs }));
    expect(renderKey(option, inputs)).toMatch(/^[0-9a-f]+$/);
  });

  it("changes with anything that changes the render", () => {
    const base = renderKey(option, inputs);
    expect(renderKey({ ...option, id: "thumbnail_option_2.jpg" }, inputs)).not.toBe(base);
    expect(renderKey({ ...option, version: "101" }, inputs)).not.toBe(base);
    expect(renderKey(option, { ...inputs, titleLines: ["The Hope We Have"] })).not.toBe(base);
    expect(renderKey(option, { ...inputs, preacher: "Pastor Y" })).not.toBe(base);
    expect(renderKey(option, { ...inputs, singleLine: true })).not.toBe(base);
  });

  it("ignores whitespace around the speaker", () => {
    expect(renderKey(option, { ...inputs, preacher: " Pastor X " })).toBe(renderKey(option, inputs));
  });
});
