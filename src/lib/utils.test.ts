import { describe, expect, it } from "vitest";
import { duplicateFileNames, formatFileSize, rawThumbnailVariant, splitTitleIntoLines } from "./utils";

describe("utils", () => {
  it("maps generated options to their raw crops", () => {
    expect(rawThumbnailVariant("/v/thumbnail_option_3.jpg")).toBe("/v/thumbnail_option_3_raw.jpg");
    expect(rawThumbnailVariant("/v/custom_thumbnail_1.jpg")).toBe("/v/custom_thumbnail_1.jpg");
  });

  it("splits a title into two lines", () => {
    expect(splitTitleIntoLines("Faith")).toEqual(["Faith"]);
    expect(splitTitleIntoLines("The Hope We Have")).toEqual(["The Hope", "We Have"]);
  });

  it("formats file sizes", () => {
    expect(formatFileSize(512)).toBe("512 bytes");
    expect(formatFileSize(1_500_000)).toBe("1.5 MB");
    expect(formatFileSize(2_340_000_000)).toBe("2.3 GB");
    expect(formatFileSize(350_000_000)).toBe("350 MB");
  });

  it("finds shared file names, NFC and NFD alike", () => {
    const dupes = duplicateFileNames([
      { file_name: "Caf\u00e9.mp4" },
      { file_name: "Cafe\u0301.mp4" },
      { file_name: "Other.mp4" },
    ]);
    expect([...dupes]).toEqual(["Caf\u00e9.mp4"]);
  });
});
