import { describe, expect, it } from "vitest";
import { clampCropToImage, defaultCrop, toPixelCrop } from "./crop";

describe("defaultCrop", () => {
  it("is the whole width of a tall image, centred", () => {
    expect(defaultCrop(1600, 1200)).toEqual({ x: 0, y: 150, width: 1600, height: 900 });
  });

  it("is the whole height of a wide image, centred", () => {
    expect(defaultCrop(4000, 900)).toEqual({ x: 1200, y: 0, width: 1600, height: 900 });
  });

  it("is the whole image when it's already 16:9", () => {
    expect(defaultCrop(1920, 1080)).toEqual({ x: 0, y: 0, width: 1920, height: 1080 });
  });
});

describe("clampCropToImage", () => {
  it("leaves a crop that fits alone", () => {
    const crop = { x: 100, y: 50, width: 800, height: 450 };
    expect(clampCropToImage(crop, 1920, 1080)).toEqual(crop);
  });

  it("moves a crop hanging off an edge back inside", () => {
    expect(clampCropToImage({ x: -40, y: 900, width: 800, height: 450 }, 1920, 1080)).toEqual({
      x: 0,
      y: 630,
      width: 800,
      height: 450,
    });
    expect(clampCropToImage({ x: 1500, y: -10, width: 800, height: 450 }, 1920, 1080)).toEqual({
      x: 1120,
      y: 0,
      width: 800,
      height: 450,
    });
  });

  it("shrinks a crop bigger than the image, keeping 16:9", () => {
    const out = clampCropToImage({ x: -800, y: -300, width: 3200, height: 1800 }, 1600, 1200);
    expect(out).toEqual({ x: 0, y: 150, width: 1600, height: 900 });
  });

  it("never goes below the minimum width", () => {
    const out = clampCropToImage({ x: 500, y: 500, width: 10, height: 10 / (16 / 9) }, 1920, 1080, 16 / 9, 192);
    expect(out.width).toBe(192);
    expect(out.height).toBe(108);
    // Grown about its centre.
    expect(out.x).toBeCloseTo(505 - 96);
  });
});

describe("toPixelCrop", () => {
  it("rounds and stays inside the image", () => {
    expect(toPixelCrop({ x: 0.4, y: 149.6, width: 1599.7, height: 899.9 }, 1600, 1200)).toEqual({
      x: 0,
      y: 150,
      width: 1600,
      height: 900,
    });
    expect(toPixelCrop({ x: 320.6, y: 0, width: 1600, height: 900 }, 1920, 1080).width).toBe(1599);
  });
});
