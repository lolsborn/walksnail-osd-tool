// Milestone 1: load the four files, show a composited preview at any point in the video.
// The preview decodes with a plain <video> element so it works in every browser; the render
// pipeline (WebCodecs) comes in milestone 2.

import { DEFAULT_OSD_OPTIONS, drawOsd, drawSrt, type OsdOptions } from "./compositor/canvas2d";
import { characterSizeForVideoHeight, loadFontFile, resizeFont, type FontFile } from "./parsers/font";
import { osdFrameIndexAt, parseOsdFile, type OsdFile } from "./parsers/osd";
import { DEFAULT_SRT_OPTIONS, parseSrtFile, srtFrameIndexAt, type SrtFile, type SrtOptions } from "./parsers/srt";

function el<T extends HTMLElement>(id: string): T {
  const e = document.getElementById(id);
  if (!e) throw new Error(`Missing element #${id}`);
  return e as T;
}

const video = el<HTMLVideoElement>("video");
const canvas = el<HTMLCanvasElement>("preview");
const scrub = el<HTMLInputElement>("scrub");
const scrubTime = el<HTMLSpanElement>("scrub-time");
const info = el<HTMLDivElement>("file-info");

interface State {
  videoFile?: File;
  videoDuration?: number;
  osd?: OsdFile;
  srt?: SrtFile;
  font?: FontFile;
  /** Font resized for the current video height. */
  sizedFont?: FontFile;
  osdOptions: OsdOptions;
  srtOptions: SrtOptions;
}

const state: State = {
  osdOptions: { ...DEFAULT_OSD_OPTIONS, masked: new Set() },
  srtOptions: { ...DEFAULT_SRT_OPTIONS },
};

function formatTime(secs: number): string {
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return `${m}:${String(s).padStart(2, "0")}`;
}

function updateInfo(): void {
  const lines: string[] = [];
  if (state.videoFile && state.videoDuration !== undefined) {
    lines.push(`Video: ${state.videoFile.name}, ${video.videoWidth}x${video.videoHeight}, ${formatTime(state.videoDuration)}`);
  }
  if (state.osd) {
    lines.push(`OSD: ${state.osd.fcFirmware}, ${state.osd.frameCount} frames, ${formatTime(state.osd.durationSecs)}`);
  }
  if (state.srt) {
    lines.push(`SRT: ${state.srt.frames.length} entries, ${formatTime(state.srt.durationSecs)}`);
  }
  if (state.font) {
    const d = state.font.dimensions;
    lines.push(`Font: ${d.characterCount} characters, ${d.type}, native size ${state.font.size}`);
  }
  info.textContent = lines.join("\n");
}

function playbackSpeedFactor(): number {
  if (!state.osd || state.videoDuration === undefined || !state.osdOptions.adjustPlaybackSpeed) return 1;
  return state.videoDuration / state.osd.durationSecs;
}

async function ensureSizedFont(): Promise<FontFile | undefined> {
  if (!state.font || !video.videoHeight) return undefined;
  const wanted = characterSizeForVideoHeight(video.videoHeight);
  if (!state.sizedFont || state.sizedFont.size !== wanted) {
    state.sizedFont = await resizeFont(state.font, wanted);
  }
  return state.sizedFont;
}

let drawPending = false;
async function drawPreview(): Promise<void> {
  if (drawPending) return;
  drawPending = true;
  try {
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const width = video.videoWidth || 1920;
    const height = video.videoHeight || 1080;
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width;
      canvas.height = height;
    }
    ctx.clearRect(0, 0, width, height);
    if (state.videoFile) ctx.drawImage(video, 0, 0, width, height);

    const t = video.currentTime;
    const font = await ensureSizedFont();
    if (state.osd && font) {
      const frame = state.osd.frames[osdFrameIndexAt(state.osd.frames, t, playbackSpeedFactor())];
      if (frame) drawOsd(ctx, frame, font, state.osdOptions);
    }
    if (state.srt) {
      const data = state.srt.frames[srtFrameIndexAt(state.srt.frames, t)]?.data;
      if (data) drawSrt(ctx, data, state.srtOptions, width, height);
    }
    scrubTime.textContent = formatTime(t);
  } finally {
    drawPending = false;
  }
}

