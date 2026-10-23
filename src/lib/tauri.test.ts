import { describe, expect, it, vi } from "vitest";

const invoke = vi.fn().mockResolvedValue("/lib/videos/v/thumbnail_rendered_x.jpg");
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

import { saveRenderedThumbnail } from "./tauri";

describe("saveRenderedThumbnail", () => {
  it("sends the JPEG as the raw body and the ids as ASCII headers", async () => {
    const bytes = new Uint8Array([0xff, 0xd8, 0xff]);
    await saveRenderedThumbnail("Café 'x'", "custom_thumbnail_1.jpg", "abc123", bytes);
    const [cmd, body, options] = invoke.mock.calls[0];
    expect(cmd).toBe("save_rendered_thumbnail");
    expect(body).toBe(bytes);
    expect(options.headers).toEqual({
      "x-video-id": "Caf%C3%A9%20'x'",
      "x-source": "custom_thumbnail_1.jpg",
      "x-render-key": "abc123",
    });
    for (const v of Object.values(options.headers as Record<string, string>)) {
      expect(/^[\x21-\x7e]+$/.test(v)).toBe(true);
    }
  });
});
