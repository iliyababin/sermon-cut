import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { Settings as SettingsType } from "@/lib/types";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  convertFileSrc: (p: string) => `asset://${p}`,
}));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: () => Promise.resolve("0.5.0") }));
const pick = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: (...a: unknown[]) => pick(...a) }));
vi.mock("@/hooks/useUpdater", () => ({
  useUpdater: () => ({
    checked: false,
    checking: false,
    available: false,
    downloading: false,
    version: null,
    progress: 0,
    error: null,
    checkForUpdates: vi.fn(),
    downloadAndInstall: vi.fn(),
  }),
}));

import { Settings } from "./Settings";

const initial: SettingsType = {
  download_quality: "best",
  output_folder: "/lib",
  modal_api_url: "https://x",
  modal_api_key: null,
  logo_path: null,
  youtube_playlist_id: null,
  upload_privacy: "public",
  youtube_signed_in: false,
  youtube_channel_name: null,  recent_preachers: [],
  auto_process: false,
};

function saves() {
  return invoke.mock.calls
    .filter(([c]) => c === "save_settings")
    .map(([, a]) => a as { patch: Record<string, unknown>; seq: number });
}

let current: SettingsType;

beforeEach(() => {
  current = { ...initial };
  invoke.mockReset();
  pick.mockReset();
  invoke.mockImplementation((cmd: string, args?: { patch?: Record<string, unknown> }) => {
    switch (cmd) {
      case "get_settings":
        return Promise.resolve({ ...current });
      case "get_app_folders":
        return Promise.resolve({ settings: "/data/sermon-cut", tools: "/data/com.youthscc.sermon-cut/bin" });
      case "youtube_get_auth_status":
        return Promise.resolve({ is_authenticated: false, channel_name: null, channel_id: null });
      case "check_library_folder":
        return Promise.resolve([]);
      case "save_settings":
        current = { ...current, ...(args?.patch ?? {}) } as SettingsType;
        return Promise.resolve({ ...current });
      default:
        return Promise.resolve(null);
    }
  });
});

afterEach(cleanup);

describe("Settings autosave", () => {
  it("saves each edited field on its own, in order, after typing pauses", async () => {
    render(<Settings />);
    const url = await screen.findByLabelText("Modal API URL");
    const key = screen.getByLabelText("API Key (Optional)");
    fireEvent.change(url, { target: { value: "https://y" } });
    fireEvent.change(url, { target: { value: "https://yz" } });
    fireEvent.change(key, { target: { value: "secret" } });
    // Nothing is sent while typing.
    expect(saves()).toHaveLength(0);
    await waitFor(() => expect(saves()).toHaveLength(2), { timeout: 2000 });
    const [first, second] = saves();
    expect(first.patch).toEqual({ modal_api_url: "https://yz" });
    expect(second.patch).toEqual({ modal_api_key: "secret" });
    expect(second.seq).toBeGreaterThan(first.seq);
    await screen.findByText("Saved");
    expect(screen.queryByRole("button", { name: /save settings/i })).toBeNull();
  });

  it("applies a picked folder at once and shows a refusal inline", async () => {
    pick.mockResolvedValue("/new");
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "save_settings" ? Promise.reject("Wait for the upload to finish.") : base(cmd, args)
    );
    render(<Settings />);
    fireEvent.click(await screen.findByTitle("Choose folder"));
    await waitFor(() => expect(saves()).toHaveLength(1));
    expect(saves()[0].patch).toEqual({ output_folder: "/new" });
    expect((await screen.findByRole("alert")).textContent).toBe("Wait for the upload to finish.");
    // The folder in use is shown again.
    await waitFor(() => expect((screen.getByLabelText("Library Folder") as HTMLInputElement).value).toBe("/lib"));
  });

  it("saves the auto-process switch at once", async () => {
    render(<Settings />);
    const toggle = (await screen.findByLabelText(/Process new videos automatically/)) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    fireEvent.click(toggle);
    await waitFor(() => expect(saves()).toHaveLength(1));
    expect(saves()[0].patch).toEqual({ auto_process: true });
    await waitFor(() => expect(toggle.checked).toBe(true));
  });

  it("saves a typed folder only on Enter", async () => {
    render(<Settings />);
    const folder = await screen.findByLabelText("Library Folder");
    fireEvent.change(folder, { target: { value: "/typ" } });
    await new Promise((r) => setTimeout(r, 800));
    expect(saves()).toHaveLength(0);
    fireEvent.keyDown(folder, { key: "Enter" });
    await waitFor(() => expect(saves()).toHaveLength(1));
    expect(saves()[0].patch).toEqual({ output_folder: "/typ" });
  });

  it("ignores a response older than one already applied", async () => {
    // The first save answers last, with its (older) view of the settings.
    const resolvers: Array<() => void> = [];
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: { patch?: Record<string, unknown> }) => {
      if (cmd !== "save_settings") return base(cmd, args);
      const snapshot = { ...current, ...(args?.patch ?? {}) } as SettingsType;
      current = snapshot;
      return new Promise((r) => resolvers.push(() => r(snapshot)));
    });
    render(<Settings />);
    const quality = (await screen.findByLabelText("Default Quality")) as HTMLSelectElement;
    fireEvent.change(quality, { target: { value: "720p" } });
    fireEvent.change(quality, { target: { value: "480p" } });
    await waitFor(() => expect(saves()).toHaveLength(2));
    resolvers[1]();
    await new Promise((r) => setTimeout(r, 20));
    resolvers[0]();
    await screen.findByText("Saved");
    await new Promise((r) => setTimeout(r, 20));
    expect(quality.value).toBe("480p");
  });

  it("keeps both fields when their saves answer out of order", async () => {
    // The backend applied the newer save (another field) first.
    const answers: Array<(s: SettingsType) => void> = [];
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "save_settings" ? new Promise((r) => answers.push(r)) : base(cmd, args)
    );
    render(<Settings />);
    const quality = (await screen.findByLabelText("Default Quality")) as HTMLSelectElement;
    const toggle = screen.getByLabelText(/Process new videos automatically/) as HTMLInputElement;
    fireEvent.change(quality, { target: { value: "720p" } });
    fireEvent.click(toggle);
    await waitFor(() => expect(saves()).toHaveLength(2));
    const [first, second] = saves();
    expect(first.patch).toEqual({ download_quality: "720p" });
    expect(second.patch).toEqual({ auto_process: true });
    expect(second.seq).toBe(first.seq + 1);
    answers[1]({ ...initial, auto_process: true });
    await new Promise((r) => setTimeout(r, 20));
    answers[0]({ ...initial, download_quality: "720p", auto_process: true });
    await screen.findByText("Saved");
    expect(quality.value).toBe("720p");
    expect(toggle.checked).toBe(true);
    // The 720p answer was taken: going back to "best" is a change to save.
    fireEvent.change(quality, { target: { value: "best" } });
    await waitFor(() => expect(saves()).toHaveLength(3));
    expect(saves()[2].patch).toEqual({ download_quality: "best" });
  });

  it("shows the settings and tools folders", async () => {
    render(<Settings />);
    await screen.findByText("/data/sermon-cut");
    expect(screen.getByText("/data/com.youthscc.sermon-cut/bin")).toBeTruthy();
  });
});
