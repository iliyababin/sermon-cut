import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  convertFileSrc: (p: string) => `asset://${p}`,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: () => Promise.resolve(() => {}) }));

import { TrimPanel } from "./TrimPanel";

const segments = [
  { start: 100, end: 104, text: "Let's open our Bibles" },
  { start: 600, end: 603, text: "Let's pray" },
];

function panel(onTrimmed = vi.fn()) {
  render(
    <TrimPanel
      videoId="vid1"
      sourcePath="/lib/a.mkv"
      duration={900}
      start={100}
      end={600}
      segments={segments}
      onTrimmed={onTrimmed}
    />
  );
  return onTrimmed;
}

afterEach(() => {
  cleanup();
  invoke.mockReset();
});

describe("TrimPanel", () => {
  it("shows each point with what is being said there", () => {
    panel();
    expect(screen.getByLabelText("Start time").textContent).toBe("1:40.0");
    expect(screen.getByLabelText("End time").textContent).toBe("10:00.0");
    expect(screen.getByText("“Let's open our Bibles”")).toBeTruthy();
    expect(screen.getByText("“Let's pray”")).toBeTruthy();
  });

  it("nudges and trims, then shows where the cut really starts", async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === "set_trim" ? Promise.resolve({ start: 102, end: 595 }) : Promise.resolve(null)
    );
    const onTrimmed = panel();
    fireEvent.click(screen.getByLabelText("Start +5 s"));
    fireEvent.click(screen.getByLabelText("Start −1 s"));
    fireEvent.click(screen.getByLabelText("End −5 s"));
    expect(screen.getByLabelText("Start time").textContent).toBe("1:44.0");
    fireEvent.click(screen.getByText("Trim video"));
    await waitFor(() => expect(onTrimmed).toHaveBeenCalledWith(102, 595));
    expect(invoke).toHaveBeenCalledWith("set_trim", { videoId: "vid1", start: 104, end: 595 });
  });

  it("refuses a trim shorter than a second", () => {
    render(
      <TrimPanel videoId="v" sourcePath="/a.mp4" duration={10} start={0} end={10} segments={[]} onTrimmed={vi.fn()} />
    );
    for (let i = 0; i < 2; i++) fireEvent.click(screen.getByLabelText("End −5 s"));
    expect(screen.getByText("The end must be at least a second after the start.")).toBeTruthy();
    expect((screen.getByText("Trim video").closest("button") as HTMLButtonElement).disabled).toBe(true);
  });

  it("falls back to stills when the video can't play", async () => {
    invoke.mockImplementation((cmd: string, args: { time: number }) =>
      cmd === "frame_still" ? Promise.resolve(`/tmp/still-${args.time}.jpg`) : Promise.resolve(null)
    );
    panel();
    fireEvent.error(screen.getByLabelText("Original video"));
    expect(screen.queryByText("Use current frame")).toBeNull();
    await waitFor(() => expect(screen.getByAltText("Start frame").getAttribute("src")).toBe("asset:///tmp/still-100.jpg"));
    expect(invoke).toHaveBeenCalledWith("frame_still", { videoId: "vid1", time: 600 });
  });
});
