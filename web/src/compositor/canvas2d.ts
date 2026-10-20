// Canvas 2D compositor: draws the OSD glyphs and the SRT text line onto a frame.
//
// This is the compatibility baseline; it works in every browser with no WebGPU requirement. A
// WebGPU compositor using the native tool's shaders can replace it later for speed.

import { CHARACTER_SIZES, type FontFile } from "../parsers/font";
import type { OsdFrame } from "../parsers/osd";
import { srtText, type SrtFrameData, type SrtOptions } from "../parsers/srt";

export interface OsdOptions {
  /** Pixel offset applied to every glyph. */
  offsetX: number;
  offsetY: number;
  /** Stretch the OSD timeline to the video length (the native "adjust playback speed"). */
  adjustPlaybackSpeed: boolean;
  /** Grid positions that must not be drawn, as "x,y" strings. */
  masked: Set<string>;
}

export const DEFAULT_OSD_OPTIONS: OsdOptions = {
  offsetX: 0,
  offsetY: 0,
  adjustPlaybackSpeed: true,
  masked: new Set(),
};

/** Same color as the native tool's SRT text. */
export const SRT_TEXT_COLOR = "rgb(240, 240, 240)";
export const SRT_FONT_FAMILY = "AzeretMono";

export function drawOsd(ctx: CanvasRenderingContext2D, frame: OsdFrame, font: FontFile, options: OsdOptions): void {
  const { width, height } = CHARACTER_SIZES[font.size];
  for (const glyph of frame.glyphs) {
    if (options.masked.size > 0 && options.masked.has(`${glyph.x},${glyph.y}`)) continue;
    const image = font.characters[glyph.index];
    if (!image) continue;
    ctx.drawImage(image, glyph.x * width + options.offsetX, glyph.y * height + options.offsetY);
  }
}

/** Position and font size of the SRT text for a frame of the given size, like the native `srt_layout`. */
export function srtLayout(options: SrtOptions, width: number, height: number): { x: number; y: number; scale: number } {
  return {
    x: Math.trunc((options.positionX / 100) * width),
    y: Math.trunc((options.positionY / 100) * height),
    scale: (options.scale / 1080) * height,
  };
}

export function drawSrt(
  ctx: CanvasRenderingContext2D,
  data: SrtFrameData,
  options: SrtOptions,
  width: number,
  height: number,
): void {
  const text = srtText(data, options);
  if (text.length === 0) return;
  const { x, y, scale } = srtLayout(options, width, height);
  ctx.font = `${scale}px ${SRT_FONT_FAMILY}, monospace`;
  // The native renderer puts the baseline one ascent below the anchor
  ctx.textBaseline = "alphabetic";
  const ascent = ctx.measureText(text).fontBoundingBoxAscent ?? scale * 0.8;
  ctx.fillStyle = SRT_TEXT_COLOR;
  ctx.fillText(text, x, y + ascent);
}
