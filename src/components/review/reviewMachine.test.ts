import { describe, expect, it } from "vitest";
import { initialReviewState, reviewReducer, type ReviewEvent, type ReviewState } from "./reviewMachine";

const fresh = initialReviewState({ status: "ready_for_review", uploadProgress: null, uploads: [] });

function run(state: ReviewState, ...events: ReviewEvent[]): ReviewState {
  return events.reduce(reviewReducer, state);
}

describe("reviewMachine", () => {
  it("starts from the video's status", () => {
    expect(fresh.phase).toEqual({ kind: "editing" });
    expect(initialReviewState({ status: "uploading", uploadProgress: 40, uploads: [] }).phase).toEqual({
      kind: "uploading",
      progress: 40,
      request: null,
    });
    const done = initialReviewState({ status: "done", uploadProgress: null, uploads: [{ url: "https://y/1" }] });
    expect(done.phase).toEqual({ kind: "done", url: "https://y/1" });
    expect(done.uploadCount).toBe(1);
  });

  it("uploads straight away the first time", () => {
    const s = run(fresh, { type: "upload", ready: true });
    expect(s.phase).toEqual({ kind: "uploading", progress: 0, request: 1 });
  });

  it("ignores a second click while uploading", () => {
    const once = run(fresh, { type: "upload", ready: true });
    const twice = run(once, { type: "upload", ready: true });
    expect(twice).toBe(once);
  });

  it("ignores Upload while the thumbnails aren't ready", () => {
    expect(run(fresh, { type: "upload", ready: false })).toBe(fresh);
  });

  it("asks before uploading again, and a cancelled confirm goes back to editing", () => {
    const uploaded = run(fresh, { type: "upload", ready: true }, { type: "succeeded", url: "u" }, { type: "uploadAgain" });
    expect(uploaded.uploadCount).toBe(1);
    const asking = run(uploaded, { type: "upload", ready: true });
    expect(asking.phase).toEqual({ kind: "confirming" });
    // Clicking again while the question is open does nothing.
    expect(run(asking, { type: "upload", ready: true })).toBe(asking);
    expect(run(asking, { type: "cancelConfirm" }).phase).toEqual({ kind: "editing" });
    expect(run(asking, { type: "confirm" }).phase).toEqual({ kind: "uploading", progress: 0, request: 2 });
  });

  it("counts a success once even when reported twice", () => {
    const s = run(fresh, { type: "upload", ready: true }, { type: "succeeded", url: "u" }, { type: "succeeded", url: "u" });
    expect(s.phase).toEqual({ kind: "done", url: "u" });
    expect(s.uploadCount).toBe(1);
  });

  it("keeps progress monotonic and follows uploads started elsewhere", () => {
    let s = run(fresh, { type: "upload", ready: true }, { type: "progress", percent: 50 }, { type: "progress", percent: 30 });
    expect(s.phase).toMatchObject({ kind: "uploading", progress: 50 });
    s = run(fresh, { type: "progress", percent: 10 });
    expect(s.phase).toEqual({ kind: "uploading", progress: 10, request: null });
  });

  it("can retry after an error or a cancel", () => {
    const failed = run(fresh, { type: "upload", ready: true }, { type: "failed", message: "boom" });
    expect(failed.phase).toEqual({ kind: "error", message: "boom" });
    expect(run(failed, { type: "upload", ready: true }).phase).toMatchObject({ kind: "uploading", request: 2 });
    const cancelled = run(fresh, { type: "upload", ready: true }, { type: "cancelled" });
    expect(cancelled.phase).toEqual({ kind: "cancelled" });
    expect(run(cancelled, { type: "upload", ready: true }).phase).toMatchObject({ kind: "uploading" });
  });

  it("ignores results that don't belong to the current phase", () => {
    expect(run(fresh, { type: "succeeded", url: "u" })).toBe(fresh);
    expect(run(fresh, { type: "failed", message: "x" })).toBe(fresh);
    expect(run(fresh, { type: "confirm" })).toBe(fresh);
    expect(run(fresh, { type: "uploadAgain" })).toBe(fresh);
  });
});
