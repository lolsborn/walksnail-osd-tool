// Port of backend/src/srt: the Walksnail .srt telemetry file and the text line drawn from it.

export interface SrtFrameData {
  signal: number;
  channel: number;
  flightTime: number;
  skyBat: number;
  groundBat: number;
  latency: number;
  bitrateMbps: number;
  distance: number;
}

export interface SrtFrame {
  startTimeSecs: number;
  endTimeSecs: number;
  /** Parsed telemetry, or undefined for lines in a format this parser does not know. */
  data?: SrtFrameData;
  /** True for the debug line format of firmware 37.42.3 and later. */
  isDebug: boolean;
}

export interface SrtFile {
  hasDistance: boolean;
  hasDebug: boolean;
  durationSecs: number;
  frames: SrtFrame[];
}

const DATA_LINE =
  /^Signal:(\d+) CH:(\d+) FlightTime:(\d+) SBat:([\d.]+)V GBat:([\d.]+)V Delay:(\d+)ms Bitrate:([\d.]+)Mbps Distance:(\d+)m$/;
const DEBUG_LINE = /^CH:(\d+) MCS:(\d+) SP\[/;
const TIME_RANGE = /^(\d+):(\d+):(\d+)[,.](\d+)\s*-->\s*(\d+):(\d+):(\d+)[,.](\d+)/;

export function parseSrtFrameData(line: string): SrtFrameData | undefined {
  const m = DATA_LINE.exec(line.trim());
  if (!m) return undefined;
  return {
    signal: Number(m[1]),
    channel: Number(m[2]),
    flightTime: Number(m[3]),
    skyBat: Number(m[4]),
    groundBat: Number(m[5]),
    latency: Number(m[6]),
    bitrateMbps: Number(m[7]),
    distance: Number(m[8]),
  };
}

function timeToSecs(h: string, m: string, s: string, ms: string): number {
  return Number(h) * 3600 + Number(m) * 60 + Number(s) + Number(ms.padEnd(3, "0").slice(0, 3)) / 1000;
}

export function parseSrtFile(text: string): SrtFile {
  const frames: SrtFrame[] = [];
  let hasDistance = false;
  let hasDebug = false;
  const blocks = text.replace(/\r\n?/g, "\n").split(/\n\s*\n/);
  for (const block of blocks) {
    const lines = block.split("\n").filter((l) => l.length > 0);
    const rangeIndex = lines.findIndex((l) => TIME_RANGE.test(l));
    if (rangeIndex < 0) continue;
    const m = TIME_RANGE.exec(lines[rangeIndex]!)!;
    const payload = lines.slice(rangeIndex + 1).join("\n");
    const data = parseSrtFrameData(payload);
    const isDebug = DEBUG_LINE.test(payload);
    if (isDebug) hasDebug = true;
    if (data && data.distance > 0) hasDistance = true;
    frames.push({
      startTimeSecs: timeToSecs(m[1]!, m[2]!, m[3]!, m[4]!),
      endTimeSecs: timeToSecs(m[5]!, m[6]!, m[7]!, m[8]!),
      data,
      isDebug,
    });
  }
  if (frames.length === 0) throw new Error("No subtitle entries found in the SRT file");
  return { hasDistance, hasDebug, durationSecs: frames[frames.length - 1]!.endTimeSecs, frames };
}

/** The SRT frame in effect at `tSecs`: the last frame whose start time is before it. */
export function srtFrameIndexAt(frames: SrtFrame[], tSecs: number): number {
  let lo = 0;
  let hi = frames.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (frames[mid]!.startTimeSecs < tSecs) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

export interface SrtOptions {
  /** Percent of the video width and height. */
  positionX: number;
  positionY: number;
  /** Font size relative to a 1080 pixel high video. */
  scale: number;
  showTime: boolean;
  showSbat: boolean;
  showGbat: boolean;
  showSignal: boolean;
  showLatency: boolean;
  showBitrate: boolean;
  showDistance: boolean;
}

export const DEFAULT_SRT_OPTIONS: SrtOptions = {
  positionX: 1.5,
  positionY: 95,
  scale: 35,
  showTime: false,
  showSbat: false,
  showGbat: false,
  showSignal: true,
  showLatency: true,
  showBitrate: true,
  showDistance: true,
};

function pad(n: number, width: number, decimals = 0): string {
  return n.toFixed(decimals).padStart(width, " ");
}

/** The text line for an SRT record, formatted exactly like the native `srt_text`. */
export function srtText(d: SrtFrameData, o: SrtOptions): string {
  let s = "";
  if (o.showTime) s += `Time:${Math.floor(d.flightTime / 60)}:${String(d.flightTime % 60).padStart(2, "0")}  `;
  if (o.showSbat) s += `SBat:${pad(d.skyBat, 4, 1)}V  `;
  if (o.showGbat) s += `GBat:${pad(d.groundBat, 4, 1)}V  `;
  if (o.showSignal) s += `Signal:${d.signal}  `;
  if (o.showLatency) s += `Latency:${pad(d.latency, 3)}ms  `;
  if (o.showBitrate) s += `Bitrate:${pad(d.bitrateMbps, 4, 1)}Mbps  `;
  if (o.showDistance) {
    s += d.distance > 999 ? `Distance:${(d.distance / 1000).toFixed(2)}km` : `Distance:${pad(d.distance, 3)}m`;
  }
  return s;
}
