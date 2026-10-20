// The render pipeline: demux -> VideoDecoder -> Canvas 2D composite -> VideoEncoder -> mux.
//
// Every stage is bounded so a long video streams through a few frames at a time, and the
// browser's hardware codecs are used when it has them.

import { Muxer } from "mp4-muxer";
import type { Sample } from "mp4box";

import { drawOsd, drawSrt, type OsdOptions } from "../compositor/canvas2d";
import type { FontFile } from "../parsers/font";
import { osdFrameIndexAt, type OsdFile } from "../parsers/osd";
import { srtFrameIndexAt, type SrtFile, type SrtOptions } from "../parsers/srt";
import { Demuxer } from "./demux";
import type { OutputSink } from "./output";

export interface RenderInputs {
  video: File;
  osd: OsdFile;
  srt?: SrtFile;
  /** Already resized to the video's character size. */
  font: FontFile;
  osdOptions: OsdOptions;
  srtOptions: SrtOptions;
  sink: OutputSink;
}

export interface RenderProgress {
  framesDone: number;
  frameCount: number;
  fps: number;
  elapsedSecs: number;
}

export interface RenderSettings {
  /** Output bitrate in bits per second. */
  bitrate: number;
}

export const DEFAULT_RENDER_SETTINGS: RenderSettings = { bitrate: 40_000_000 };

/** Frames allowed between the decoder and the encoder. */
const MAX_FRAMES_IN_FLIGHT = 8;

export function webCodecsAvailable(): boolean {
  return typeof VideoDecoder !== "undefined" && typeof VideoEncoder !== "undefined";
}

/** H.264 High profile at the level needed for the given size and frame rate. */
function h264Codec(width: number, height: number, frameRate: number): string {
  const macroblocks = Math.ceil(width / 16) * Math.ceil(height / 16);
  const rate = macroblocks * frameRate;
  // Level 4.2 covers 1080p60; 5.1 covers 1440p60 and 4K30; 5.2 covers 4K60
  const level = rate <= 522_240 ? 0x2a : rate <= 983_040 ? 0x33 : 0x34;
  return `avc1.6400${level.toString(16).padStart(2, "0")}`;
}

async function pickEncoderConfig(width: number, height: number, frameRate: number, bitrate: number): Promise<VideoEncoderConfig> {
  const base: VideoEncoderConfig = {
    codec: h264Codec(width, height, frameRate),
    width,
    height,
    framerate: frameRate,
    bitrate,
    bitrateMode: "constant",
    latencyMode: "quality",
    avc: { format: "avc" },
  };
  for (const hardwareAcceleration of ["prefer-hardware", "no-preference"] as const) {
    const config: VideoEncoderConfig = { ...base, hardwareAcceleration };
    const support = await VideoEncoder.isConfigSupported(config);
    if (support.supported) return config;
  }
  throw new Error("This browser cannot encode H.264 video at this size");
}

class Cancelled extends Error {
  constructor() {
    super("Render cancelled");
  }
}

export class Renderer {
  private cancelled = false;

  cancel(): void {
    this.cancelled = true;
  }

