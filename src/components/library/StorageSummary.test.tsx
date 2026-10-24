import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { StorageView } from "@/lib/types";
import { StorageSummary, storageFigures } from "./StorageSummary";

const GB = 1e9;
const storage = (free: number, videos: [number, number][] = [[11 * GB, 4 * GB], [10 * GB, 0]]): StorageView => ({
  videos: videos.map(([original, files], i) => ({ id: `v${i}`, original, files })),
  other: 0,
  disk_free: free,
  disk_total: 1000 * GB,
});

describe("storageFigures", () => {
  it("sizes a typical video from the processed ones", () => {
    const f = storageFigures(storage(300 * GB));
    expect(f.library).toBe(25 * GB);
    expect(f.elsewhere).toBe(675 * GB);
    expect(f.perVideo).toBe(15 * GB);
    expect(f.roomFor).toBe(20);
    expect(f.tone).toBe("ok");
  });

  it("falls back to the recordings when nothing is processed yet", () => {
    const f = storageFigures(storage(100 * GB, [[10 * GB, 0], [20 * GB, 0]]));
    expect(f.perVideo).toBe(15 * GB);
  });

  it("warns as the room runs out", () => {
    expect(storageFigures(storage(120 * GB)).tone).toBe("low");
    expect(storageFigures(storage(40 * GB)).tone).toBe("very_low");
  });

  it("makes no estimate for an empty library", () => {
    const f = storageFigures(storage(500 * GB, []));
    expect(f.perVideo).toBeNull();
    expect(f.tone).toBe("ok");
  });
});

describe("StorageSummary", () => {
  it("shows the free space and the library's share", () => {
    render(<StorageSummary storage={storage(300 * GB)} folder="/Users/me/Movies/sermon-cut" />);
    expect(screen.getByText("300 GB free")).toBeTruthy();
    expect(screen.getByText(/of 1.0 TB on this Mac/)).toBeTruthy();
    expect(screen.getByText(/room for about 20 more/)).toBeTruthy();
    expect(screen.queryByText(/getting low/)).toBeNull();
  });

  it("names an external drive as a drive, and warns when it's filling up", () => {
    render(<StorageSummary storage={storage(120 * GB)} folder="/Volumes/Sermons/library" />);
    expect(screen.getByText(/on this drive/)).toBeTruthy();
    expect(screen.getByText(/Space is getting low/)).toBeTruthy();
  });

  it("shows nothing when the drive couldn't be read", () => {
    const { container } = render(
      <StorageSummary storage={{ ...storage(0), disk_free: null, disk_total: null }} folder="/x" />
    );
    expect(container.innerHTML).toBe("");
  });
});
