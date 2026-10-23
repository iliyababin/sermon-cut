import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

import { DownloadForm } from "./DownloadForm";

const URL = "https://youtube.com/watch?v=abc";
let finishDownload: (id: string) => void = () => {};

function calls(cmd: string) {
  return invoke.mock.calls.filter(([c]) => c === cmd).map(([, a]) => a as Record<string, unknown>);
}

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => {
    switch (cmd) {
      case "get_settings":
        return Promise.resolve({ download_quality: "720p" });
      case "get_video_info":
        return Promise.resolve({ id: "abc", title: "Sunday", thumbnail: null, channel: null, duration: 60 });
      case "download_video":
        return new Promise((r) => (finishDownload = r));
      default:
        return Promise.resolve(null);
    }
  });
});

afterEach(cleanup);

describe("DownloadForm", () => {
  it("fetches on paste, downloads on Enter with the Settings quality", async () => {
    const onGoToLibrary = vi.fn();
    const { rerender } = render(<DownloadForm onGoToLibrary={onGoToLibrary} isActive />);
    await waitFor(() => expect((screen.getByLabelText("Quality") as HTMLSelectElement).value).toBe("720p"));
    const input = screen.getByLabelText("YouTube URL");
    fireEvent.paste(input, { clipboardData: { getData: () => URL } });
    await screen.findByText("Sunday");
    expect(calls("get_video_info")).toEqual([{ url: URL }]);
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(calls("download_video")).toHaveLength(1));
    expect(calls("download_video")[0]).toEqual({ url: URL, quality: "720p", process: true });
    // The user went elsewhere before it finished: stay there.
    rerender(<DownloadForm onGoToLibrary={onGoToLibrary} isActive={false} />);
    finishDownload("vid");
    await waitFor(() => expect(screen.queryByText("Sunday")).toBeNull());
    expect(onGoToLibrary).not.toHaveBeenCalled();
  });
});
