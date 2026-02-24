import { useState, useRef, useCallback, useEffect } from "react";
import type { CropRect } from "@/lib/types";

interface CanvasEditorState {
  zoom: number;
  panX: number;
  panY: number;
}

interface UseCanvasEditorOptions {
  aspectRatio?: number;
  minZoom?: number;
  maxZoom?: number;
}

interface ImageDimensions {
  width: number;
  height: number;
}

export function useCanvasEditor(options: UseCanvasEditorOptions = {}) {
  const { aspectRatio = 16 / 9, minZoom = 0.1, maxZoom = 5 } = options;

  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const imageRef = useRef<HTMLImageElement | null>(null);
  const [imageDimensions, setImageDimensions] = useState<ImageDimensions | null>(null);
  const [state, setState] = useState<CanvasEditorState>({
    zoom: 1,
    panX: 0,
    panY: 0,
  });
  const [isDragging, setIsDragging] = useState(false);
  const [dragStart, setDragStart] = useState({ x: 0, y: 0 });
  const [imageLoaded, setImageLoaded] = useState(false);

  // Load image from path (base64 data URL)
  const loadImage = useCallback((imageDataUrl: string) => {
    const img = new Image();
    img.onload = () => {
      imageRef.current = img;
      setImageDimensions({ width: img.width, height: img.height });
      setImageLoaded(true);

      // Calculate initial zoom to fit image in view while maintaining aspect ratio
      if (canvasRef.current) {
        const canvas = canvasRef.current;
        const canvasAspect = canvas.width / canvas.height;
        const imageAspect = img.width / img.height;

        // Start with the image filling the canvas
        let initialZoom: number;
        if (imageAspect > canvasAspect) {
          // Image is wider than canvas - fit by width
          initialZoom = canvas.width / img.width;
        } else {
          // Image is taller than canvas - fit by height
          initialZoom = canvas.height / img.height;
        }

        // Center the image
        const scaledWidth = img.width * initialZoom;
        const scaledHeight = img.height * initialZoom;
        const panX = (canvas.width - scaledWidth) / 2;
        const panY = (canvas.height - scaledHeight) / 2;

        setState({
          zoom: initialZoom,
          panX,
          panY,
        });
      }
    };
    img.onerror = () => {
      console.error("Failed to load image");
      setImageLoaded(false);
    };
    img.src = imageDataUrl;
  }, []);

  // Draw the canvas
  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    const img = imageRef.current;

    if (!canvas || !ctx || !img) return;

    // Clear canvas
    ctx.fillStyle = "#1a1a1a";
    ctx.fillRect(0, 0, canvas.width, canvas.height);

    // Draw image with current pan and zoom
    const scaledWidth = img.width * state.zoom;
    const scaledHeight = img.height * state.zoom;

    ctx.drawImage(
      img,
      state.panX,
      state.panY,
      scaledWidth,
      scaledHeight
    );

    // Draw crop overlay
    drawCropOverlay(ctx, canvas.width, canvas.height);
  }, [state, aspectRatio]);

  // Draw crop overlay with dimmed areas outside the crop
  const drawCropOverlay = useCallback(
    (ctx: CanvasRenderingContext2D, canvasWidth: number, canvasHeight: number) => {
      // Calculate crop area (centered 16:9 rectangle)
      const cropRect = calculateCropRectInCanvas(canvasWidth, canvasHeight);

      // Draw semi-transparent overlay on areas outside crop
      ctx.fillStyle = "rgba(0, 0, 0, 0.5)";

      // Top
      ctx.fillRect(0, 0, canvasWidth, cropRect.y);
      // Bottom
      ctx.fillRect(0, cropRect.y + cropRect.height, canvasWidth, canvasHeight - cropRect.y - cropRect.height);
      // Left
      ctx.fillRect(0, cropRect.y, cropRect.x, cropRect.height);
      // Right
      ctx.fillRect(cropRect.x + cropRect.width, cropRect.y, canvasWidth - cropRect.x - cropRect.width, cropRect.height);

      // Draw crop border
      ctx.strokeStyle = "#fff";
      ctx.lineWidth = 2;
      ctx.strokeRect(cropRect.x, cropRect.y, cropRect.width, cropRect.height);

      // Draw rule of thirds guides
      ctx.strokeStyle = "rgba(255, 255, 255, 0.3)";
      ctx.lineWidth = 1;
      const thirdW = cropRect.width / 3;
      const thirdH = cropRect.height / 3;
      for (let i = 1; i < 3; i++) {
        // Vertical lines
        ctx.beginPath();
        ctx.moveTo(cropRect.x + thirdW * i, cropRect.y);
        ctx.lineTo(cropRect.x + thirdW * i, cropRect.y + cropRect.height);
        ctx.stroke();
        // Horizontal lines
        ctx.beginPath();
        ctx.moveTo(cropRect.x, cropRect.y + thirdH * i);
        ctx.lineTo(cropRect.x + cropRect.width, cropRect.y + thirdH * i);
        ctx.stroke();
      }
    },
    [aspectRatio]
  );

  // Calculate crop rectangle in canvas coordinates
  const calculateCropRectInCanvas = useCallback(
    (canvasWidth: number, canvasHeight: number) => {
      const canvasAspect = canvasWidth / canvasHeight;

      let cropWidth: number;
      let cropHeight: number;

      if (aspectRatio > canvasAspect) {
        // Crop is wider than canvas - fit by width
        cropWidth = canvasWidth * 0.9;
        cropHeight = cropWidth / aspectRatio;
      } else {
        // Crop is taller than canvas - fit by height
        cropHeight = canvasHeight * 0.9;
        cropWidth = cropHeight * aspectRatio;
      }

      const cropX = (canvasWidth - cropWidth) / 2;
      const cropY = (canvasHeight - cropHeight) / 2;

      return { x: cropX, y: cropY, width: cropWidth, height: cropHeight };
    },
    [aspectRatio]
  );

  // Convert canvas crop to source image coordinates
  const getCropRect = useCallback((): CropRect | null => {
    const canvas = canvasRef.current;
    const img = imageRef.current;

    if (!canvas || !img) return null;

    const cropInCanvas = calculateCropRectInCanvas(canvas.width, canvas.height);

    // Convert canvas coordinates to image coordinates
    // The image is drawn at (panX, panY) with scale (zoom)
    const sourceX = (cropInCanvas.x - state.panX) / state.zoom;
    const sourceY = (cropInCanvas.y - state.panY) / state.zoom;
    const sourceWidth = cropInCanvas.width / state.zoom;
    const sourceHeight = cropInCanvas.height / state.zoom;

    // Clamp to image bounds
    const clampedX = Math.max(0, Math.round(sourceX));
    const clampedY = Math.max(0, Math.round(sourceY));
    const clampedWidth = Math.min(img.width - clampedX, Math.round(sourceWidth));
    const clampedHeight = Math.min(img.height - clampedY, Math.round(sourceHeight));

    return {
      x: clampedX,
      y: clampedY,
      width: Math.max(1, clampedWidth),
      height: Math.max(1, clampedHeight),
    };
  }, [state, calculateCropRectInCanvas]);

  // Zoom controls
  const setZoom = useCallback(
    (newZoom: number) => {
      const canvas = canvasRef.current;
      if (!canvas) return;

      const clampedZoom = Math.min(maxZoom, Math.max(minZoom, newZoom));

      // Zoom toward center of canvas
      const centerX = canvas.width / 2;
      const centerY = canvas.height / 2;

      const zoomRatio = clampedZoom / state.zoom;

      setState((prev) => ({
        ...prev,
        zoom: clampedZoom,
        panX: centerX - (centerX - prev.panX) * zoomRatio,
        panY: centerY - (centerY - prev.panY) * zoomRatio,
      }));
    },
    [state.zoom, minZoom, maxZoom]
  );

  const zoomIn = useCallback(() => {
    setZoom(state.zoom * 1.2);
  }, [state.zoom, setZoom]);

  const zoomOut = useCallback(() => {
    setZoom(state.zoom / 1.2);
  }, [state.zoom, setZoom]);

  // Center the image in the crop area
  const center = useCallback(() => {
    const canvas = canvasRef.current;
    const img = imageRef.current;

    if (!canvas || !img) return;

    const scaledWidth = img.width * state.zoom;
    const scaledHeight = img.height * state.zoom;

    setState((prev) => ({
      ...prev,
      panX: (canvas.width - scaledWidth) / 2,
      panY: (canvas.height - scaledHeight) / 2,
    }));
  }, [state.zoom]);

  // Reset to initial state
  const reset = useCallback(() => {
    const canvas = canvasRef.current;
    const img = imageRef.current;

    if (!canvas || !img) return;

    const canvasAspect = canvas.width / canvas.height;
    const imageAspect = img.width / img.height;

    let initialZoom: number;
    if (imageAspect > canvasAspect) {
      initialZoom = canvas.width / img.width;
    } else {
      initialZoom = canvas.height / img.height;
    }

    const scaledWidth = img.width * initialZoom;
    const scaledHeight = img.height * initialZoom;

    setState({
      zoom: initialZoom,
      panX: (canvas.width - scaledWidth) / 2,
      panY: (canvas.height - scaledHeight) / 2,
    });
  }, []);

  // Mouse event handlers
  const handleMouseDown = useCallback((e: React.MouseEvent<HTMLCanvasElement>) => {
    setIsDragging(true);
    setDragStart({ x: e.clientX, y: e.clientY });
  }, []);

  const handleMouseMove = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
      if (!isDragging) return;

      const dx = e.clientX - dragStart.x;
      const dy = e.clientY - dragStart.y;

      setState((prev) => ({
        ...prev,
        panX: prev.panX + dx,
        panY: prev.panY + dy,
      }));

      setDragStart({ x: e.clientX, y: e.clientY });
    },
    [isDragging, dragStart]
  );

  const handleMouseUp = useCallback(() => {
    setIsDragging(false);
  }, []);

  const handleMouseLeave = useCallback(() => {
    setIsDragging(false);
  }, []);

  const handleWheel = useCallback(
    (e: React.WheelEvent<HTMLCanvasElement>) => {
      e.preventDefault();
      const delta = e.deltaY > 0 ? 0.9 : 1.1;
      setZoom(state.zoom * delta);
    },
    [state.zoom, setZoom]
  );

  // Redraw when state changes
  useEffect(() => {
    if (imageLoaded) {
      draw();
    }
  }, [draw, imageLoaded, state]);

  return {
    canvasRef,
    loadImage,
    zoom: state.zoom,
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
      onWheel: handleWheel,
    },
  };
}
