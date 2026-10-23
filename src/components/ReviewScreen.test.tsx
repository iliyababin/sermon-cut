import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { VideoDetail } from "@/lib/types";
import { renderKey } from "./review/renderKey";
import { splitTitleIntoLines } from "@/lib/utils";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  convertFileSrc: (p: string) => `asset://${p}`,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn(() => Promise.resolve(true)) }));
vi.mock("@tauri-apps/plugin-shell", () => ({ open: vi.fn() }));
const renderThumbnailBatch = vi.fn();
vi.mock("@/lib/thumbnailRenderer", () => ({
  renderThumbnailBatch: (...args: unknown[]) => renderThumbnailBatch(...args),
}));
vi.mock("./ThumbnailEditor", () => ({ ThumbnailEditor: () => null }));

import { ReviewScreen } from "./ReviewScreen";
import { ask } from "@tauri-apps/plugin-dialog";

const TITLE = "The Hope We Have";
const PREACHER = "Pastor X";
const options = [
  { id: "thumbnail_option_1.jpg", path: "/v/thumbnail_option_1.jpg", render_from: "/v/thumbnail_option_1_raw.jpg", version: "1" },
  { id: "thumbnail_option_2.jpg", path: "/v/thumbnail_option_2.jpg", render_from: "/v/thumbnail_option_2_raw.jpg", version: "1" },
];
const currentKey = (o: (typeof options)[number]) =>
  renderKey(o, { titleLines: splitTitleIntoLines(TITLE), preacher: PREACHER, singleLine: false });

function video(overrides: Partial<VideoDetail> = {}): VideoDetail {
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
    thumbnail_options: options,
    // Already rendered with the current title and speaker.
    renders: options.map((o) => ({ source: o.id, path: `/v/r_${o.id}`, render_key: currentKey(o) })),
    selected_source: options[0].id,
    selected_thumbnail: null,
    review: { title: TITLE, description: "", preacher: PREACHER },
    ...overrides,
  };
}

function calls(cmd: string) {
  return invoke.mock.calls.filter(([c]) => c === cmd);
}

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => {
    switch (cmd) {
      case "youtube_get_auth_status":
        return Promise.resolve({ is_authenticated: true, channel_name: "Youth", channel_id: "UC1" });
      case "youtube_upload_video":
        return new Promise(() => {}); // still uploading
      case "get_settings":
        return Promise.resolve({ upload_privacy: "unlisted", youtube_playlist_id: "PL1" });
      case "youtube_list_playlists":
        return Promise.resolve([
          { id: "PL1", title: "Sermons" },
          { id: "PL2", title: "Youth Nights" },
        ]);
      default:
        return Promise.resolve(null);
    }
  });
  renderThumbnailBatch.mockReset();
  renderThumbnailBatch.mockResolvedValue([{ imageData: new Uint8Array([1]), index: 0 }]);
});

afterEach(cleanup);