// --- File loading ---

el<HTMLInputElement>("video-file").addEventListener("change", (e) => {
  const file = (e.target as HTMLInputElement).files?.[0];
  if (!file) return;
  state.videoFile = file;
  if (video.src) URL.revokeObjectURL(video.src);
  video.src = URL.createObjectURL(file);
});

video.addEventListener("loadedmetadata", () => {
  state.videoDuration = video.duration;
  scrub.max = String(Math.floor(video.duration * 10));
  scrub.value = "0";
  state.sizedFont = undefined;
  updateInfo();
  video.currentTime = 0;
});

video.addEventListener("seeked", () => void drawPreview());

scrub.addEventListener("input", () => {
  if (!state.videoFile) return;
  video.currentTime = Number(scrub.value) / 10;
});

el<HTMLInputElement>("osd-file").addEventListener("change", async (e) => {
  const file = (e.target as HTMLInputElement).files?.[0];
  if (!file) return;
  try {
    state.osd = parseOsdFile(new Uint8Array(await file.arrayBuffer()));
  } catch (err) {
    alert(`Could not read OSD file: ${(err as Error).message}`);
    return;
  }
  updateInfo();
  void drawPreview();
});

el<HTMLInputElement>("srt-file").addEventListener("change", async (e) => {
  const file = (e.target as HTMLInputElement).files?.[0];
  if (!file) return;
  try {
    state.srt = parseSrtFile(await file.text());
  } catch (err) {
    alert(`Could not read SRT file: ${(err as Error).message}`);
    return;
  }
  updateInfo();
  void drawPreview();
});

el<HTMLInputElement>("font-file").addEventListener("change", async (e) => {
  const file = (e.target as HTMLInputElement).files?.[0];
  if (!file) return;
  try {
    state.font = await loadFontFile(file);
    state.sizedFont = undefined;
  } catch (err) {
    alert(`Could not read font file: ${(err as Error).message}`);
    return;
  }
  updateInfo();
  void drawPreview();
});

// --- Settings ---

function bindNumber(id: string, apply: (v: number) => void): void {
  const input = el<HTMLInputElement>(id);
  input.addEventListener("input", () => {
    apply(Number(input.value));
    void drawPreview();
  });
}

function bindCheckbox(id: string, apply: (v: boolean) => void): void {
  const input = el<HTMLInputElement>(id);
  input.addEventListener("change", () => {
    apply(input.checked);
    void drawPreview();
  });
}

bindNumber("osd-x", (v) => (state.osdOptions.offsetX = v));
bindNumber("osd-y", (v) => (state.osdOptions.offsetY = v));
bindCheckbox("osd-adjust-speed", (v) => (state.osdOptions.adjustPlaybackSpeed = v));
bindNumber("srt-x", (v) => (state.srtOptions.positionX = v));
bindNumber("srt-y", (v) => (state.srtOptions.positionY = v));
bindNumber("srt-scale", (v) => (state.srtOptions.scale = v));
bindCheckbox("srt-time", (v) => (state.srtOptions.showTime = v));
bindCheckbox("srt-sbat", (v) => (state.srtOptions.showSbat = v));
bindCheckbox("srt-gbat", (v) => (state.srtOptions.showGbat = v));
bindCheckbox("srt-signal", (v) => (state.srtOptions.showSignal = v));
bindCheckbox("srt-latency", (v) => (state.srtOptions.showLatency = v));
bindCheckbox("srt-bitrate", (v) => (state.srtOptions.showBitrate = v));
bindCheckbox("srt-distance", (v) => (state.srtOptions.showDistance = v));

// Make sure the SRT font is loaded before the first text draw
void document.fonts.load(`35px AzeretMono`).then(() => void drawPreview());
