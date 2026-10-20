// Milestone 1: pick the flight files, choose a provided OSD font, preview any point in the video.
// The preview decodes with a plain <video> element so it works in every browser; the render
// pipeline (WebCodecs) comes in milestone 2.

import { canPickSaveLocation, openDownloadSink, openFileSink } from "./codec/output";
import { DEFAULT_RENDER_SETTINGS, Renderer, webCodecsAvailable } from "./codec/render";
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
const fontSelect = el<HTMLSelectElement>("font-select");
const srtOptionsPanel = el<HTMLFieldSetElement>("srt-options");

interface ProvidedFont {
  id: string;
  name: string;
  file: string;
}

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
    lines.push(`${state.videoFile.name}: ${video.videoWidth}x${video.videoHeight}, ${formatTime(state.videoDuration)}`);
  }
  if (state.videoFile && !state.osd) lines.push("No .osd file selected yet.");
  info.textContent = lines.join("\n");
  srtOptionsPanel.hidden = !state.srt;
  updateRenderButton();
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

// --- Provided OSD fonts ---

async function loadProvidedFont(file: string): Promise<FontFile> {
  // Load through an <img> so the page needs no fetch permissions beyond its own images
  const image = new Image();
  image.src = `${import.meta.env.BASE_URL}osd-fonts/${file}`;
  await image.decode();
  return loadFontFile(image);
}

async function selectFont(font: ProvidedFont): Promise<void> {
  try {
    state.font = await loadProvidedFont(font.file);
    state.sizedFont = undefined;
  } catch (err) {
    alert(`Could not load the ${font.name} font: ${(err as Error).message}`);
    return;
  }
  void drawPreview();
}

async function initFonts(): Promise<void> {
  const response = await fetch(`${import.meta.env.BASE_URL}osd-fonts/fonts.json`);
  const fonts = (await response.json()) as ProvidedFont[];
  fontSelect.replaceChildren(
    ...fonts.map((f) => {
      const option = document.createElement("option");
      option.value = f.id;
      option.textContent = f.name;
      return option;
    }),
  );
  fontSelect.addEventListener("change", () => {
    const font = fonts.find((f) => f.id === fontSelect.value);
    if (font) void selectFont(font);
  });
  if (fonts[0]) await selectFont(fonts[0]);
}

// --- Flight files ---

async function addFlightFiles(files: Iterable<File>): Promise<void> {
  for (const file of files) {
    const ext = file.name.toLowerCase().split(".").pop();
    try {
      if (ext === "mp4" || file.type === "video/mp4") {
        state.videoFile = file;
        if (video.src) URL.revokeObjectURL(video.src);
        video.src = URL.createObjectURL(file);
      } else if (ext === "osd") {
        state.osd = parseOsdFile(new Uint8Array(await file.arrayBuffer()));
      } else if (ext === "srt") {
        state.srt = parseSrtFile(await file.text());
      }
    } catch (err) {
      alert(`Could not read ${file.name}: ${(err as Error).message}`);
    }
  }
  updateInfo();
  void drawPreview();
}

el<HTMLInputElement>("flight-files").addEventListener("change", (e) => {
  const files = (e.target as HTMLInputElement).files;
  if (files) void addFlightFiles(files);
});

document.body.addEventListener("dragover", (e) => e.preventDefault());
document.body.addEventListener("drop", (e) => {
  e.preventDefault();
  if (e.dataTransfer?.files) void addFlightFiles(e.dataTransfer.files);
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

// --- Advanced settings ---

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

// --- Rendering ---

const renderButton = el<HTMLButtonElement>("render");
const cancelButton = el<HTMLButtonElement>("cancel");
const progressBar = el<HTMLProgressElement>("progress");
const renderStatus = el<HTMLDivElement>("render-status");
let renderer: Renderer | undefined;

function updateRenderButton(): void {
  const ready = Boolean(state.videoFile && state.osd && state.font) && !renderer;
  renderButton.disabled = !ready;
  if (!webCodecsAvailable()) {
    renderButton.disabled = true;
    renderStatus.textContent = "This browser cannot render: it lacks WebCodecs. The preview still works.";
  }
}

async function startRender(): Promise<void> {
  if (!state.videoFile || !state.osd) return;
  const font = await ensureSizedFont();
  if (!font) return;
  const outputName = state.videoFile.name.replace(/\.mp4$/i, "") + "_with_osd.mp4";
  const sink = canPickSaveLocation() ? await openFileSink(outputName) : openDownloadSink(outputName);
  if (!sink) return; // cancelled the file picker

  renderer = new Renderer();
  updateRenderButton();
  cancelButton.hidden = false;
  progressBar.hidden = false;
  progressBar.value = 0;
  renderStatus.textContent = "Starting...";
  try {
    await renderer.run(
      {
        video: state.videoFile,
        osd: state.osd,
        srt: state.srt,
        font,
        osdOptions: state.osdOptions,
        srtOptions: state.srtOptions,
        sink,
      },
      DEFAULT_RENDER_SETTINGS,
      (p) => {
        progressBar.value = p.frameCount ? p.framesDone / p.frameCount : 0;
        const remaining = p.fps > 0 ? (p.frameCount - p.framesDone) / p.fps : 0;
        renderStatus.textContent = `${p.framesDone} / ${p.frameCount} frames, ${p.fps.toFixed(0)} fps, ${formatTime(remaining)} left`;
      },
      (stage) => {
        renderStatus.textContent = stage;
      },
    );
    renderStatus.textContent = `Done: ${outputName}`;
    progressBar.value = 1;
  } catch (err) {
    renderStatus.textContent = (err as Error).message;
  } finally {
    renderer = undefined;
    cancelButton.hidden = true;
    updateRenderButton();
  }
}

renderButton.addEventListener("click", () => void startRender());
cancelButton.addEventListener("click", () => renderer?.cancel());

void Promise.all([initFonts(), document.fonts.load("35px AzeretMono")]).then(() => {
  updateRenderButton();
  void drawPreview();
});
