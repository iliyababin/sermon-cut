import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { LibraryVideo, VideoDetail } from "@/lib/types";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, ...rest: unknown[]) => invoke(cmd, ...rest),
  convertFileSrc: (p: string) => `asset://${p}`,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}), emit: vi.fn() }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: () => Promise.resolve("0.5.0") }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn(() => Promise.resolve(true)) }));
vi.mock("@tauri-apps/plugin-shell", () => ({ open: vi.fn() }));
vi.mock("@/lib/thumbnailRenderer", () => ({ renderThumbnailBatch: vi.fn(() => Promise.resolve([])) }));
vi.mock("./components/ThumbnailEditor", () => ({ ThumbnailEditor: () => null }));
vi.mock("./hooks/useUpdater", async () => {
  const { createContext } = await import("react");
  return {
    UpdaterContext: createContext(null),
    useUpdaterState: () => ({ available: false, checkForUpdates: () => {} }),
  };
});
// The library is a single card here.
vi.mock("./components/VideoLibrary", () => ({
  VideoLibrary: ({ onVideoSelect }: { onVideoSelect: (v: LibraryVideo) => void }) => (
    <button onClick={() => onVideoSelect({ id: "vid1", status: "ready_for_review" } as LibraryVideo)}>Open vid1</button>
  ),
}));
vi.mock("./components/DownloadForm", () => ({ DownloadForm: () => null }));
vi.mock("./components/Settings", () => ({ Settings: () => null }));
vi.mock("./components/ThumbnailGenerator", () => ({ ThumbnailGenerator: () => null }));

import App from "./App";

function detail(title: string): VideoDetail {
  return {
    id: "vid1",
    file_name: "a.mp4",
    file_path: "/lib/a.mp4",
    title: "a",
    status: "ready_for_review",
    error: null,
    duration: 600,
    size: 1,
    channel: null,
    source_url: null,
    hidden: false,
    uploads: [],
    added_at: "",
    thumbnail: null,
    processing_stage: null,
    processing_progress: null,
    processing_message: null,
    upload_progress: null,
    busy: false,
    queue_position: null,
    queue_length: null,
    transcription: null,
    trimmed_path: "/v/trimmed.mp4",
    trim_start: 0,
    trim_end: 600,
    thumbnail_options: [],
    renders: [],
    selected_source: null,
    selected_thumbnail: null,
    review: { title, description: "", preacher: "Pastor X" },
    duplicate_upload: null,
  };
}

// What the backend has saved.
let savedTitle: string;
let failSaves: boolean;

beforeEach(() => {
  savedTitle = "Old title";
  failSaves = false;
  invoke.mockReset();
  invoke.mockImplementation((cmd: string, args?: { title?: string }) => {
    switch (cmd) {
      case "get_video":
        return Promise.resolve(detail(savedTitle));
      case "save_review":
        if (failSaves) return Promise.reject("The disk is full.");
        // Slower than a click: the save is still under way when the user
        // has already moved on.
        return new Promise((r) =>
          setTimeout(() => {
            savedTitle = args?.title ?? savedTitle;
            r(null);
          }, 50)
        );
      case "youtube_get_auth_status":
        return Promise.resolve({ is_authenticated: false, channel_name: null, channel_id: null });
      case "get_settings":
        return Promise.resolve({ upload_privacy: "public", youtube_playlist_id: null, recent_preachers: [] });
      default:
        return Promise.resolve(null);
    }
  });
});

afterEach(cleanup);

async function openReview() {
  fireEvent.click(screen.getByText("Open vid1"));
  return (await screen.findByDisplayValue(/title/)) as HTMLInputElement;
}

describe("Leaving the review", () => {
  it("saves an edit before the sidebar moves on, so reopening shows it", async () => {
    render(<App />);
    const title = await openReview();
    fireEvent.change(title, { target: { value: "New title" } });
    // Well before the autosave would run.
    fireEvent.click(screen.getByText("Video Library"));
    await waitFor(() => expect(savedTitle).toBe("New title"));
    await openReview();
    expect(screen.getByDisplayValue("New title")).toBeTruthy();
  });

  it("stays when the edit can't be saved, and can leave anyway", async () => {
    render(<App />);
    const title = await openReview();
    failSaves = true;
    fireEvent.change(title, { target: { value: "New title" } });
    fireEvent.click(screen.getByText("Settings"));
    const leave = await screen.findByText("Leave anyway");
    expect(leave.closest("[role=alert]")?.textContent).toContain("The disk is full.");
    // Still on the review.
    expect(screen.getByDisplayValue("New title")).toBeTruthy();
    expect(screen.getByDisplayValue("New title").closest(".hidden")).toBeNull();
    fireEvent.click(screen.getByText("Leave anyway"));
    await waitFor(() => expect(screen.getByDisplayValue("New title").closest(".hidden")).not.toBeNull());
  });
});
