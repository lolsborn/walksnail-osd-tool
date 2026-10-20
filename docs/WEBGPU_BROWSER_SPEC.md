# Walksnail OSD Tool for the browser: WebGPU + WebCodecs

Status: proposal, 2026-10-20. Companion to PERFORMANCE_NOTES.md, which documents the
native pipeline this design mirrors.

## 1. Goal and constraints

Render a Walksnail Avatar DVR with its OSD and SRT data burned in, entirely inside the
user's browser, with the same output as the native tool.

Hard constraints:

- **Nothing leaves the user's computer.** No uploads, no server-side processing, no
  telemetry. The app is a static site that works with the network disconnected.
- **Full length renders.** A 1080p60 DVR of 4 minutes is 1.4 GB in and 1 GB out;
  neither may be held in memory. Everything streams.
- **Hardware codecs.** Decode and encode must use the platform's media engine, like the
  native tool does through VideoToolbox. Software paths are fallbacks only.
- **Pixel parity** with the native renderer so users can switch between the two.

Non-goals for v1: mobile browsers, editing features beyond what the native app has,
browser extensions.

## 2. Platform APIs

| Need | API | Notes |
|---|---|---|
| Read input files | `<input type=file>` / drag-and-drop, `File.slice()` streaming | Universal. Files are read in chunks; never fully loaded. |
| Write output file | File System Access `showSaveFilePicker()` + `FileSystemWritableFileStream` | Chromium only. Fallback: write to the Origin Private File System, then offer a download. |
| Demux MP4 | mp4box.js (JS) | Browsers expose no demuxer. Walksnail files are plain MP4 with `moov` at the end, so the parser needs random access via `File.slice()`. |
| Decode | WebCodecs `VideoDecoder` | Hardware H.264 and HEVC on Chromium and Safari. Output `VideoFrame` objects stay on the GPU. |
| Composite | WebGPU | Same design and the same WGSL as `backend/src/gpu/overlay.wgsl`. |
| Encode | WebCodecs `VideoEncoder` | H.264 everywhere. HEVC encode is hardware-only and must be feature-detected with `VideoEncoder.isConfigSupported`. |
| Mux | mp4-muxer (JS) | Writes a regular MP4 incrementally to the output stream. Handles `moov` placement with a reserved area so the file is not rewritten at the end. |
| Audio | Demux with mp4box.js, remux unchanged | The native tool currently drops audio; the browser version should pass it through. |
| Parsers | TypeScript ports of `backend/src/osd`, `srt`, `font` | Small and well tested on the Rust side; port with the same test vectors. |

Browser support target: Chromium 120+ on macOS and Windows for v1. Safari 26 has
WebGPU and WebCodecs but no save picker (OPFS + download fallback). Firefox has
WebCodecs and WebGPU on Windows since 2025; macOS WebGPU support should be re-checked
at implementation time.

## 3. Architecture

A single page app in TypeScript. No framework requirement; the UI is small. All heavy
work runs in a dedicated Worker so the page stays responsive.

```
main thread                      render worker
-----------                      -------------
file pickers, settings UI  --->  Demuxer (mp4box.js, File.slice)
preview canvas (WebGPU)            |  encoded samples + timestamps
progress, cancel                   v
                                 VideoDecoder (hardware)
                                   |  VideoFrame (GPU-backed)
                                   v
                                 Compositor (WebGPU): import frame -> overlay OSD + SRT
                                   |  GPUTexture -> VideoFrame
                                   v
                                 VideoEncoder (hardware)
                                   |  EncodedVideoChunk
                                   v
                                 Muxer (mp4-muxer) ---> FileSystemWritableFileStream
                                 Audio samples pass straight from demuxer to muxer
```

### 3.1 Frame flow and backpressure

WebCodecs is asynchronous and queues internally; the native lesson (stage 1 in the
performance notes) applies directly: every stage must be allowed to run ahead by a
bounded number of frames, and nothing may run unbounded.

- Keep at most `N_DECODE = 8` decoded frames outstanding: pause feeding the decoder
  when `decoder.decodeQueueSize + framesHeld >= N_DECODE`.
- Composite as soon as a decoded frame arrives; `VideoFrame.close()` it immediately
  after the GPU copy is submitted.
- Keep at most `N_ENCODE = 8` frames queued in the encoder (`encoder.encodeQueueSize`);
  await `ondequeue` otherwise.
- Muxer writes are awaited per chunk; the writable stream applies its own
  backpressure to disk.

Frame timing: decoder output order equals presentation order; the OSD and SRT
records are selected by `VideoFrame.timestamp` exactly as `FrameCompositor` does
with the frame index / frame rate.

### 3.2 Compositor (WebGPU)

Port of `backend/src/gpu/`:

- **Font atlas**: 16 columns of character cells in one `rgba8unorm` texture, built
  from the font PNG decoded with `createImageBitmap`. Resize to the video's character
  size (`get_character_size`) on a 2D canvas once, as the native code does once with
  `FontFile::resized`.
- **Glyph quads**: one instance per visible glyph (index != 0, not masked), rebuilt only
  when the OSD frame changes. `overlay.wgsl` is used as is.
- **SRT text**: rasterize the line into a small canvas with Canvas 2D (`fillText`)
  using the bundled AzeretMono font, upload as a texture, draw as one quad. Keep the
  same position and scale rules as `srt_layout`. The native tool's over-coverage
  artifact is not reproduced; the GPU path draws the text color.
