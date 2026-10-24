import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode, createElement } from "react";
import { cleanup, render } from "@testing-library/react";

const plugin = vi.hoisted(() => ({
  info: vi.fn((_message: string) => Promise.resolve()),
  warn: vi.fn((_message: string) => Promise.resolve()),
  error: vi.fn((_message: string) => Promise.resolve()),
}));
vi.mock("@tauri-apps/plugin-log", () => plugin);

import { installLogging, log, resetLoggingForTests } from "./log";
import { ErrorBanner } from "@/components/ErrorBanner";

const realWarn = console.warn;
const realError = console.error;

beforeEach(() => {
  resetLoggingForTests();
  for (const fn of Object.values(plugin)) fn.mockReset().mockImplementation(() => Promise.resolve());
});

afterEach(() => {
  cleanup();
  console.warn = realWarn;
  console.error = realError;
});

describe("app log", () => {
  it("console.error goes to the console and to the log, once", () => {
    const original = vi.fn();
    console.error = original;
    installLogging();
    installLogging();
    console.error("Failed to load image:", new Error("gone"));
    expect(original).toHaveBeenCalledTimes(1);
    expect(original).toHaveBeenCalledWith("Failed to load image:", expect.any(Error));
    expect(plugin.error).toHaveBeenCalledTimes(1);
    expect(plugin.error.mock.calls[0][0]).toMatch(/^Failed to load image: Error: gone/);
  });

  it("a log call that fails doesn't come back round", async () => {
    console.error = vi.fn();
    installLogging();
    // The plugin rejects, and logs to the console while it's at it.
    plugin.error.mockImplementation(() => {
      console.error("inside the plugin");
      return Promise.reject(new Error("plugin:log|log not allowed"));
    });
    log("error", "first");
    await new Promise((r) => setTimeout(r, 0));
    expect(plugin.error).toHaveBeenCalledTimes(1);

    // A rejection from the plugin's own invoke is never logged.
    const event = new Event("unhandledrejection") as Event & { reason: unknown };
    event.reason = new Error("plugin:log|log not allowed");
    window.dispatchEvent(event);
    expect(plugin.error).toHaveBeenCalledTimes(1);

    // Another one is.
    plugin.error.mockImplementation(() => Promise.resolve());
    const other = new Event("unhandledrejection") as Event & { reason: unknown };
    other.reason = "the disk is full";
    window.dispatchEvent(other);
    expect(plugin.error).toHaveBeenLastCalledWith("Unhandled rejection: the disk is full");
  });

  it("a plugin that throws outright is shrugged off", () => {
    plugin.warn.mockImplementation(() => {
      throw new Error("no IPC here");
    });
    expect(() => log("warn", "x")).not.toThrow();
  });

  it("an error banner is logged once under StrictMode", () => {
    render(createElement(StrictMode, null, createElement(ErrorBanner, { message: "The drive isn't connected." })));
    render(createElement(StrictMode, null, createElement(ErrorBanner, { message: "The drive isn't connected." })));
    const shown = plugin.warn.mock.calls.filter((c) => String(c[0]).includes("The drive isn't connected."));
    expect(shown).toHaveLength(1);
  });
});
