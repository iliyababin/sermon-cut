import { useState, useRef, useCallback, useEffect } from "react";
import type { CropRect } from "@/lib/types";
import { clampCropToImage, defaultCrop, toPixelCrop, type Rect } from "@/lib/crop";

interface UseCanvasEditorOptions {
  aspectRatio?: number;
  /** How far in you can zoom, relative to the whole image. */
  maxZoom?: number;
}

interface ImageDimensions {
  width: number;
  height: number;
}

/**
 * A crop editor on a canvas. The state is the crop itself, in image pixels,
 * always kept inside the image (`clampCropToImage`); the canvas draws the
 * image so that the crop fills a fixed frame. So what's in the frame is
 * exactly what gets cut, whatever the canvas size.
 *
 * `canvasRef` is a callback ref: the canvas may mount later (or be
 * replaced), and it is sized from its CSS box times devicePixelRatio,
 * redrawn whenever that box changes.
 */
export function useCanvasEditor(options: UseCanvasEditorOptions = {}) {
  const { aspectRatio = 16 / 9, maxZoom = 5 } = options;

  const [canvas, setCanvas] = useState<HTMLCanvasElement | null>(null);
  // The canvas's CSS size; drawing works in CSS pixels.
  const [size, setSize] = useState({ width: 0, height: 0, dpr: 1 });
  const canvasRef = useCallback((node: HTMLCanvasElement | null) => {
    setCanvas(node);
  }, []);
  const imageRef = useRef<HTMLImageElement | null>(null);
  const [imageDimensions, setImageDimensions] = useState<ImageDimensions | null>(null);
  const [crop, setCrop] = useState<Rect | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [dragStart, setDragStart] = useState({ x: 0, y: 0 });
  const [imageLoaded, setImageLoaded] = useState(false);

  /** The crop made valid for the loaded image. */
  const clampToImage = useCallback(
    (rect: Rect): Rect => {
      const img = imageRef.current;
      if (!img) return rect;
      const full = defaultCrop(img.width, img.height, aspectRatio);
      return clampCropToImage(rect, img.width, img.height, aspectRatio, full.width / maxZoom);
    },
    [aspectRatio, maxZoom]
  );

  // Load image from path (base64 data URL); starts on the whole image.
  const loadImage = useCallback(
    (imageDataUrl: string) => {
      const img = new Image();
      img.onload = () => {
        imageRef.current = img;
        setImageDimensions({ width: img.width, height: img.height });
        setCrop(defaultCrop(img.width, img.height, aspectRatio));
        setImageLoaded(true);
      };
      img.onerror = () => {
        console.error("Failed to load image");
        setImageLoaded(false);
      };
      img.src = imageDataUrl;
    },
    [aspectRatio]
  );

  /** The crop frame on a canvas of this size: a centred box with a margin. */
  const frameIn = useCallback(
    (canvasWidth: number, canvasHeight: number): Rect => {
      let width = canvasWidth * 0.9;
      let height = width / aspectRatio;
      if (height > canvasHeight * 0.9) {
        height = canvasHeight * 0.9;
        width = height * aspectRatio;
      }
      return { x: (canvasWidth - width) / 2, y: (canvasHeight - height) / 2, width, height };
    },
    [aspectRatio]
  );

  /** CSS pixels per image pixel. */
  const scaleOf = useCallback(
    (c: Rect): number => (size.width > 0 ? frameIn(size.width, size.height).width / c.width : 1),
    [frameIn, size]
  );

  // Draw the canvas
  const draw = useCallback(() => {
    const ctx = canvas?.getContext("2d");
    const img = imageRef.current;
    if (!canvas || !ctx || !img || !crop || size.width === 0) return;

    ctx.setTransform(size.dpr, 0, 0, size.dpr, 0, 0);
    const w = size.width;
    const h = size.height;
    const frame = frameIn(w, h);
    const scale = frame.width / crop.width;

    ctx.fillStyle = "#1a1a1a";
    ctx.fillRect(0, 0, w, h);
    ctx.drawImage(img, frame.x - crop.x * scale, frame.y - crop.y * scale, img.width * scale, img.height * scale);

    // Dim outside the frame
    ctx.fillStyle = "rgba(0, 0, 0, 0.5)";
    ctx.fillRect(0, 0, w, frame.y);
    ctx.fillRect(0, frame.y + frame.height, w, h - frame.y - frame.height);
    ctx.fillRect(0, frame.y, frame.x, frame.height);
    ctx.fillRect(frame.x + frame.width, frame.y, w - frame.x - frame.width, frame.height);

    ctx.strokeStyle = "#fff";
    ctx.lineWidth = 2;
    ctx.strokeRect(frame.x, frame.y, frame.width, frame.height);

    // Rule of thirds
    ctx.strokeStyle = "rgba(255, 255, 255, 0.3)";
    ctx.lineWidth = 1;
    for (let i = 1; i < 3; i++) {
      ctx.beginPath();
      ctx.moveTo(frame.x + (frame.width / 3) * i, frame.y);
      ctx.lineTo(frame.x + (frame.width / 3) * i, frame.y + frame.height);
      ctx.stroke();
      ctx.beginPath();
      ctx.moveTo(frame.x, frame.y + (frame.height / 3) * i);
      ctx.lineTo(frame.x + frame.width, frame.y + (frame.height / 3) * i);
      ctx.stroke();
    }
  }, [canvas, crop, frameIn, size]);

  // The crop in source image pixels, for the backend.
  const getCropRect = useCallback((): CropRect | null => {
    const img = imageRef.current;
    if (!img || !crop) return null;
    return toPixelCrop(crop, img.width, img.height);
  }, [crop]);

  /** 1 = the whole image; up to maxZoom. */
  const zoom = (() => {
    const img = imageRef.current;
    if (!img || !crop) return 1;
    return defaultCrop(img.width, img.height, aspectRatio).width / crop.width;
  })();

  // Zoom about the crop's centre
  const setZoom = useCallback(
    (newZoom: number) => {
      const img = imageRef.current;
      if (!img) return;
      const width = defaultCrop(img.width, img.height, aspectRatio).width / Math.min(maxZoom, Math.max(1, newZoom));
      setCrop((prev) => {
        if (!prev) return prev;
        const height = width / aspectRatio;
        const cx = prev.x + prev.width / 2;
        const cy = prev.y + prev.height / 2;
        return clampToImage({ x: cx - width / 2, y: cy - height / 2, width, height });
      });
    },
    [aspectRatio, maxZoom, clampToImage]
  );

  const zoomIn = useCallback(() => setZoom(zoom * 1.2), [zoom, setZoom]);
  const zoomOut = useCallback(() => setZoom(zoom / 1.2), [zoom, setZoom]);

  // Centre the crop on the image, keeping its size
  const center = useCallback(() => {
    const img = imageRef.current;
    if (!img) return;
    setCrop((prev) =>
      prev ? clampToImage({ ...prev, x: (img.width - prev.width) / 2, y: (img.height - prev.height) / 2 }) : prev
    );
  }, [clampToImage]);

  // Back to the whole image
  const reset = useCallback(() => {
    const img = imageRef.current;
    if (!img) return;
    setCrop(defaultCrop(img.width, img.height, aspectRatio));
  }, [aspectRatio]);

  // Dragging moves the image under the frame
  const handleMouseDown = useCallback((e: React.MouseEvent<HTMLCanvasElement>) => {
    setIsDragging(true);
    setDragStart({ x: e.clientX, y: e.clientY });
  }, []);

  const handleMouseMove = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
      if (!isDragging) return;
      const dx = e.clientX - dragStart.x;
      const dy = e.clientY - dragStart.y;
      setCrop((prev) => {
        if (!prev) return prev;
        const scale = scaleOf(prev);
        return clampToImage({ ...prev, x: prev.x - dx / scale, y: prev.y - dy / scale });
      });
      setDragStart({ x: e.clientX, y: e.clientY });
    },
    [isDragging, dragStart, scaleOf, clampToImage]
  );

  const handleMouseUp = useCallback(() => setIsDragging(false), []);
  const handleMouseLeave = useCallback(() => setIsDragging(false), []);

  // Wheel zoom. React's onWheel is passive, so it can't stop the page
  // scrolling; a native non-passive listener can.
  const onWheel = useRef((_deltaY: number) => {});
  onWheel.current = (deltaY: number) => setZoom(zoom * (deltaY > 0 ? 0.9 : 1.1));

  // Size the backing store to the CSS box at the display's pixel ratio.
  useEffect(() => {
    if (!canvas) return;
    const resize = () => {
      const dpr = window.devicePixelRatio || 1;
      const width = canvas.clientWidth;
      const height = canvas.clientHeight;
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      setSize({ width, height, dpr });
    };
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      onWheel.current(e.deltaY);
    };
    canvas.addEventListener("wheel", wheel, { passive: false });
    return () => {
      observer.disconnect();
      canvas.removeEventListener("wheel", wheel);
    };
  }, [canvas]);

  // Redraw when the crop or the canvas size changes
  useEffect(() => {
    if (imageLoaded) draw();
  }, [draw, imageLoaded]);

  return {
    canvasRef,
    loadImage,
    zoom,
    setZoom,
    zoomIn,
    zoomOut,
    center,
    reset,
    getCropRect,
    imageLoaded,
    imageDimensions,
    handlers: {
      onMouseDown: handleMouseDown,
      onMouseMove: handleMouseMove,
      onMouseUp: handleMouseUp,
      onMouseLeave: handleMouseLeave,
    },
  };
}
