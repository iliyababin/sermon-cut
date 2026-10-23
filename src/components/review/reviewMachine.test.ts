import { describe, expect, it } from "vitest";
import { initialReviewState, reviewReducer, reviewShortcut, type ReviewEvent, type ReviewState, type ShortcutKey } from "./reviewMachine";

const fresh = initialReviewState({ status: "ready_for_review", uploadProgress: null, uploads: [] });

function run(state: ReviewState, ...events: (ReviewEvent | ReviewEvent[])[]): ReviewState {
  return events.flat().reduce(reviewReducer, state);
}

/** Upload clicked and confirmed. */
const UPLOAD: ReviewEvent[] = [{ type: "upload", ready: true }, { type: "confirm" }];

describe("reviewMachine", () => {
  it("starts from the video's status", () => {
    expect(fresh.phase).toEqual({ kind: "editing" });
    expect(initialReviewState({ status: "uploading", uploadProgress: 40, uploads: [] }).phase).toEqual({
      kind: "uploading",
      progress: 40,
      request: null,
    });
    const done = initialReviewState({ status: "done", uploadProgress: null, uploads: [{ url: "https://y/1" }] });
    expect(done.phase).toEqual({ kind: "done", url: "https://y/1", warnings: [] });
    expect(done.uploadCount).toBe(1);
  });

  it("asks first, then uploads", () => {
    const asking = run(fresh, { type: "upload", ready: true });
    expect(asking.phase).toEqual({ kind: "confirming" });
    const s = run(asking, { type: "confirm" });
    expect(s.phase).toEqual({ kind: "uploading", progress: 0, request: 1 });
  });

  it("ignores a second click while uploading", () => {
    const once = run(fresh, UPLOAD);
    const twice = run(once, { type: "upload", ready: true });
    expect(twice).toBe(once);
  });

  it("ignores Upload while the thumbnails aren't ready", () => {
    expect(run(fresh, { type: "upload", ready: false })).toBe(fresh);
  });

  it("asks before uploading again, and a cancelled confirm goes back to editing", () => {
    const uploaded = run(fresh, UPLOAD, { type: "succeeded", url: "u" }, { type: "uploadAgain" });
    expect(uploaded.uploadCount).toBe(1);
    const asking = run(uploaded, { type: "upload", ready: true });
    expect(asking.phase).toEqual({ kind: "confirming" });
    // Clicking again while the question is open does nothing.
    expect(run(asking, { type: "upload", ready: true })).toBe(asking);
    expect(run(asking, { type: "cancelConfirm" }).phase).toEqual({ kind: "editing" });
    expect(run(asking, { type: "confirm" }).phase).toEqual({ kind: "uploading", progress: 0, request: 2 });
  });

  it("counts a success once even when reported twice", () => {
    const s = run(fresh, UPLOAD, { type: "succeeded", url: "u" }, { type: "succeeded", url: "u" });
    expect(s.phase).toEqual({ kind: "done", url: "u", warnings: [] });
    expect(s.uploadCount).toBe(1);
  });

  it("keeps progress monotonic and follows uploads started elsewhere", () => {
    let s = run(fresh, UPLOAD, { type: "progress", percent: 50 }, { type: "progress", percent: 30 });
    expect(s.phase).toMatchObject({ kind: "uploading", progress: 50 });
    s = run(fresh, { type: "progress", percent: 10 });
    expect(s.phase).toEqual({ kind: "uploading", progress: 10, request: null });
  });

  it("can retry after an error or a cancel", () => {
    const failed = run(fresh, UPLOAD, { type: "failed", message: "boom" });
    expect(failed.phase).toEqual({ kind: "error", message: "boom" });
    expect(run(failed, UPLOAD).phase).toMatchObject({ kind: "uploading", request: 2 });
    const cancelled = run(fresh, UPLOAD, { type: "cancelled" });
    expect(cancelled.phase).toEqual({ kind: "cancelled" });
    expect(run(cancelled, UPLOAD).phase).toMatchObject({ kind: "uploading" });
  });

  it("cancels once, and the upload ends as cancelled", () => {
    const uploading = run(fresh, UPLOAD);
    const asked = run(uploading, { type: "cancelRequested" });
    expect(asked.cancelling).toBe(true);
    expect(run(asked, { type: "cancelRequested" })).toBe(asked);
    const stopped = run(asked, { type: "cancelled" });
    expect(stopped.phase).toEqual({ kind: "cancelled" });
    expect(stopped.cancelling).toBe(false);
    // Upload again continues (the backend resumes the session).
    expect(run(stopped, UPLOAD).phase).toMatchObject({ kind: "uploading", request: 2 });
    // Nothing to cancel when not uploading.
    expect(run(fresh, { type: "cancelRequested" })).toBe(fresh);
  });

  it("keeps the upload's warnings and updates them after a thumbnail retry", () => {
    const thumb = { kind: "thumbnail" as const, message: "The thumbnail wasn't set." };
    const playlist = { kind: "playlist" as const, message: "Not added to the playlist." };
    const done = run(
      fresh,
      UPLOAD,
      { type: "succeeded", url: "u", warnings: [thumb, playlist] }
    );
    expect(done.phase).toEqual({ kind: "done", url: "u", warnings: [thumb, playlist] });
    expect(run(done, { type: "warnings", warnings: [playlist] }).phase).toEqual({
      kind: "done",
      url: "u",
      warnings: [playlist],
    });
    expect(run(fresh, { type: "warnings", warnings: [] })).toBe(fresh);
    const reopened = initialReviewState({ status: "done", uploadProgress: null, uploads: [{ url: "u", warnings: [thumb] }] });
    expect(reopened.phase).toEqual({ kind: "done", url: "u", warnings: [thumb] });
  });

  it("ignores results that don't belong to the current phase", () => {
    expect(run(fresh, { type: "succeeded", url: "u" })).toBe(fresh);
    expect(run(fresh, { type: "failed", message: "x" })).toBe(fresh);
    expect(run(fresh, { type: "confirm" })).toBe(fresh);
    expect(run(fresh, { type: "uploadAgain" })).toBe(fresh);
  });
});

