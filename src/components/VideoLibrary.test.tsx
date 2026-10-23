import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { LibraryVideo, LibraryView } from "@/lib/types";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  convertFileSrc: (p: string) => `asset://${p}`,
}));
const listeners = new Map<string, (e: unknown) => void>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: unknown) => void) => {
    listeners.set(name, cb);
    return Promise.resolve(() => {});
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn() }));
vi.mock("@tauri-apps/plugin-shell", () => ({ open: vi.fn() }));

import { VideoLibrary } from "./VideoLibrary";

function video(id: string, over: Partial<LibraryVideo> = {}): LibraryVideo {
  return {
    id,
    file_name: `${id}.mp4`,
    file_path: `/lib/${id}.mp4`,
    title: id,
    status: "new",
    error: null,
    duration: 60,
    size: 1,
    channel: null,
    source_url: null,
    hidden: false,
    uploads: [],
    added_at: "2026-01-01T00:00:00Z",
    thumbnail: null,
    processing_stage: null,
    processing_progress: null,
    processing_message: null,
    upload_progress: null,
    busy: false,
    queue_position: null,
    queue_length: null,
    ...over,
  };
}

let library: LibraryView;

beforeEach(() => {
  library = {
    folder: "/lib",
    available: true,
    writable: true,
    message: null,
    locked_by: null,
    tool_problem: null,
    downloads: [],
    notices: [],
    max_transcribe_secs: 7200,
    videos: [
      video("queued", { status: "queued", queue_position: 2, queue_length: 3, added_at: "2026-01-05T00:00:00Z" }),
      video("newer", { status: "ready_for_review", added_at: "2026-01-04T00:00:00Z" }),
      video("older", { status: "ready_for_review", added_at: "2026-01-02T00:00:00Z" }),
      video("hidden", { status: "ready_for_review", hidden: true, added_at: "2026-01-01T00:00:00Z" }),
    ],
  };
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) =>
    cmd === "get_library" || cmd === "rescan_library" ? Promise.resolve(library) : Promise.resolve(null)
  );
});

afterEach(cleanup);

describe("VideoLibrary", () => {
  it("offers to show the leftovers of a save that couldn't be undone", async () => {
    const stuck =
      'An earlier save of this video didn\'t finish and couldn\'t be undone, so nothing was changed. Click Show leftovers, move the ".commit-backup" folder out of the video\'s folder, then try again.';
    library.videos = [video("stuck", { status: "ready_for_review", error: `Re-processing failed: ${stuck}` }), video("fine")];
    render(<VideoLibrary onVideoSelect={() => {}} />);
    const buttons = await screen.findAllByRole("button", { name: "Show leftovers" });
    expect(buttons).toHaveLength(1);
    fireEvent.click(buttons[0]);
    expect(invoke).toHaveBeenCalledWith("reveal_commit_backup", { videoId: "stuck" });
  });

  it("shows only the newest answer when an older one arrives late", async () => {
    let answerScan: (v: LibraryView) => void = () => {};
    const stale = { ...library, videos: [video("gone", { title: "Gone" })] };
    const fresh = { ...library, videos: [video("current", { title: "Current" })] };
    invoke.mockImplementation((cmd: string) =>
      cmd === "rescan_library"
        ? new Promise((r) => (answerScan = r))
        : cmd === "get_library"
          ? Promise.resolve(fresh)
          : Promise.resolve(null)
    );
    render(<VideoLibrary onVideoSelect={() => {}} />);
    // The scan is slow; a refresh asked after it answers first.
    await act(async () => listeners.get("library-changed")?.({}));
    await screen.findByText("Current");
    await act(async () => answerScan(stale));
    expect(screen.getByText("Current")).toBeTruthy();
    expect(screen.queryByText("Gone")).toBeNull();
  });

  it("shows why a download couldn't be cancelled", async () => {
    library.downloads = [{ job_id: "dl1", youtube_id: "yt1", title: "Sunday" }];
    invoke.mockImplementation((cmd: string) =>
      cmd === "cancel_download"
        ? Promise.reject("The download already finished.")
        : cmd === "get_library" || cmd === "rescan_library"
          ? Promise.resolve(library)
          : Promise.resolve(null)
    );
    render(<VideoLibrary onVideoSelect={() => {}} />);
    fireEvent.click(await screen.findByTitle("Cancel download"));
    await screen.findByText("The download already finished.");
  });

  it("shows a queued video's place in line", async () => {
    render(<VideoLibrary onVideoSelect={() => {}} />);
    await screen.findByText("Queued (2 of 3)");
  });

  it("Review next opens the oldest video waiting for review", async () => {
    const onVideoSelect = vi.fn();
    render(<VideoLibrary onVideoSelect={onVideoSelect} />);
    fireEvent.click(await screen.findByRole("button", { name: /review next/i }));
    expect(onVideoSelect).toHaveBeenCalledWith(expect.objectContaining({ id: "older" }));
  });

  it("a card is one Review button with the actions menu beside it, not inside", async () => {
    const onVideoSelect = vi.fn();
    render(<VideoLibrary onVideoSelect={onVideoSelect} />);
    const open = await screen.findByRole("button", { name: "Review older" });
    expect(open.querySelector("button")).toBeNull();
    const more = screen.getByRole("button", { name: "More actions for older" });
    expect(open.contains(more)).toBe(false);
    fireEvent.click(open);
    expect(onVideoSelect).toHaveBeenCalledWith(expect.objectContaining({ id: "older" }));

    // The menu opens without opening the review, and closes on Esc or a
    // click elsewhere.
    onVideoSelect.mockClear();
    fireEvent.click(more);
    expect(screen.getByRole("menu")).toBeTruthy();
    expect(more.getAttribute("aria-expanded")).toBe("true");
    expect(onVideoSelect).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    fireEvent.click(more);
    expect(screen.getByRole("menu")).toBeTruthy();
    fireEvent.click(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
  });
});
