import { describe, expect, it } from "vitest";
import { settingsPatch } from "./settingsPatch";
import type { Settings } from "./types";

const saved: Settings = {
  download_quality: "best",
  output_folder: "/lib",
  modal_api_url: "https://x",
  modal_api_key: "k",
  logo_path: null,
  youtube_playlist_id: "PL1",
  upload_privacy: "public",
  youtube_signed_in: true,
  youtube_channel_name: "Youth",
};

describe("settingsPatch", () => {
  it("is empty when nothing changed", () => {
    expect(settingsPatch(saved, { ...saved })).toEqual({});
  });

  it("sends only edited fields, with cleared ones as empty strings", () => {
    const edited = { ...saved, download_quality: "720p", modal_api_key: null, logo_path: "/l.png" };
    expect(settingsPatch(saved, edited)).toEqual({
      download_quality: "720p",
      modal_api_key: "",
      logo_path: "/l.png",
    });
  });

  it("never sends app-owned fields", () => {
    const edited: Settings = { ...saved, upload_privacy: "private", youtube_signed_in: false, youtube_channel_name: null };
    expect(settingsPatch(saved, edited)).toEqual({});
  });
});
