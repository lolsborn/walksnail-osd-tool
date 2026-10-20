// Port of backend/src/osd: the Walksnail .osd file format.
//
// Layout: a 40 byte header whose first 4 bytes name the flight controller firmware, followed by
// frames of 2124 bytes each: a little-endian u32 timestamp in milliseconds and 53 x 20 glyph
// indices as little-endian u16.

export const GRID_WIDTH = 53;
export const GRID_HEIGHT = 20;
export const HEADER_BYTES = 40;
const FC_TYPE_BYTES = 4;
const TIMESTAMP_BYTES = 4;
const BYTES_PER_GLYPH = 2;
export const FRAME_BYTES = TIMESTAMP_BYTES + GRID_WIDTH * GRID_HEIGHT * BYTES_PER_GLYPH; // 2124

export type FcFirmware = "BetaFlight" | "INAV" | "ArduPilot" | "KISS" | "KISS ULTRA" | "Unknown";

export interface Glyph {
  /** Index into the font's character list. */
  index: number;
  x: number;
  y: number;
}

export interface OsdFrame {
  timeMillis: number;
  /** Visible glyphs only: index 0 and the space (0x20) are omitted, like the native parser. */
  glyphs: Glyph[];
}

export interface OsdFile {
  fcFirmware: FcFirmware;
  frameCount: number;
  /** Seconds. Last timestamp plus one average frame interval, like the native parser. */
  durationSecs: number;
  frames: OsdFrame[];
}

export function parseFcFirmware(tag: string): FcFirmware {
  switch (tag) {
    case "BTFL":
      return "BetaFlight";
    case "INAV":
      return "INAV";
    case "ARDU":
      return "ArduPilot";
    case "KISS":
      return "KISS";
    case "ULTR":
      return "KISS ULTRA";
    default:
      return "Unknown";
  }
}

export function parseOsdFrame(bytes: Uint8Array, offset: number): OsdFrame {
  const view = new DataView(bytes.buffer, bytes.byteOffset + offset, FRAME_BYTES);
  const timeMillis = view.getUint32(0, true);
  const glyphs: Glyph[] = [];
  const count = GRID_WIDTH * GRID_HEIGHT;
  for (let i = 0; i < count; i++) {
    const index = view.getUint16(TIMESTAMP_BYTES + i * BYTES_PER_GLYPH, true);
    if (index === 0x00 || index === 0x20) continue;
    glyphs.push({ index, x: i % GRID_WIDTH, y: Math.floor(i / GRID_WIDTH) });
  }
  return { timeMillis, glyphs };
}

export function parseOsdFile(bytes: Uint8Array): OsdFile {
  if (bytes.length < HEADER_BYTES + FRAME_BYTES) {
    throw new Error("OSD file is too short to contain a header and a frame");
  }
  const tag = new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, FC_TYPE_BYTES));
  const fcFirmware = parseFcFirmware(tag);
  const frameCount = Math.floor((bytes.length - HEADER_BYTES) / FRAME_BYTES);
  const frames: OsdFrame[] = new Array(frameCount);
  for (let i = 0; i < frameCount; i++) {
    frames[i] = parseOsdFrame(bytes, HEADER_BYTES + i * FRAME_BYTES);
  }
  const first = frames[0]!;
  const last = frames[frameCount - 1]!;
  const intervalMillis = frameCount > 1 ? (last.timeMillis - first.timeMillis) / (frameCount - 1) : 0;
  return {
    fcFirmware,
    frameCount,
    durationSecs: (last.timeMillis + intervalMillis) / 1000,
    frames,
  };
}

/**
 * The OSD frame to show at video time `tSecs`, with the same rule as the native renderer: start
 * on the first frame and advance while the next frame's (speed adjusted) time is before `tSecs`.
 */
export function osdFrameIndexAt(frames: OsdFrame[], tSecs: number, playbackSpeedFactor: number): number {
  let lo = 0;
  let hi = frames.length - 1;
  // Largest i with frames[i].time * factor < tSecs, or 0
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if ((frames[mid]!.timeMillis / 1000) * playbackSpeedFactor < tSecs) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}
