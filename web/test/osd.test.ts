import { describe, expect, it } from "vitest";

import { FRAME_BYTES, GRID_WIDTH, HEADER_BYTES, osdFrameIndexAt, parseFcFirmware, parseOsdFile } from "../src/parsers/osd";

/** Build an OSD file with the given per-frame (timestamp, glyph list) contents. */
function osdBytes(firmware: string, frames: { time: number; glyphs: Array<[number, number, number]> }[]): Uint8Array {
  const bytes = new Uint8Array(HEADER_BYTES + frames.length * FRAME_BYTES);
  new TextEncoder().encodeInto(firmware, bytes);
  const view = new DataView(bytes.buffer);
  frames.forEach((frame, i) => {
    const base = HEADER_BYTES + i * FRAME_BYTES;
    view.setUint32(base, frame.time, true);
    for (const [index, x, y] of frame.glyphs) {
      view.setUint16(base + 4 + (y * GRID_WIDTH + x) * 2, index, true);
    }
  });
  return bytes;
}

describe("OSD file", () => {
  it("parses the firmware tag", () => {
    expect(parseFcFirmware("BTFL")).toBe("BetaFlight");
    expect(parseFcFirmware("INAV")).toBe("INAV");
    expect(parseFcFirmware("ARDU")).toBe("ArduPilot");
    expect(parseFcFirmware("KISS")).toBe("KISS");
    expect(parseFcFirmware("ULTR")).toBe("KISS ULTRA");
    expect(parseFcFirmware("WHAT")).toBe("Unknown");
  });

  it("parses frames, skipping blank and space glyphs", () => {
    const file = parseOsdFile(
      osdBytes("INAV", [
        { time: 0, glyphs: [[65, 0, 0], [0x20, 1, 0], [300, 52, 19]] },
        { time: 100, glyphs: [[66, 5, 3]] },
        { time: 200, glyphs: [] },
      ]),
    );
    expect(file.fcFirmware).toBe("INAV");
    expect(file.frameCount).toBe(3);
    expect(file.frames[0]!.glyphs).toEqual([
      { index: 65, x: 0, y: 0 },
      { index: 300, x: 52, y: 19 },
    ]);
    expect(file.frames[1]!.timeMillis).toBe(100);
    // last timestamp plus one average interval
    expect(file.durationSecs).toBeCloseTo(0.3);
  });

  it("selects the frame for a video time like the native renderer", () => {
    const frames = [0, 100, 200, 300].map((t) => ({ timeMillis: t, glyphs: [] }));
    expect(osdFrameIndexAt(frames, 0, 1)).toBe(0);
    expect(osdFrameIndexAt(frames, 0.05, 1)).toBe(0);
    expect(osdFrameIndexAt(frames, 0.1, 1)).toBe(0); // advances only once strictly past the frame time
    expect(osdFrameIndexAt(frames, 0.15, 1)).toBe(1);
    expect(osdFrameIndexAt(frames, 10, 1)).toBe(3);
    // Playback speed factor stretches the OSD timeline
    expect(osdFrameIndexAt(frames, 0.15, 2)).toBe(0);
    expect(osdFrameIndexAt(frames, 0.25, 2)).toBe(1);
  });

  it("rejects truncated files", () => {
    expect(() => parseOsdFile(new Uint8Array(100))).toThrow();
  });
});