describe("reviewShortcut", () => {
  const key = (over: Partial<ShortcutKey>): ShortcutKey => ({
    key: "",
    metaKey: false,
    ctrlKey: false,
    inField: false,
    composing: false,
    blocked: false,
    ...over,
  });

  it("uploads on ⌘Enter or Ctrl+Enter, also from a textarea", () => {
    expect(reviewShortcut(key({ key: "Enter", metaKey: true }))).toBe("upload");
    expect(reviewShortcut(key({ key: "Enter", ctrlKey: true }))).toBe("upload");
    expect(reviewShortcut(key({ key: "Enter", metaKey: true, inField: true }))).toBe("upload");
    expect(reviewShortcut(key({ key: "Enter" }))).toBeNull();
  });

  it("goes back on Esc; in a field Esc leaves the field", () => {
    expect(reviewShortcut(key({ key: "Escape" }))).toBe("back");
    expect(reviewShortcut(key({ key: "Escape", inField: true }))).toBe("blur");
  });

  it("leaves keys to an input method that is composing", () => {
    expect(reviewShortcut(key({ key: "Escape", composing: true }))).toBeNull();
    expect(reviewShortcut(key({ key: "Escape", inField: true, composing: true }))).toBeNull();
    expect(reviewShortcut(key({ key: "Enter", metaKey: true, composing: true }))).toBeNull();
  });

  it("does nothing while a dialog or the editor is open", () => {
    expect(reviewShortcut(key({ key: "Escape", blocked: true }))).toBeNull();
    expect(reviewShortcut(key({ key: "Enter", metaKey: true, blocked: true }))).toBeNull();
  });
});