  async run(
    inputs: RenderInputs,
    settings: RenderSettings,
    onProgress: (p: RenderProgress) => void,
    onStage: (stage: string) => void = () => undefined,
  ): Promise<void> {
    onStage("Reading the video index...");
    const demuxer = await Demuxer.open(inputs.video);
    const { video } = demuxer.media;

    onStage(`Checking codecs (${video.codec}, ${video.width}x${video.height})...`);
    const decoderSupport = await VideoDecoder.isConfigSupported({ codec: video.codec, description: video.description });
    if (!decoderSupport.supported) throw new Error(`This browser cannot decode ${video.codec} video`);
    const encoderConfig = await pickEncoderConfig(video.width, video.height, video.frameRate, settings.bitrate);
    onStage(`Rendering with ${encoderConfig.codec} (${encoderConfig.hardwareAcceleration})...`);

    const canvas = new OffscreenCanvas(video.width, video.height);
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("Canvas 2D is not available");

    const muxer = new Muxer({
      target: inputs.sink.target,
      video: { codec: "avc", width: video.width, height: video.height, frameRate: video.frameRate },
      // The index goes at the end so the output streams to disk; "in-memory" would hold the
      // whole file in RAM until the end.
      fastStart: false,
      firstTimestampBehavior: "offset",
    });

    const start = performance.now();
    let framesDone = 0;
    let failure: Error | undefined;
    const fail = (err: Error) => {
      failure ??= err;
      this.cancelled = true;
    };

    const encoder = new VideoEncoder({
      output: (chunk, meta) => {
        try {
          muxer.addVideoChunk(chunk, meta);
        } catch (err) {
          fail(err as Error);
        }
        framesDone++;
        if (framesDone % 15 === 0) {
          const elapsedSecs = (performance.now() - start) / 1000;
          onProgress({ framesDone, frameCount: video.frameCount, fps: framesDone / elapsedSecs, elapsedSecs });
        }
      },
      error: (err) => fail(err),
    });
    encoder.configure(encoderConfig);

    // Decoded frames wait here until the compositor takes them, so that the number of frames
    // alive at once stays bounded regardless of how the decoder batches its output.
    const decoded: VideoFrame[] = [];
    let wake: (() => void) | undefined;
    const decoder = new VideoDecoder({
      output: (frame) => {
        decoded.push(frame);
        wake?.();
      },
      error: (err) => fail(err),
    });
    decoder.configure({
      codec: video.codec,
      description: video.description,
      hardwareAcceleration: "prefer-hardware",
      optimizeForLatency: false,
    });

    const playbackSpeedFactor = inputs.osdOptions.adjustPlaybackSpeed
      ? video.durationSecs / inputs.osd.durationSecs
      : 1;
    const srtFrames = inputs.srt?.frames;

    const compositeOne = (frame: VideoFrame): void => {
      const tSecs = (frame.timestamp ?? 0) / 1_000_000;
      ctx.drawImage(frame, 0, 0, video.width, video.height);
      const osdFrame = inputs.osd.frames[osdFrameIndexAt(inputs.osd.frames, tSecs, playbackSpeedFactor)];
      if (osdFrame) drawOsd(ctx, osdFrame, inputs.font, inputs.osdOptions);
      if (srtFrames) {
        const data = srtFrames[srtFrameIndexAt(srtFrames, tSecs)]?.data;
        if (data) drawSrt(ctx, data, inputs.srtOptions, video.width, video.height);
      }
      const out = new VideoFrame(canvas, { timestamp: frame.timestamp, duration: frame.duration ?? undefined });
      frame.close();
      encoder.encode(out, { keyFrame: framesSubmitted % 120 === 0 });
      out.close();
      framesSubmitted++;
    };
    let framesSubmitted = 0;

    // Wake the drain loop when the decoder takes a chunk as well as when it emits a frame:
    // hardware decoders consume several chunks before the first frame comes out.
    decoder.addEventListener("dequeue", () => wake?.());
    const waitForDecoder = () =>
      new Promise<void>((resolve) => {
        wake = () => {
          wake = undefined;
          resolve();
        };
      });

    /** Composite everything decoded so far, then wait until the pipeline has room again. */
    const drain = async (): Promise<void> => {
      while (!this.cancelled) {
        while (decoded.length > 0) {
          compositeOne(decoded.shift()!);
          if (encoder.encodeQueueSize > MAX_FRAMES_IN_FLIGHT) {
            await new Promise<void>((resolve) => encoder.addEventListener("dequeue", () => resolve(), { once: true }));
          }
        }
        if (decoder.decodeQueueSize < MAX_FRAMES_IN_FLIGHT) return;
        await waitForDecoder();
      }
    };

    const onSamples = async (_trackId: number, samples: Sample[]): Promise<void> => {
      for (const sample of samples) {
        if (this.cancelled) return;
        if (!sample.data) continue;
        decoder.decode(
          new EncodedVideoChunk({
            type: sample.is_sync ? "key" : "delta",
            timestamp: (sample.cts * 1_000_000) / sample.timescale,
            duration: (sample.duration * 1_000_000) / sample.timescale,
            data: sample.data,
          }),
        );
        await drain();
      }
    };

    try {
      await demuxer.samples([video.id], onSamples, () => this.cancelled);
      if (this.cancelled) throw failure ?? new Cancelled();
      await decoder.flush();
      // Flushing hands the remaining frames to the output callback; composite them too
      while (decoded.length > 0) compositeOne(decoded.shift()!);
      await encoder.flush();
      if (failure) throw failure;
      muxer.finalize();
      await inputs.sink.finish();
      const elapsedSecs = (performance.now() - start) / 1000;
      onProgress({ framesDone, frameCount: video.frameCount, fps: framesDone / elapsedSecs, elapsedSecs });
    } catch (err) {
      await inputs.sink.abort().catch(() => undefined);
      throw err;
    } finally {
      for (const frame of decoded) frame.close();
      decoded.length = 0;
      if (decoder.state !== "closed") decoder.close();
      if (encoder.state !== "closed") encoder.close();
    }
  }
}
