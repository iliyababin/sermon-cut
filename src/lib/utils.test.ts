import { describe, expect, it } from "vitest";
import { rawThumbnailVariant, splitTitleIntoLines } from "./utils";

describe("utils", () => {
  it("maps generated options to their raw crops", () => {
    expect(rawThumbnailVariant("/v/thumbnail_option_3.jpg")).toBe("/v/thumbnail_option_3_raw.jpg");
    expect(rawThumbnailVariant("/v/custom_thumbnail_1.jpg")).toBe("/v/custom_thumbnail_1.jpg");
  });

  it("splits a title into two lines", () => {
    expect(splitTitleIntoLines("Faith")).toEqual(["Faith"]);
    expect(splitTitleIntoLines("The Hope We Have")).toEqual(["The Hope", "We Have"]);
  });
});
