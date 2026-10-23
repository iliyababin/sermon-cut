import { describe, expect, it, vi } from "vitest";

vi.mock("html2canvas", () => ({ default: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ convertFileSrc: (p: string) => `asset://${p}` }));

import { renderThumbnailBatch } from "./thumbnailRenderer";
import type { TemplateConfig } from "@/templates/types";

describe("renderThumbnailBatch", () => {
  it("removes its hidden host when the fonts can't load", async () => {
    const fonts = Object.getOwnPropertyDescriptor(document, "fonts");
    Object.defineProperty(document, "fonts", {
      configurable: true,
      value: { load: () => Promise.reject(new Error("no fonts")), ready: Promise.resolve() },
    });
    try {
      const template = { fonts: ["700 10px Baskervville"], render: vi.fn() } as unknown as TemplateConfig;
      const before = document.body.childElementCount;
      await expect(renderThumbnailBatch(template, ["/a.jpg"], ["Hi"], "X")).rejects.toThrow("no fonts");
      expect(document.body.childElementCount).toBe(before);
      expect(template.render).not.toHaveBeenCalled();
    } finally {
      if (fonts) Object.defineProperty(document, "fonts", fonts);
      else delete (document as { fonts?: unknown }).fonts;
    }
  });
});
