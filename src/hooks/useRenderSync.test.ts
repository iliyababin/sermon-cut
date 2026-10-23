import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { ThumbnailOption, ThumbnailRender } from "@/lib/types";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, ...args: unknown[]) => invoke(cmd, ...args),
  convertFileSrc: (p: string) => `asset://${p}`,
}));

import { useRenderSync } from "./useRenderSync";

const options: ThumbnailOption[] = [
  { id: "a.jpg", path: "/v/a.jpg", render_from: "/v/a.jpg", version: "1" },
  { id: "b.jpg", path: "/v/b.jpg", render_from: "/v/b.jpg", version: "1" },
];

/** A promise the test settles. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

let saves: { videoId: string; source: string; done: ReturnType<typeof deferred<string>> }[];
let stored: Record<string, ThumbnailRender[]>;

beforeEach(() => {
  saves = [];
  stored = {};
  invoke.mockReset();
  invoke.mockImplementation((cmd: string, _body: unknown, opts?: { headers: Record<string, string> }) => {
    if (cmd === "save_rendered_thumbnail") {
      const h = opts!.headers;
      const done = deferred<string>();
      saves.push({ videoId: decodeURIComponent(h["x-video-id"]), source: decodeURIComponent(h["x-source"]), done });
      return done.promise;
    }
    if (cmd === "get_video") {
      const { videoId } = _body as { videoId: string };
      return Promise.resolve({ id: videoId, renders: stored[videoId] ?? [] });
    }
    return Promise.resolve(null);
  });
});

afterEach(cleanup);

interface Props {
  videoId: string;
  keys: Record<string, string>;
  enabled: boolean;
}

function setup(initial: Props) {
  const render = vi.fn(() => Promise.resolve(new Uint8Array([1])));
  const hook = renderHook((p: Props) => useRenderSync({ ...p, options, initial: [], render }), { initialProps: initial });
  return { hook, render };
}

const keysA = { "a.jpg": "kA", "b.jpg": "kA" };

describe("useRenderSync", () => {
  it("keeps a render the backend saved after the run was cancelled, and clears the progress", async () => {
    const { hook } = setup({ videoId: "v1", keys: keysA, enabled: true });
    await waitFor(() => expect(saves).toHaveLength(1));
    expect(hook.result.current.progress).toMatch(/1\/2/);

    // The speaker is cleared mid-save: the run is cancelled.
    hook.rerender({ videoId: "v1", keys: keysA, enabled: false });
    stored.v1 = [{ source: "a.jpg", path: "/v/r_a_new.jpg", render_key: "kA" }];
    await act(async () => saves[0].done.resolve("/v/r_a_new.jpg"));

    await waitFor(() => expect(hook.result.current.renders["a.jpg"]?.path).toBe("/v/r_a_new.jpg"));
    expect(hook.result.current.progress).toBe("");
    // Cancelling stopped the next render.
    expect(saves).toHaveLength(1);
    // And the stored renders were read back.
    expect(invoke.mock.calls.some(([c]) => c === "get_video")).toBe(true);
  });

  it("clears the progress when a render fails", async () => {
    const { hook } = setup({ videoId: "v1", keys: keysA, enabled: true });
    await waitFor(() => expect(saves).toHaveLength(1));
    await act(async () => saves[0].done.reject("disk full"));
    await waitFor(() => expect(hook.result.current.error).toBe("disk full"));
    expect(hook.result.current.progress).toBe("");
  });

  it("never writes one video's render into another video's screen", async () => {
    const { hook } = setup({ videoId: "v1", keys: keysA, enabled: true });
    await waitFor(() => expect(saves).toHaveLength(1));

    hook.rerender({ videoId: "v2", keys: { "a.jpg": "kB", "b.jpg": "kB" }, enabled: false });
    stored.v1 = [{ source: "a.jpg", path: "/v1/r_a.jpg", render_key: "kA" }];
    await act(async () => saves[0].done.resolve("/v1/r_a.jpg"));
    await act(async () => {});

    expect(hook.result.current.renders["a.jpg"]).toBeUndefined();
    expect(invoke.mock.calls.filter(([c]) => c === "get_video")).toHaveLength(0);
    expect(saves.every((s) => s.videoId === "v1")).toBe(true);
  });

  it("does nothing once the screen is gone", async () => {
    const { hook } = setup({ videoId: "v1", keys: keysA, enabled: true });
    await waitFor(() => expect(saves).toHaveLength(1));
    hook.unmount();
    await act(async () => saves[0].done.resolve("/v1/r_a.jpg"));
    await act(async () => {});
    expect(saves).toHaveLength(1);
    expect(invoke.mock.calls.filter(([c]) => c === "get_video")).toHaveLength(0);
  });
});