describe("ReviewScreen", () => {
  it("doesn't re-render thumbnails whose stored render is current", async () => {
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    await screen.findByRole("button", { name: /upload/i });
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(renderThumbnailBatch).not.toHaveBeenCalled();
    expect(calls("save_rendered_thumbnail")).toHaveLength(0);
  });

  it("sends one upload for a double-click", async () => {
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    const upload = await screen.findByRole("button", { name: /upload/i });
    expect((upload as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(upload);
    fireEvent.click(upload);
    await waitFor(() => expect(calls("youtube_upload_video")).toHaveLength(1));
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(calls("youtube_upload_video")).toHaveLength(1);
    const args = calls("youtube_upload_video")[0][1] as Record<string, unknown>;
    expect(args.thumbnailSource).toBe(options[0].id);
    expect(args.renderKey).toBe(currentKey(options[0]));
  });

  it("disables Upload while thumbnails are being updated", async () => {
    renderThumbnailBatch.mockReturnValue(new Promise(() => {})); // never finishes
    render(<ReviewScreen video={video({ renders: [] })} onBack={() => {}} />);
    const upload = await screen.findByRole("button", { name: /updating thumbnail/i });
    expect((upload as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(upload);
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(calls("youtube_upload_video")).toHaveLength(0);
  });

  it("saves edits before going back", async () => {
    const onBack = vi.fn(() => {
      // By the time the screen is left, the edit is saved.
      const saves = calls("save_review");
      expect(saves).toHaveLength(1);
      expect((saves[0][1] as Record<string, unknown>).title).toBe("New title");
    });
    render(<ReviewScreen video={video()} onBack={onBack} />);
    fireEvent.change(screen.getByDisplayValue(TITLE), { target: { value: "New title" } });
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await waitFor(() => expect(onBack).toHaveBeenCalledTimes(1));
  });

  it("Esc saves the edits, then goes back", async () => {
    const onBack = vi.fn(() => {
      const saves = calls("save_review");
      expect(saves).toHaveLength(1);
      expect((saves[0][1] as Record<string, unknown>).title).toBe("Esc title");
    });
    render(<ReviewScreen video={video()} onBack={onBack} />);
    fireEvent.change(screen.getByDisplayValue(TITLE), { target: { value: "Esc title" } });
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(onBack).toHaveBeenCalledTimes(1));
  });

  it("ignores Esc in the description and while hidden", async () => {
    const onBack = vi.fn();
    const { rerender } = render(<ReviewScreen video={video()} onBack={onBack} />);
    const description = screen.getByRole("textbox", { name: /description/i });
    fireEvent.keyDown(description, { key: "Escape" });
    rerender(<ReviewScreen video={video()} onBack={onBack} active={false} />);
    fireEvent.keyDown(window, { key: "Escape" });
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(onBack).not.toHaveBeenCalled();
  });

  it("stays when the save fails, and Back retries it", async () => {
    const base = invoke.getMockImplementation()!;
    let fail = true;
    invoke.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "save_review" && fail) {
        fail = false;
        return Promise.reject("disk full");
      }
      return base(cmd, args);
    });
    const onBack = vi.fn();
    render(<ReviewScreen video={video()} onBack={onBack} />);
    fireEvent.change(screen.getByDisplayValue(TITLE), { target: { value: "Again" } });
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await screen.findByText("disk full");
    expect(onBack).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await waitFor(() => expect(onBack).toHaveBeenCalledTimes(1));
    expect(calls("save_review")).toHaveLength(2);
    expect((calls("save_review")[1][1] as Record<string, unknown>).title).toBe("Again");
  });

  it("shows a failed regenerate inline, and disables it during an upload", async () => {
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "regenerate_thumbnails" ? Promise.reject("ffmpeg crashed") : base(cmd, args)
    );
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    const regenerate = screen.getByRole("button", { name: /regenerate/i }) as HTMLButtonElement;
    fireEvent.click(regenerate);
    expect((await screen.findByRole("alert")).textContent).toContain("ffmpeg crashed");
    fireEvent.click(await screen.findByRole("button", { name: /^upload$/i }));
    await waitFor(() => expect(regenerate.disabled).toBe(true));
  });

  it("names the title, speaker and privacy before uploading", async () => {
    vi.mocked(ask).mockClear();
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    const privacy = (await screen.findByLabelText("Privacy")) as HTMLSelectElement;
    await waitFor(() => expect(privacy.value).toBe("unlisted"));
    fireEvent.click(screen.getByRole("button", { name: /^upload$/i }));
    await waitFor(() => expect(calls("youtube_upload_video")).toHaveLength(1));
    expect(vi.mocked(ask).mock.calls[0][0]).toBe(`Upload "${TITLE}" by ${PREACHER} as Unlisted?`);
  });

  it("starts a video with no speaker from the most recent one", async () => {
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "get_settings"
        ? Promise.resolve({ upload_privacy: "public", youtube_playlist_id: null, recent_preachers: ["Pastor Y", "Pastor Z"] })
        : base(cmd, args)
    );
    render(<ReviewScreen video={video({ review: { title: TITLE, description: "", preacher: null } })} onBack={() => {}} />);
    const input = screen.getByLabelText("Preacher") as HTMLInputElement;
    await waitFor(() => expect(input.value).toBe("Pastor Y"));
    expect(document.querySelectorAll("#recent-preachers option")).toHaveLength(2);
  });

  it("starts from the last privacy and the default playlist", async () => {
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    const privacy = (await screen.findByLabelText("Privacy")) as HTMLSelectElement;
    const playlist = screen.getByLabelText("Playlist") as HTMLSelectElement;
    await waitFor(() => expect(privacy.value).toBe("unlisted"));
    await waitFor(() => expect(playlist.value).toBe("PL1"));
    await screen.findByRole("option", { name: "Youth Nights" });
    fireEvent.click(screen.getByRole("button", { name: /upload/i }));
    await waitFor(() => expect(calls("youtube_upload_video")).toHaveLength(1));
    const args = calls("youtube_upload_video")[0][1] as Record<string, unknown>;
    expect(args.privacy).toBe("unlisted");
    expect(args.playlistId).toBe("PL1");
  });

  it("uploads with the privacy and playlist chosen next to Upload", async () => {
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    const privacy = (await screen.findByLabelText("Privacy")) as HTMLSelectElement;
    await screen.findByRole("option", { name: "Youth Nights" });
    await waitFor(() => expect(privacy.value).toBe("unlisted"));
    fireEvent.change(privacy, { target: { value: "private" } });
    fireEvent.change(screen.getByLabelText("Playlist"), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: /upload/i }));
    await waitFor(() => expect(calls("youtube_upload_video")).toHaveLength(1));
    const args = calls("youtube_upload_video")[0][1] as Record<string, unknown>;
    expect(args.privacy).toBe("private");
    // "No playlist" is sent as such, so the default isn't used instead.
    expect(args.playlistId).toBe("");
  });

  it("cancels a running upload, which can then be continued", async () => {
    let reject: (e: unknown) => void = () => {};
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "youtube_upload_video") return new Promise((_, r) => (reject = r));
      if (cmd === "cancel_upload") {
        reject("Upload cancelled.");
        return Promise.resolve(null);
      }
      return base(cmd, args);
    });
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: /upload/i }));
    const cancel = await screen.findByRole("button", { name: "Cancel upload" });
    fireEvent.click(cancel);
    await screen.findByText(/Upload cancelled\. Upload again to continue/);
    expect(calls("cancel_upload")).toEqual([["cancel_upload", { videoId: "vid1" }]]);
    expect(screen.queryByRole("button", { name: "Cancel upload" })).toBeNull();
    // No error banner: a cancel isn't a failure.
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    const upload = screen.getByRole("button", { name: /^upload$/i }) as HTMLButtonElement;
    expect(upload.disabled).toBe(false);
  });

  it("shows the upload's warnings and sets the thumbnail again", async () => {
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "retry_thumbnail"
        ? Promise.resolve([{ kind: "playlist", message: "The video wasn't added to the playlist." }])
        : base(cmd, args)
    );
    const uploads = [
      {
        url: "https://youtube.com/watch?v=abc",
        uploaded_at: "",
        file_hash: "h",
        video_id: "abc",
        warnings: [
          { kind: "thumbnail" as const, message: "The thumbnail wasn't set. Try later." },
          { kind: "playlist" as const, message: "The video wasn't added to the playlist." },
        ],
      },
    ];
    render(<ReviewScreen video={video({ status: "done", uploads })} onBack={() => {}} />);
    await screen.findByText("The thumbnail wasn't set. Try later.");
    fireEvent.click(screen.getByRole("button", { name: /set thumbnail again/i }));
    await waitFor(() => expect(screen.queryByText("The thumbnail wasn't set. Try later.")).toBeNull());
    expect(calls("retry_thumbnail")).toEqual([["retry_thumbnail", { videoId: "vid1" }]]);
    expect(screen.getByText("The video wasn't added to the playlist.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /set thumbnail again/i })).toBeNull();
  });
});
