import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
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

import { AUTOSAVE_DELAY_MS, Settings } from "./Settings";

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

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

/** Let `ms` of (fake) time pass, settling promises along the way. */
async function tick(ms = 0) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

/** Render and wait for the settings to load. */
async function renderSettings() {
  vi.useFakeTimers();
  render(<Settings />);
  await tick();
}

describe("Settings autosave", () => {
  it("saves each edited field on its own, in order, after typing pauses", async () => {
    await renderSettings();
    const url = screen.getByLabelText("Modal API URL");
    const key = screen.getByLabelText("API Key (Optional)");
    fireEvent.change(url, { target: { value: "https://y" } });
    fireEvent.change(url, { target: { value: "https://yz" } });
    fireEvent.change(key, { target: { value: "secret" } });
    // Nothing is sent while typing.
    await tick(AUTOSAVE_DELAY_MS - 1);
    expect(saves()).toHaveLength(0);
    await tick(1);
    expect(saves()).toHaveLength(2);
    const [first, second] = saves();
    expect(first.patch).toEqual({ modal_api_url: "https://yz" });
    expect(second.patch).toEqual({ modal_api_key: "secret" });
    expect(second.seq).toBeGreaterThan(first.seq);
    expect(screen.getByText("Saved")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /save settings/i })).toBeNull();
  });

  it("applies a picked folder at once and shows a refusal inline", async () => {
    pick.mockResolvedValue("/new");
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "save_settings" ? Promise.reject("Wait for the upload to finish.") : base(cmd, args)
    );
    await renderSettings();
    fireEvent.click(screen.getByTitle("Choose folder"));
    await tick();
    expect(saves()).toHaveLength(1);
    expect(saves()[0].patch).toEqual({ output_folder: "/new" });
    expect(screen.getByRole("alert").textContent).toBe("Wait for the upload to finish.");
    // The folder in use is shown again.
    expect((screen.getByLabelText("Library Folder") as HTMLInputElement).value).toBe("/lib");
  });

  it("saves a click made as soon as the settings show", async () => {
    // Click in the same task as the render that shows the settings, before
    // React has run its effects (what a quick user, or findBy, can do).
    let clicked = false;
    const observer = new MutationObserver(() => {
      const toggle = document.querySelector<HTMLInputElement>('input[type="checkbox"]');
      if (toggle && !clicked) {
        clicked = true;
        toggle.click();
      }
    });
    observer.observe(document.body, { childList: true, subtree: true });
    render(<Settings />);
    await screen.findByText("Saved", undefined, { timeout: 3000 });
    observer.disconnect();
    expect(clicked).toBe(true);
    expect(saves().map((s) => s.patch)).toEqual([{ auto_process: true }]);
  });

  it("saves the auto-process switch at once", async () => {
    await renderSettings();
    const toggle = screen.getByLabelText(/Process new videos automatically/) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    fireEvent.click(toggle);
    await tick();
    expect(saves()).toHaveLength(1);
    expect(saves()[0].patch).toEqual({ auto_process: true });
    expect(toggle.checked).toBe(true);
  });

  it("sends on→off→on even while the off is still in flight", async () => {
    current = { ...initial, auto_process: true };
    const answers: Array<() => void> = [];
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: { patch?: Record<string, unknown> }) => {
      if (cmd !== "save_settings") return base(cmd, args);
      current = { ...current, ...(args?.patch ?? {}) } as SettingsType;
      const snapshot = { ...current };
      return new Promise((r) => answers.push(() => r(snapshot)));
    });
    await renderSettings();
    const toggle = screen.getByLabelText(/Process new videos automatically/) as HTMLInputElement;
    fireEvent.click(toggle); // off, unanswered
    await tick();
    fireEvent.click(toggle); // on again
    await tick();
    expect(saves().map((s) => s.patch)).toEqual([{ auto_process: false }, { auto_process: true }]);
    answers[0]();
    answers[1]();
    await tick();
    expect(current.auto_process).toBe(true);
    expect(toggle.checked).toBe(true);
  });

  it("rolls a field back to the saved value when its save fails", async () => {
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "save_settings" ? Promise.reject("The disk is full.") : base(cmd, args)
    );
    await renderSettings();
    const quality = screen.getByLabelText("Default Quality") as HTMLSelectElement;
    fireEvent.change(quality, { target: { value: "720p" } });
    await tick();
    expect(saves()).toHaveLength(1);
    expect(screen.getByRole("alert").textContent).toBe("The disk is full.");
    expect(quality.value).toBe("best");
    // Choosing 720p again is a change to send again.
    invoke.mockImplementation(base);
    fireEvent.change(quality, { target: { value: "720p" } });
    await tick();
    expect(saves().map((s) => s.patch)).toEqual([{ download_quality: "720p" }, { download_quality: "720p" }]);
    expect(quality.value).toBe("720p");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("saves a typed folder only on Enter", async () => {
    await renderSettings();
    const folder = screen.getByLabelText("Library Folder");
    fireEvent.change(folder, { target: { value: "/typ" } });
    await tick(10 * AUTOSAVE_DELAY_MS);
    expect(saves()).toHaveLength(0);
    fireEvent.keyDown(folder, { key: "Enter" });
    await tick();
    expect(saves()).toHaveLength(1);
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
    await renderSettings();
    const quality = screen.getByLabelText("Default Quality") as HTMLSelectElement;
    fireEvent.change(quality, { target: { value: "720p" } });
    fireEvent.change(quality, { target: { value: "480p" } });
    await tick();
    expect(saves()).toHaveLength(2);
    resolvers[1]();
    await tick();
    resolvers[0]();
    await tick();
    expect(screen.getByText("Saved")).toBeTruthy();
    expect(quality.value).toBe("480p");
  });

  it("keeps both fields when their saves answer out of order", async () => {
    // The backend applied the newer save (another field) first.
    const answers: Array<(s: SettingsType) => void> = [];
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, args?: unknown) =>
      cmd === "save_settings" ? new Promise((r) => answers.push(r)) : base(cmd, args)
    );
    await renderSettings();
    const quality = screen.getByLabelText("Default Quality") as HTMLSelectElement;
    const toggle = screen.getByLabelText(/Process new videos automatically/) as HTMLInputElement;
    fireEvent.change(quality, { target: { value: "720p" } });
    fireEvent.click(toggle);
    await tick();
    expect(saves()).toHaveLength(2);
    const [first, second] = saves();
    expect(first.patch).toEqual({ download_quality: "720p" });
    expect(second.patch).toEqual({ auto_process: true });
    expect(second.seq).toBe(first.seq + 1);
    answers[1]({ ...initial, auto_process: true });
    await tick();
    answers[0]({ ...initial, download_quality: "720p", auto_process: true });
    await tick();
    expect(screen.getByText("Saved")).toBeTruthy();
    expect(quality.value).toBe("720p");
    expect(toggle.checked).toBe(true);
    // The 720p answer was taken: going back to "best" is a change to save.
    fireEvent.change(quality, { target: { value: "best" } });
    await tick();
    expect(saves()).toHaveLength(3);
    expect(saves()[2].patch).toEqual({ download_quality: "best" });
  });

  it("shows the settings and tools folders", async () => {
    await renderSettings();
    expect(screen.getByText("/data/sermon-cut")).toBeTruthy();
    expect(screen.getByText("/data/com.youthscc.sermon-cut/bin")).toBeTruthy();
  });
});
