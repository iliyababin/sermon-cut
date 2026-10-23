import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, renderHook, screen } from "@testing-library/react";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  convertFileSrc: (p: string) => `asset://${p}`,
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./LocalImage", () => ({ LocalImage: ({ alt }: { alt: string }) => <img alt={alt} /> }));

import { ThumbnailEditor } from "./ThumbnailEditor";
import { useCanvasEditor } from "@/hooks/useCanvasEditor";

/** An Image whose load the test finishes, in any order. */
class FakeImage {
  static all: FakeImage[] = [];
  width = 0;
  height = 0;
  src = "";
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor() {
    FakeImage.all.push(this);
  }
  /** Finish loading as a `width`×`height` image. */
  finish(width: number, height: number) {
    this.width = width;
    this.height = height;
    this.onload?.();
  }
  static bySrc(src: string): FakeImage {
    const img = FakeImage.all.find((i) => i.src === src);
    if (!img) throw new Error(`no image for ${src}`);
    return img;
  }
}

const realImage = globalThis.Image;
beforeEach(() => {
  FakeImage.all = [];
  (globalThis as { Image: unknown }).Image = FakeImage;
  (globalThis as { ResizeObserver: unknown }).ResizeObserver = class {
    observe() {}
    disconnect() {}
  };
  // jsdom has no canvas; nothing is drawn.
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(null);
  invoke.mockReset();
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  (globalThis as { Image: unknown }).Image = realImage;
});

describe("useCanvasEditor", () => {
  it("only the latest load reaches the canvas, whatever order they finish in", async () => {
    const { result } = renderHook(() => useCanvasEditor());
    let first!: ReturnType<typeof result.current.loadImage>;
    let second!: ReturnType<typeof result.current.loadImage>;
    act(() => {
      first = result.current.loadImage("data:a", "/a.jpg");
      second = result.current.loadImage("data:b", "/b.jpg");
    });
    act(() => FakeImage.bySrc("data:b").finish(1600, 900));
    act(() => FakeImage.bySrc("data:a").finish(4000, 3000));
    expect(await second.done).toBe("loaded");
    expect(await first.done).toBe("superseded");
    expect(result.current.loadedKey).toBe("/b.jpg");
    // The crop is B's whole image, not A's.
    expect(result.current.getCropRect()).toEqual({ x: 0, y: 0, width: 1600, height: 900 });

    // A load in progress: nothing is named as shown until it finishes.
    act(() => void result.current.loadImage("data:c", "/c.jpg"));
    expect(result.current.loadedKey).toBeNull();
  });

  it("a cancelled load never reaches the canvas", async () => {
    const { result } = renderHook(() => useCanvasEditor());
    let load!: ReturnType<typeof result.current.loadImage>;
    act(() => {
      load = result.current.loadImage("data:a", "/a.jpg");
    });
    load.cancel();
    act(() => FakeImage.bySrc("data:a").finish(1600, 900));
    expect(await load.done).toBe("superseded");
    expect(result.current.loadedKey).toBeNull();
    expect(result.current.getCropRect()).toBeNull();
  });
});

describe("ThumbnailEditor", () => {
  const options = [
    { id: "thumbnail_option_1.jpg", path: "/v/thumbnail_option_1.jpg", render_from: "/v/thumbnail_option_1_raw.jpg", version: "1" },
    { id: "thumbnail_option_2.jpg", path: "/v/thumbnail_option_2.jpg", render_from: "/v/thumbnail_option_2_raw.jpg", version: "1" },
  ];

  it("applies only once the selected image is the one shown, and crops that one", async () => {
    invoke.mockImplementation((cmd: string, args?: { path?: string }) => {
      if (cmd === "read_image_base64") return Promise.resolve(`data:${args!.path}`);
      if (cmd === "process_custom_thumbnail") return Promise.resolve("/v/custom_thumbnail_1.jpg");
      return Promise.resolve(null);
    });
    const onSave = vi.fn();
    render(
      <ThumbnailEditor videoId="v" thumbnailOptions={options} selectedSource={options[0].id} onSave={onSave} onClose={() => {}} />
    );
    const apply = () => screen.getByRole("button", { name: /apply/i }) as HTMLButtonElement;
    await act(async () => {});
    expect(apply().disabled).toBe(true);

    // Switch to the second option before the first has loaded; the first
    // then finishes late.
    fireEvent.click(screen.getByRole("button", { name: "AI Option 2" }));
    await act(async () => {});
    act(() => FakeImage.bySrc("data:/v/thumbnail_option_1_raw.jpg").finish(4000, 3000));
    expect(apply().disabled).toBe(true);

    act(() => FakeImage.bySrc("data:/v/thumbnail_option_2_raw.jpg").finish(1280, 720));
    expect(apply().disabled).toBe(false);
    await act(async () => fireEvent.click(apply()));
    const [, args] = invoke.mock.calls.find(([c]) => c === "process_custom_thumbnail")!;
    expect(args.sourceImagePath).toBe("/v/thumbnail_option_2_raw.jpg");
    expect(args.cropRect).toEqual({ x: 0, y: 0, width: 1280, height: 720 });
    expect(onSave).toHaveBeenCalledWith("/v/custom_thumbnail_1.jpg");
  });
});
