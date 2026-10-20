import { describe, expect, it } from "vitest";

import { DEFAULT_SRT_OPTIONS, parseSrtFile, parseSrtFrameData, srtFrameIndexAt, srtText } from "../src/parsers/srt";

// Test vectors from backend/src/srt/frame.rs
describe("SRT frame data", () => {
  it("parses the pre 31.36.8 format", () => {
    expect(parseSrtFrameData("Signal:4 CH:8 FlightTime:0 SBat:4.7V GBat:7.2V Delay:32ms Bitrate:25Mbps Distance:7m")).toEqual({
      signal: 4,
      channel: 8,
      flightTime: 0,
      skyBat: 4.7,
      groundBat: 7.2,
      latency: 32,
      bitrateMbps: 25,
      distance: 7,
    });
  });

  it("parses the 32.37.10 format", () => {
    expect(
      parseSrtFrameData("Signal:4 CH:7 FlightTime:0 SBat:16.7V GBat:12.5V Delay:25ms Bitrate:25.0Mbps Distance:1m"),
    ).toEqual({
      signal: 4,
      channel: 7,
      flightTime: 0,
      skyBat: 16.7,
      groundBat: 12.5,
      latency: 25,
      bitrateMbps: 25,
      distance: 1,
    });
  });

  it("does not parse the debug format as telemetry", () => {
    const line =
      "CH:1 MCS:4 SP[ 45 152  47 149] GP[ 49  48  45  47] GTP:27 GTP0:00 STP:24 STP0:00 GSNR:25.9 SSNR:17.8 Gtemp:50 Stemp:82 Delay:31ms Frame:60  Gerr:0 SErr:0 42, [iso:0,mode=max, exp:0] [gain:0.00 exp:0.000ms]";
    expect(parseSrtFrameData(line)).toBeUndefined();
    const file = parseSrtFile(`1\n00:00:00,000 --> 00:00:01,000\n${line}\n`);
    expect(file.hasDebug).toBe(true);
    expect(file.frames[0]!.data).toBeUndefined();
  });
});

describe("SRT file", () => {
  const text = [
    "1",
    "00:00:00,000 --> 00:00:01,000",
    "Signal:4 CH:7 FlightTime:0 SBat:16.7V GBat:12.5V Delay:25ms Bitrate:25.0Mbps Distance:0m",
    "",
    "2",
    "00:00:01,000 --> 00:00:02,000",
    "Signal:3 CH:7 FlightTime:1 SBat:16.6V GBat:12.5V Delay:28ms Bitrate:25.0Mbps Distance:1234m",
    "",
  ].join("\r\n");

  it("parses entries, times and flags", () => {
    const file = parseSrtFile(text);
    expect(file.frames).toHaveLength(2);
    expect(file.frames[1]!.startTimeSecs).toBe(1);
    expect(file.frames[1]!.endTimeSecs).toBe(2);
    expect(file.durationSecs).toBe(2);
    expect(file.hasDistance).toBe(true);
    expect(file.hasDebug).toBe(false);
  });

  it("selects the entry in effect at a time", () => {
    const { frames } = parseSrtFile(text);
    expect(srtFrameIndexAt(frames, 0)).toBe(0);
    expect(srtFrameIndexAt(frames, 0.9)).toBe(0);
    expect(srtFrameIndexAt(frames, 1.5)).toBe(1);
  });

  it("formats the text line like the native tool", () => {
    const { frames } = parseSrtFile(text);
    const d = frames[1]!.data!;
    expect(srtText(d, DEFAULT_SRT_OPTIONS)).toBe("Signal:3  Latency: 28ms  Bitrate:25.0Mbps  Distance:1.23km");
    expect(srtText(frames[0]!.data!, { ...DEFAULT_SRT_OPTIONS, showTime: true, showSbat: true, showGbat: true })).toBe(
      "Time:0:00  SBat:16.7V  GBat:12.5V  Signal:4  Latency: 25ms  Bitrate:25.0Mbps  Distance:  0m",
    );
  });

  it("rejects files with no entries", () => {
    expect(() => parseSrtFile("hello")).toThrow();
  });
});
