import type { CropRect } from "@/lib/types";

/** A rectangle in image pixels; fractional while editing. */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export const THUMBNAIL_ASPECT = 16 / 9;

/** The whole image fitted to `aspect`: the largest centred crop. */
export function defaultCrop(imageWidth: number, imageHeight: number, aspect = THUMBNAIL_ASPECT): Rect {
  const width = Math.min(imageWidth, imageHeight * aspect);
  const height = width / aspect;
  return { x: (imageWidth - width) / 2, y: (imageHeight - height) / 2, width, height };
}

/**
 * `crop` made valid for an image: `aspect` kept, no bigger than the image
 * fitted to `aspect` (shrunk about its centre) and no smaller than
 * `minWidth`, then moved inside the image. What the editor shows is always
 * exactly what gets cut.
 */
export function clampCropToImage(
  crop: Rect,
  imageWidth: number,
  imageHeight: number,
  aspect = THUMBNAIL_ASPECT,
  minWidth = 1,
): Rect {
  const maxWidth = Math.min(imageWidth, imageHeight * aspect);
  const width = clamp(crop.width, Math.min(minWidth, maxWidth), maxWidth);
  const height = width / aspect;
  const cx = crop.x + crop.width / 2;
  const cy = crop.y + crop.height / 2;
  return {
    x: clamp(cx - width / 2, 0, imageWidth - width),
    y: clamp(cy - height / 2, 0, imageHeight - height),
    width,
    height,
  };
}

/** Whole pixels for the backend, still inside the image. */
export function toPixelCrop(crop: Rect, imageWidth: number, imageHeight: number): CropRect {
  const x = clamp(Math.round(crop.x), 0, Math.max(0, imageWidth - 1));
  const y = clamp(Math.round(crop.y), 0, Math.max(0, imageHeight - 1));
  return {
    x,
    y,
    width: clamp(Math.round(crop.width), 1, imageWidth - x),
    height: clamp(Math.round(crop.height), 1, imageHeight - y),
  };
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}
