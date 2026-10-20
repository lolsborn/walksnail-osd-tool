import { existsSync, readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { Demuxer } from "../src/codec/demux";

// A real Walksnail DVR clip, when available on this machine (not checked in: 62 MB).
const CLIP =
  process.env.WALKSNAIL_TEST_CLIP ??
  "/private/tmp/claude-501/-Users-steven-Downloads-walksnail-osd-tool/16172fdb-2e97-488f-9891-46ea806a3279/scratchpad/clip/clip10.mp4";

describe.skipIf(!existsSync(CLIP))("demuxer on a real DVR clip", () => {
  it("reads the index and streams every video sample", async () => {
    const file = new File([readFileSync(CLIP)], "clip10.mp4", { type: "video/mp4" });
    const demuxer = await Demuxer.open(file);
    const { video, audio, durationSecs } = demuxer.media;

    expect(video.width).toBe(1920);
    expect(video.height).toBe(1080);
    expect(video.codec).toMatch(/^(avc1|hvc1|hev1)\./);
    expect(video.frameRate).toBeCloseTo(60, 0);
    expect(video.description.byteLength).toBeGreaterThan(0);
    expect(durationSecs).toBeGreaterThan(9);
    expect(durationSecs).toBeLessThan(11);

    let count = 0;
    let keyFrames = 0;
    let lastTimestamp = -1;
    await demuxer.samples(
      [video.id],
      async (trackId, samples) => {
        expect(trackId).toBe(video.id);
        for (const s of samples) {
          expect(s.data?.byteLength).toBeGreaterThan(0);
          // Decode order: timestamps need not increase (B-frames) but must stay in range
          expect(s.cts / s.timescale).toBeLessThan(durationSecs + 1);
          lastTimestamp = Math.max(lastTimestamp, s.cts / s.timescale);
          if (s.is_sync) keyFrames++;
          count++;
        }
      },
      () => false,
    );
    expect(count).toBe(video.frameCount);
    expect(keyFrames).toBeGreaterThan(0);
    expect(lastTimestamp).toBeGreaterThan(9);
    if (audio) expect(audio.codec).toMatch(/^mp4a/);
  }, 60_000);
});
