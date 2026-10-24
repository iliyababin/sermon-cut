import { error as pluginError, info as pluginInfo, warn as pluginWarn } from "@tauri-apps/plugin-log";

/** The webview's side of the app log (the file itself is written by the
 * backend, which also masks secrets in it). Warnings, errors, uncaught
 * exceptions and unhandled rejections go there as well as to the console;
 * release builds have no console to look at. */

type Level = "info" | "warn" | "error";

const send = { info: pluginInfo, warn: pluginWarn, error: pluginError };

/** Set while a line is on its way: anything a log call itself sets off
 * (a console.error inside it, a rejection) isn't logged again. */
let forwarding = false;

/** Text for one console argument. */
function describe(arg: unknown): string {
  if (arg instanceof Error) return arg.stack || `${arg.name}: ${arg.message}`;
  if (typeof arg === "string") return arg;
  try {
    return JSON.stringify(arg) ?? String(arg);
  } catch {
    return String(arg);
  }
}

/** Write `parts` to the app log. Never throws, never rejects. */
export function log(level: Level, ...parts: unknown[]) {
  if (forwarding) return;
  forwarding = true;
  try {
    send[level](parts.map(describe).join(" ")).catch(() => {});
  } catch {
    // Not in the app (tests), or the plugin is missing: the console has it.
  } finally {
    forwarding = false;
  }
}

/** Whether a rejection came from the log plugin's own call. */
function fromLogPlugin(reason: unknown): boolean {
  return /plugin[:.]log|\blog\.log\b|plugin log/i.test(describe(reason));
}

let installed = false;

/** Forward console.warn/console.error and uncaught errors to the log.
 * Once, from main.tsx. */
export function installLogging() {
  if (installed) return;
  installed = true;
  for (const level of ["warn", "error"] as const) {
    const original = console[level].bind(console);
    console[level] = (...args: unknown[]) => {
      original(...args);
      log(level, ...args);
    };
  }
  window.addEventListener("error", (event) => {
    log("error", "Uncaught:", event.error ?? event.message);
  });
  window.addEventListener("unhandledrejection", (event) => {
    if (fromLogPlugin(event.reason)) return;
    log("error", "Unhandled rejection:", event.reason);
  });
}

/** Errors shown to the user lately; each is logged once a minute at most
 * (a banner can render many times, twice per mount under StrictMode). */
const shown = new Set<string>();
const SHOWN_FOR_MS = 60_000;

/** Log an error the user was shown (ErrorBanner). */
export function logShown(message: string) {
  if (shown.has(message)) return;
  shown.add(message);
  setTimeout(() => shown.delete(message), SHOWN_FOR_MS);
  log("warn", "Shown to the user:", message);
}

/** For tests: forget what was installed and shown. */
export function resetLoggingForTests() {
  installed = false;
  forwarding = false;
  shown.clear();
}