- **Input**: `device.importExternalTexture({ source: videoFrame })` gives the decoded
  frame as a sampled texture without a copy on Chromium; the browser does the YUV
  to RGB conversion. The frame is drawn with a fullscreen pass into the working
  `rgba8unorm` texture, then the overlay pass runs on top (chroma key: clear instead).
  `convert.wgsl` is not needed; the browser owns the color conversion.
- **Output**: `new VideoFrame(canvas, { timestamp })` from an `OffscreenCanvas` with a
  WebGPU context. On Chromium with a hardware encoder this stays on the GPU. Fallback
  for other browsers: `copyTextureToBuffer` + `VideoFrame` from an `ArrayBuffer`
  (`format: "RGBA"`), which is the stage 2 round trip and costs a few ms per frame.
- **Upscale**: render into a 2560x1440 target instead of a scale filter.
- **Preview**: the same compositor draws into the visible canvas for the frame slider,
  so preview and output are pixel-identical.

Color: WebCodecs frames carry their color space; `importExternalTexture` converts to
the canvas color space. Output frames are tagged by the encoder (`colorSpace` on the
`VideoFrame`). Verify BT.709 tagging end to end on the test clip.

### 3.3 Encoder configuration

```ts
{
  codec: "hvc1.1.6.L153.B0" | "avc1.640033",   // HEVC Main L5.1 or H.264 High 5.1
  width, height,                                 // 1920x1080 or 2560x1440
  framerate: 60,
  bitrate: 40_000_000, bitrateMode: "constant",
  hardwareAcceleration: "prefer-hardware",
  latencyMode: "quality",
  hevc: { format: "hevc" }, avc: { format: "avc" }
}
```

Check `VideoEncoder.isConfigSupported` for HEVC first and fall back to H.264; show
which one is in use. Expect roughly the native ceilings on the same machine (about
318 fps HEVC, 207 fps H.264 at 1080p on an M3 Pro), minus browser overhead; 150 to
250 fps is a realistic target.

### 3.4 Settings parity

Everything in `RenderSettings`, `OsdOptions` and `SrtOptions` has a UI equivalent:
encoder, bitrate, upscale, chroma key and color, OSD position offset, glyph mask
(click on the preview), adjust-playback-speed, SRT position, scale and field toggles.
Settings persist in `localStorage`. The last used font file is remembered as a
`FileSystemFileHandle` in IndexedDB where the API exists.

## 4. Privacy and offline guarantees

- Static hosting only; no backend. Build output is plain HTML/JS/WASM.
- `Content-Security-Policy: default-src 'self'; connect-src 'none'; img-src 'self'
  blob:; worker-src 'self'`. No third-party scripts, fonts or analytics.
- Service worker caches the app so it runs with the network off. First run after an
  update is the only time the network is used.
- The app never calls `fetch` with user data. Output goes only to the file the user
  picked (or the OPFS fallback, which is sandboxed to the origin and cleared by the
  user).
- Document this on the page and in the README, and keep the build reproducible so
  the claim can be audited.

## 5. Project layout

```
web/
  index.html, app.ts            UI, file handling, settings, preview
  worker/render.worker.ts       pipeline orchestration and backpressure
  codec/demux.ts                mp4box.js wrapper, streams samples from File
  codec/decoder.ts, encoder.ts  WebCodecs wrappers with queue limits
  codec/mux.ts                  mp4-muxer wrapper, audio passthrough
  gpu/compositor.ts             WebGPU pipeline, atlas, glyph instances, text strip
  gpu/overlay.wgsl              copied from backend/src/gpu/overlay.wgsl
  parsers/osd.ts, srt.ts, font.ts   ports of the Rust parsers with their test vectors
  test/                         golden-image tests against native renders
```

Tooling: TypeScript, Vite, Vitest. No UI framework needed. Keep the Rust repo as the
source of truth for shaders and parser test vectors; a small script copies the WGSL
and test vectors into `web/`.

## 6. Milestones

1. **Load and preview.** Pick video, OSD, SRT, font; parse; show one composited frame
   on a WebGPU canvas with the frame slider. Golden-image test against a native render
   of the same frame.
2. **Pipeline, H.264.** Demux, hardware decode, composite, hardware encode, mux to a
   file via the save picker. Bounded queues, progress, cancel. Measure fps with the
   benchmark clip.
3. **Audio passthrough and output robustness.** Remux the audio track; verify the file
   plays in QuickTime, VLC and a browser; OPFS fallback for browsers without the save
   picker.
4. **HEVC and settings parity.** HEVC encode where supported, chroma key, upscale,
   masks, all SRT options, settings persistence.
5. **Offline PWA and polish.** Service worker, CSP, install prompt, README.

## 7. Risks

- **HEVC encode support** differs by browser and OS. H.264 is the safe default.
- **Memory**: every `VideoFrame` must be closed promptly or decoding stalls. The queue
  limits above are the guard.
- **Safari**: no save picker, so the output must go through OPFS and a download; test
  memory behavior with 1 GB outputs.
- **Demuxing edge cases**: DVR files with `moov` at the end need the random-access
  reader; corrupted last fragments after a power loss should be handled gracefully.
- **Throughput**: the browser adds copies the native tool avoids. If the GPU-to-encoder
  handoff copies through the CPU on some platform, expect 100 fps rather than 250.

## 8. What is reused from the native tool

- `overlay.wgsl` unchanged; the glyph instance layout and atlas layout.
- The parsers' behavior and unit test vectors.
- The pipeline shape and the backpressure lessons from the performance notes.
- The benchmark clip and the golden frames for parity testing.
