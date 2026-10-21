import html2canvas from "html2canvas";
import { convertFileSrc } from "@tauri-apps/api/core";
import type { TemplateConfig, TemplateData } from "@/templates/types";

async function ensureFontsLoaded(): Promise<void> {
  await document.fonts.load("700 96px 'Baskervville'");
  await document.fonts.load("400 51px 'Baskervville'");
  await document.fonts.ready;
  // Small delay for the DOM to paint with custom fonts
  await new Promise((r) => setTimeout(r, 50));
}

function waitForImageLoad(container: HTMLElement): Promise<void> {
  const bgDiv = container.querySelector(".bg-image") as HTMLElement | null;
  if (!bgDiv) return Promise.resolve();

  const bgUrl = bgDiv.style.backgroundImage.replace(/url\(['"]?(.+?)['"]?\)/, "$1");
  if (!bgUrl) return Promise.resolve();

  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve();
    img.onerror = () => resolve(); // Don't block on error
    img.src = bgUrl;
  });
}

export interface RenderResult {
  imageData: Uint8Array;
  index: number;
}

export async function renderThumbnailBatch(
  template: TemplateConfig,
  baseImagePaths: string[],
  titleLines: string[],
  speakerName: string,
  onProgress?: (current: number, total: number) => void,
): Promise<RenderResult[]> {
  // Create hidden container
  const host = document.createElement("div");
  host.style.position = "fixed";
  host.style.left = "-9999px";
  host.style.top = "0";
  host.style.zIndex = "-1";
  document.body.appendChild(host);

  // Inject font-face rules into document head so the browser loads them
  const fontStyle = document.createElement("style");
  fontStyle.textContent = `
    @font-face {
      font-family: 'Baskervville';
      font-style: normal;
      font-weight: 700;
      src: url('/fonts/Baskervville-Bold.woff2') format('woff2');
    }
    @font-face {
      font-family: 'Baskervville';
      font-style: normal;
      font-weight: 400;
      src: url('/fonts/Baskervville-Regular.woff2') format('woff2');
    }
  `;
  document.head.appendChild(fontStyle);

  await ensureFontsLoaded();

  const results: RenderResult[] = [];

  try {
    for (let i = 0; i < baseImagePaths.length; i++) {
      onProgress?.(i + 1, baseImagePaths.length);

      const assetUrl = convertFileSrc(baseImagePaths[i]);

      const data: TemplateData = {
        backgroundUrl: assetUrl,
        titleLines,
        speakerName,
      };

      const html = template.render(data);
      host.innerHTML = html;

      // Wait for the background image to load
      await waitForImageLoad(host);

      const canvas = await html2canvas(
        host.querySelector(".thumbnail-canvas") as HTMLElement,
        {
          width: template.width,
          height: template.height,
          scale: 1,
          useCORS: true,
          allowTaint: true,
          backgroundColor: "#000000",
        },
      );

      const blob = await new Promise<Blob>((resolve, reject) => {
        canvas.toBlob(
          (b) => (b ? resolve(b) : reject(new Error("toBlob failed"))),
          "image/png",
        );
      });

      const arrayBuffer = await blob.arrayBuffer();
      results.push({ imageData: new Uint8Array(arrayBuffer), index: i });
    }
  } finally {
    document.body.removeChild(host);
    document.head.removeChild(fontStyle);
  }

  return results;
}

export async function renderSingleThumbnail(
  template: TemplateConfig,
  baseImagePath: string,
  titleLines: string[],
  speakerName: string,
): Promise<Uint8Array> {
  const results = await renderThumbnailBatch(
    template,
    [baseImagePath],
    titleLines,
    speakerName,
  );
  return results[0].imageData;
}
