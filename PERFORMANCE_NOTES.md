# Render performance notes

Machine: Apple Silicon Mac (arm64), macOS 25.5. App built in release mode from
the `optional-srt` branch with the `macos-app-bundle` feature and bundled ffmpeg 6.

## Baseline (2026-10-19)

| Run      | fps  | speed  | Notes                                   |
|----------|------|--------|-----------------------------------------|
| Baseline | 65.0 | 1.090x | Unmodified pipeline, software decode    |

Fill in for the baseline: input resolution/framerate, encoder selected
(libx264 / libx265 / h264_videotoolbox / hevc_videotoolbox), bitrate, upscale on/off.
Re-use the same input file and settings for every run so numbers are comparable.

## Pipeline today

1. Decoder: ffmpeg process, software decode, outputs raw RGBA frames over a pipe.
2. Compositing: single Rust thread (`backend/src/overlay/iter.rs`) reads a frame,
   draws OSD glyphs and SRT text on the CPU, clones the buffer, writes to encoder stdin.
3. Encoder: ffmpeg process. GPU only if a VideoToolbox encoder is selected.

Decoder and encoder already run concurrently with the compositing thread.
Compositing is the serial stage and the expected bottleneck.

## How to measure

- Use the same input video, font, OSD/SRT files and render settings every time.
- The fps/speed shown in the app come from ffmpeg's encoder progress.
- Since stage 0 the compositing thread logs a per-stage breakdown at the end of
  every render (also on abort). Read it from the log file:
  `~/Library/Application Support/walksnail-osd-tool/walksnail-osd-tool.log`
  Look for `Render timing summary` and the four `Render stage timing` lines
  (decoder wait, osd overlay, srt overlay, encoder write) with ms per frame and
  percent of the compositing thread's time. The log is reset on each app start.

## Direction: full wgpu render pipeline, built incrementally

Goal: decode -> composite -> encode with frames staying on the GPU. Each stage
below is a separate branch/PR, keeps the app working end to end, and gets a row
in the results table. wgpu 0.20.1 is already in the lock file via eframe, so the
backend crate pins that same version to avoid a second copy.

### Stage 0: measure and fix the obvious CPU waste (no GPU yet)

- Add per-stage timing (decode read, composite, encoder write) behind a tracing
  span so each later stage has a breakdown, not just the final fps.
- `FontFile::get_character` resizes every glyph with Lanczos3 on every frame when
  the font size does not match the video height. Cache the resized character set
  once per (font, size). Likely the single biggest CPU cost today.
- Drop the full-frame `to_vec()` copy at the end of `FrameOverlayIter::next`.
- Result: a clean CPU baseline so GPU gains are not confused with these fixes.

### Stage 1: composite on the GPU, keep the CPU round trip

- New module `backend/src/gpu/`: headless wgpu instance + device (Metal on macOS),
  no window or surface needed.
- Font atlas: pack the font's characters (at the video's character size) into one
  RGBA texture at load time. One atlas per (font, size).
- OSD pass: each glyph is an instanced quad (grid x, grid y, atlas index). Masked
  positions are filtered on the CPU before building the instance buffer. Alpha
  blend onto the frame texture. Position offset and chroma key are shader uniforms
  / clear color.
- SRT pass: keep rusttype rasterization on the CPU but cache one small RGBA strip
  per unique SRT record and draw it as a single textured quad. GPU text
  rasterization is not worth it here.
- Per frame: upload RGBA frame -> texture, run both passes, copy to a staging
  buffer, map and read back, write to encoder stdin. Apple Silicon unified memory
  keeps the copy cost modest.
- Add a small golden-image test: CPU overlay vs GPU overlay on one frame must
  match within a tolerance. This guards every later stage.
- Expected: compositing cost drops a lot, but upload/readback latency caps the gain
  until stage 2.

### Stage 2: keep several frames in flight

- Ring of N staging buffers and textures (N = 3 or 4). Submit frame k while frame
  k-1 is executing and frame k-2 is being mapped for readback.
- Use `map_async` callbacks + `device.poll` on a dedicated thread; keep output
  ordered by sequence number before writing to the encoder.
- Bound memory: N * frame size (4K RGBA = 33 MB per frame).
- Expected: this is where most of the throughput appears.

### Stage 3: cut pipe bandwidth and move color conversion to the GPU

- Decoder outputs `nv12` or `yuv420p` instead of `rgba` (1.5 bytes/px vs 4), and
  the shader does YUV -> RGB when sampling the frame.
- Composite pass writes NV12/yuv420p planes (RGB -> YUV in a compute shader) so the
  encoder pipe is also 1.5 bytes/px and ffmpeg skips swscale on both sides.
- Add `-hwaccel videotoolbox` to the decoder. With raw output over a pipe this only
  helps the decode step, but it is free to add here.
- Upscale: render to a 2560x1440 target instead of ffmpeg `-vf scale`.
- Expected: large win at 4K where pipe/copy bandwidth dominates.

### Stage 4: zero-copy on macOS (frames never leave the GPU)

- Replace the ffmpeg decoder process with in-process VideoToolbox decode
  (`objc2-video-toolbox` / `objc2-core-video`), producing CVPixelBuffers backed by
  IOSurface. Import them into wgpu through the Metal backend (`wgpu-hal` texture
  from an `MTLTexture` created from the IOSurface).
- Replace the ffmpeg encoder process with in-process VideoToolbox encode fed from
  a CVPixelBuffer that wraps the composited texture; mux with ffmpeg or in-process.
- Windows/Linux keep the stage 3 pipe path. This stage is macOS-only by design.
- Expected: render speed limited by the media engine, not by memory copies.

### Side benefit

Once stage 1 exists, the egui preview can switch eframe to its wgpu backend and
render the preview with the same shader, so preview and output are pixel-identical.

### Results

| Stage | Change                               | fps | speed | Notes |
|-------|--------------------------------------|-----|-------|-------|
| 0     | Timing + glyph cache + no copy       |     |       |       |
| 1     | GPU composite, CPU round trip        |     |       |       |
| 2     | Frames in flight                     |     |       |       |
| 3     | YUV pipes + hwaccel decode           |     |       |       |
| 4     | Zero-copy VideoToolbox (macOS)       |     |       |       |

## Known ceilings

- Raw RGBA frames cross ffmpeg -> Rust -> ffmpeg through pipes. 1080p frame ~8 MB,
  4K frame ~33 MB. At 4K60 that is ~2 GB/s each direction.
- ffmpeg converts YUV -> RGBA on decode and RGBA -> YUV on encode on the CPU.
- Going fully GPU (composite in a shader or via an ffmpeg overlay filter) avoids the
  round trip but is a redesign. Not planned yet.
