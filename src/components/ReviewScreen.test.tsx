import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { VideoDetail } from "@/lib/types";
import { renderKey } from "./review/renderKey";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, ...rest: unknown[]) => invoke(cmd, ...rest),
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
  renderKey(o, { title: TITLE, preacher: PREACHER, singleLine: false });

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
    duplicate_upload: null,
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
  it("asks for the start and end when the AI couldn't find the sermon", async () => {
    const transcription = {
      segments: [],
      full_text: "",
      sermon_start: null,
      sermon_end: null,
      suggested_title: null,
      suggested_description: null,
      suggested_chapters: null,
    };
    render(<ReviewScreen video={video({ transcription: { ...transcription, analysis_error: "overloaded" } })} onBack={() => {}} />);
    expect(await screen.findByText(/Couldn't find the sermon automatically — set the start and end below/)).toBeTruthy();
    cleanup();
    render(<ReviewScreen video={video({ transcription })} onBack={() => {}} />);
    await screen.findByRole("button", { name: /upload/i });
    expect(screen.queryByText(/Couldn't find the sermon automatically/)).toBeNull();
  });

  it("times captions from where a new trim really starts", async () => {
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "set_trim" ? Promise.resolve({ start: 4, end: 500 }) : base(cmd, args)
    );
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    fireEvent.click(await screen.findByLabelText("Start +5 s"));
    fireEvent.click(screen.getByLabelText("End −5 s"));
    fireEvent.click(screen.getByText("Trim video"));
    await screen.findByText(/Starts at 0:04.0/);
    fireEvent.click(screen.getByRole("button", { name: /upload/i }));
    await waitFor(() => expect(calls("youtube_upload_video")).toHaveLength(1));
    const args = calls("youtube_upload_video")[0][1] as Record<string, unknown>;
    expect([args.startTime, args.endTime]).toEqual([4, 500]);
  });

  it("doesn't re-render thumbnails whose stored render is current", async () => {
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    await screen.findByRole("button", { name: /upload/i });
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(renderThumbnailBatch).not.toHaveBeenCalled();
    expect(calls("save_rendered_thumbnail")).toHaveLength(0);
  });

  it("saves the title and speaker before a render of them, which the backend checks", async () => {
    // Fresh from processing: the suggested title was never saved.
    render(<ReviewScreen video={video({ renders: [], review: { title: null, description: null, preacher: PREACHER } })} onBack={() => {}} />);
    await waitFor(() => expect(calls("save_rendered_thumbnail").length).toBeGreaterThan(0));
    const order = invoke.mock.calls.map(([c]) => c).filter((c) => c === "save_review" || c === "save_rendered_thumbnail");
    expect(order[0]).toBe("save_review");
    const saved = calls("save_review")[0][1] as Record<string, unknown>;
    expect(saved).toMatchObject({ title: "a", preacher: PREACHER, singleLine: false });
    const headers = (calls("save_rendered_thumbnail")[0] as unknown[])[2] as { headers: Record<string, string> };
    expect(headers.headers["x-render-key"]).toBe(renderKey(options[0], { title: "a", preacher: PREACHER, singleLine: false }));
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

  it("Esc in a field leaves the field, and is left to an input method", async () => {
    const onBack = vi.fn();
    render(<ReviewScreen video={video()} onBack={onBack} />);
    const title = screen.getByDisplayValue(TITLE);
    const preacher = screen.getByLabelText("Preacher");
    for (const field of [title, preacher]) {
      field.focus();
      expect(document.activeElement).toBe(field);
      fireEvent.keyDown(field, { key: "Escape" });
      expect(document.activeElement).not.toBe(field);
    }
    // Composing (e.g. Japanese input): Esc cancels the composition only.
    title.focus();
    fireEvent.keyDown(title, { key: "Escape", isComposing: true });
    fireEvent.keyDown(window, { key: "Escape", keyCode: 229 });
    expect(document.activeElement).toBe(title);
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(onBack).not.toHaveBeenCalled();
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

  it("warns when the same file was uploaded under another name", async () => {
    vi.mocked(ask).mockClear();
    render(<ReviewScreen video={video({ duplicate_upload: "https://youtu.be/dup" })} onBack={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: /^upload$/i }));
    await waitFor(() => expect(calls("youtube_upload_video")).toHaveLength(1));
    expect(vi.mocked(ask).mock.calls[0][0]).toContain("https://youtu.be/dup");
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

  it("offers to update an earlier finished upload's title, or to upload a new copy after confirming", async () => {
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "youtube_upload_video"
        ? Promise.resolve({
            url: "https://youtube.com/watch?v=old1",
            video_id: "old1",
            warnings: [],
            earlier: { new_cut: false, metadata_changed: true },
            before_cancel: false,
          })
        : base(cmd, args)
    );
    vi.mocked(ask).mockClear();
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: /^upload$/i }));
    await screen.findByText("Already on YouTube");
    expect(screen.getByRole("button", { name: /open on youtube/i })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Update title & description on that video" }));
    await screen.findByText("The title and description on YouTube are updated.");
    expect(calls("youtube_update_metadata")).toEqual([
      ["youtube_update_metadata", { videoId: "vid1", youtubeId: "old1", title: TITLE, description: expect.any(String) }],
    ]);
    // The notice is closed; the link stays.
    expect(screen.queryByRole("button", { name: "Upload a new copy" })).toBeNull();
    expect(screen.getByRole("button", { name: /open on youtube/i })).toBeTruthy();
    expect(calls("youtube_upload_video")).toHaveLength(1);
  });

  it("after a re-trim, says the old cut is on YouTube and uploads the new cut only when confirmed", async () => {
    const base = invoke.getMockImplementation()!;
    let first = true;
    invoke.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "youtube_upload_video" && first) {
        first = false;
        return Promise.resolve({
          url: "https://youtube.com/watch?v=old1",
          video_id: "old1",
          warnings: [],
          earlier: { new_cut: true, metadata_changed: false },
        });
      }
      return base(cmd, args);
    });
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: /^upload$/i }));
    await screen.findByText("The previous cut is already on YouTube");
    expect(screen.queryByRole("button", { name: /update title/i })).toBeNull();

    vi.mocked(ask).mockClear();
    vi.mocked(ask).mockResolvedValueOnce(false);
    fireEvent.click(screen.getByRole("button", { name: "Upload the new cut as a new video" }));
    await waitFor(() => expect(vi.mocked(ask)).toHaveBeenCalledTimes(1));
    expect(vi.mocked(ask).mock.calls[0][0]).toContain("new, separate YouTube video");
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(calls("youtube_upload_video")).toHaveLength(1);

    fireEvent.click(screen.getByRole("button", { name: "Upload the new cut as a new video" }));
    await waitFor(() => expect(calls("youtube_upload_video")).toHaveLength(2));
  });

  it("says when a cancel may have come too late", async () => {
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "youtube_upload_video"
        ? Promise.reject("Upload cancelled, but it may have finished just before. Uploading again checks with YouTube first.")
        : base(cmd, args)
    );
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: /^upload$/i }));
    await screen.findByText(/may have finished just before/);
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  });

  it("won't upload without a title, and says why", async () => {
    render(<ReviewScreen video={video({ review: { title: "   ", description: "", preacher: PREACHER } })} onBack={() => {}} />);
    const upload = (await screen.findByRole("button", { name: /^upload$/i })) as HTMLButtonElement;
    expect(upload.disabled).toBe(true);
    expect(screen.getByRole("note").textContent).toBe("Add a title to upload.");
    fireEvent.keyDown(window, { key: "Enter", metaKey: true });
    await act(() => new Promise((r) => setTimeout(r, 20)));
    expect(calls("youtube_upload_video")).toHaveLength(0);
  });

  it("disables Reprocess during an upload", async () => {
    render(<ReviewScreen video={video()} onBack={() => {}} />);
    const reprocess = screen.getByRole("button", { name: /reprocess/i }) as HTMLButtonElement;
    expect(reprocess.disabled).toBe(false);
    fireEvent.click(await screen.findByRole("button", { name: /^upload$/i }));
    await waitFor(() => expect(calls("youtube_upload_video")).toHaveLength(1));
    expect(reprocess.disabled).toBe(true);
    expect(reprocess.title).toBe("Wait for the upload to finish");
  });
});
