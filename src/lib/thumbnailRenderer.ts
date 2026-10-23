import html2canvas from "html2canvas";
import { convertFileSrc } from "@tauri-apps/api/core";
import type { TemplateConfig, TemplateData } from "@/templates/types";

/** The template fonts, declared once for the whole document (html2canvas
 * clones the document, so they must live in a stylesheet there). */
const FONT_FACES = `
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
}`;

function installFonts(): void {
  if (document.getElementById("thumbnail-fonts")) return;
  const style = document.createElement("style");
  style.id = "thumbnail-fonts";
  style.textContent = FONT_FACES;
  document.head.appendChild(style);
}

async function ensureFontsLoaded(template: TemplateConfig): Promise<void> {
  installFonts();
  await Promise.all(template.fonts.map((f) => document.fonts.load(f)));
  await document.fonts.ready;
}

function loadImage(url: string): Promise<void> {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve();
    img.onerror = () => resolve(); // Don't block on error
    img.src = url;
  });
}

export interface RenderResult {
  imageData: Uint8Array;
  index: number;
}

export interface RenderOptions {
  onProgress?: (current: number, total: number) => void;
  /** File path to a URL the webview can load; Tauri's asset protocol by default. */
  resolveUrl?: (path: string) => string;
}

/** Draw the template over each image and return the results as PNG bytes. */
export async function renderThumbnailBatch(
  template: TemplateConfig,
  baseImagePaths: string[],
  titleLines: string[],
  speakerName: string,
  options: RenderOptions = {},
): Promise<RenderResult[]> {
  const resolveUrl = options.resolveUrl ?? convertFileSrc;
  // Hidden but laid out, so text can be measured.
  const host = document.createElement("div");
  host.style.position = "fixed";
  host.style.left = "-9999px";
  host.style.top = "0";
  host.style.zIndex = "-1";
  document.body.appendChild(host);

  await ensureFontsLoaded(template);

  const results: RenderResult[] = [];

  try {
    for (let i = 0; i < baseImagePaths.length; i++) {
      options.onProgress?.(i + 1, baseImagePaths.length);

      const data: TemplateData = {
        backgroundUrl: resolveUrl(baseImagePaths[i]),
        titleLines,
        speakerName,
      };
      const root = template.render(host, data);
      await loadImage(data.backgroundUrl);

      const canvas = await html2canvas(root, {
        width: template.width,
        height: template.height,
        scale: 1,
        useCORS: true,
        allowTaint: true,
        backgroundColor: "#000000",
        logging: false,
      });

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
  }

  return results;
}

export async function renderSingleThumbnail(
  template: TemplateConfig,
  baseImagePath: string,
  titleLines: string[],
  speakerName: string,
  options: RenderOptions = {},
): Promise<Uint8Array> {
  const results = await renderThumbnailBatch(template, [baseImagePath], titleLines, speakerName, options);
  return results[0].imageData;
}
