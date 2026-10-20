# Render performance notes

Machine: Apple Silicon Mac (arm64), macOS 25.5. App built in release mode from
the `optional-srt` branch with the `macos-app-bundle` feature and bundled ffmpeg 6.

## Baseline (2026-10-19)

| Run      | fps  | speed  | Notes                                   |
|----------|------|--------|-----------------------------------------|
| Baseline | 65.0 | 1.090x | Unmodified pipeline, software decode    |

Test input and settings (use these for every run):

- Input: AvatarS0098.mp4, 1920x1080 @ 60 fps, 217 s, 13035 frames, ~51 Mbps
- Encoder: libx264 (software), upscale off, chroma key off
- OSD font at native 1080p size (so the glyph cache mostly removes clones, not resizes)
- SRT: signal/latency/bitrate/distance shown, time/battery hidden

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
  `~/Library/Application Support/rs.walksnail-osd-tool/walksnail-osd-tool.log`
  Look for `Render timing summary` and the four `Render stage timing` lines
  (decoder wait, osd overlay, srt overlay, encoder write) with ms per frame and
  percent of the compositing thread's time. The log is reset on each app start.

## ffmpeg micro-benchmarks (2026-10-19, bundled ffmpeg 6, 1080p, 2000 frames)

Script: scratchpad `ffbench.sh`. Encode tests feed all-zero frames through a pipe,
so encoder cost is a lower bound; conversion and pipe costs are representative.

| Test                              | fps   | ms/frame |
|-----------------------------------|-------|----------|
| pipe rgba passthrough (no work)   | 416   | 2.4      |
| swscale rgba -> yuv420p only      | 362   | 2.8      |
| enc rgba -> h264_videotoolbox     | 215   | 4.7      |
| enc nv12 -> h264_videotoolbox     | 213   | 4.7      |
| enc rgba -> libx264 (zeros)       | 326   | 3.1      |
| enc nv12 -> libx264 (zeros)       | 703   | 1.4      |
| dec sw -> rgba (real clip)        | 654   | 1.5      |
| dec sw -> nv12 (real clip)        | 726   | 1.4      |
| dec -hwaccel videotoolbox -> nv12 | 170   | 5.9      |
| dec -hwaccel videotoolbox -> rgba | 198   | 5.1      |

Conclusions:
- Reading or writing one 8 MB RGBA frame over a pipe costs ~2.4 ms by itself.
- ffmpeg's h264_videotoolbox path tops out ~213 fps at 1080p regardless of input
  pixel format; conversion overlaps with encoding inside ffmpeg.
- Software decode is fast (650+ fps, multi-threaded). `-hwaccel videotoolbox`
  through ffmpeg is 3-4x SLOWER when the output must come back over a pipe.
  Dropped from the plan; hardware decode only pays off in-process (zero-copy stage).
- The app's 8.4 ms/frame is roughly read (2.2) + composite (0.7) + write (5.5)
  added together. macOS pipe buffers are 64 KB, far less than one frame, so the
  decoder, the compositing thread and the encoder run in lockstep instead of
  overlapping. Decoupling them with threads and a few frames of buffering should
  bring per-frame time toward max(stage) ~ 5 ms (~190 fps) with no GPU work.

### Decode -> encode chains with real frames (2000 frames, 1080p)

| Test                                          | fps   |
|-----------------------------------------------|-------|
| dec -> rgba over pipe -> h264_videotoolbox    | 206   |
| dec -> nv12 over pipe -> h264_videotoolbox    | 208   |
| dec -> rgba over pipe -> hevc_videotoolbox    | 195   |
| dec -> nv12 over pipe -> hevc_videotoolbox    | 196   |
| dec -> h264_videotoolbox in ONE ffmpeg, no pipe | 207 |
| hwaccel dec -> h264_videotoolbox, one ffmpeg  | 203   |

ffmpeg's VideoToolbox encoder path caps at ~207 fps at 1080p regardless of pipe,
pixel format or decoder. That is the ceiling of any design that encodes through
ffmpeg. Stage 1 (155.5) is at 75% of it. NV12 pipes (old stage 3) would not help
at 1080p; they only matter at 4K where bytes per frame are 4x larger. Beating 207
needs in-process VideoToolbox encoding (stage 4), for which the GPU compositor is
the prerequisite.


Goal: decode -> composite -> encode with frames staying on the GPU. Each stage
below is a separate branch/PR, keeps the app working end to end, and gets a row
in the results table. wgpu 0.20.1 is already in the lock file via eframe, so the
backend crate pins that same version to avoid a second copy.

### VideoToolbox encoder flags (nv12 chain, real frames, 2000 frames, 1080p)

| Encoder / flags                         | fps   |
|-----------------------------------------|-------|
| h264_videotoolbox default               | 207   |
| h264_videotoolbox -prio_speed 1         | 207   |
| h264_videotoolbox -realtime 1           | 158   |
| h264_videotoolbox no B-frames           | 207   |
| h264_videotoolbox baseline profile      | 207   |
| hevc_videotoolbox default               | 196   |
| hevc_videotoolbox -prio_speed 1         | 321   |

h264 is capped by the encoder path itself. hevc with `-prio_speed 1` is 1.6x faster
than anything else through ffmpeg, so it is now added to the hevc_videotoolbox
encoder's extra args and is the encoder to measure with from here on.

## Automated benchmark protocol (from 2026-10-19)

`walksnail-osd-tool --bench --video <mp4> --encoder <name> --output <local path>`
(see ui/src/bench.rs). Scratchpad `bench.sh <label> <encoder...>` runs it and appends
to `results.log`. Input and output are on the local SSD: reading from the SMB share
and writing the 1 GB output back to it cost ~50 fps (160 -> 210 with h264).

## Direction: full wgpu render pipeline, built incrementally

Goal: decode -> composite -> encode with frames staying on the GPU. Each stage
below is a separate branch/PR, keeps the app working end to end, and gets a row
in the results table. wgpu 0.20.1 is already in the lock file via eframe, so the
backend crate pins that same version to avoid a second copy.

Stage order was revised after stage 0's measurements: pipelined I/O moved ahead
of the GPU compositor because the overlay was <10% of frame time.

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

Stage 2 outcome (2026-10-19): `backend/src/gpu/` with a headless wgpu context, font
atlas (16 columns), instanced glyph quads, CPU-rasterized SRT strip, `over` blending,
synchronous readback. `RenderSettings::use_gpu` toggles it (UI checkbox, `--gpu` in
bench). Three parity tests vs the CPU path (OSD, chroma key, SRT text) pass within
rounding. Found a CPU-path artifact: imageproc does not clamp rusttype coverage, so
overlapping glyph contours render white instead of the text color on the CPU.

### Stage 2b: keep several frames in flight

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
| 0     | Timing + glyph cache + no copy, libx264 | 78.0 | 1.310x | UI figure, run aborted at 6514 frames. Thread: 54.6 fps |
| 0     | Same build, h264_videotoolbox        | 77-99 | 1.290x | UI figure. Full run: 13035 frames in 109.1 s = 119.5 fps on the compositing thread |

Stage 0 breakdown (ms per frame on the compositing thread, share of its time):

| Stage          | libx264 (aborted) | h264_videotoolbox (full) |
|----------------|-------------------|--------------------------|
| decoder wait   | 0.84  (4.6%)      | 2.19 (26.1%)             |
| osd overlay    | 2.08 (11.4%)      | 0.71  (8.5%)             |
| srt overlay    | 0.00              | 0.00 (no .srt for this clip) |
| encoder write  | 15.38 (84.0%)     | 5.47 (65.4%)             |

Takeaways:
- Swapping to the hardware encoder barely changed the UI fps. The "encoder write"
  cost is dominated by pushing 8 MB RGBA frames through the pipe and ffmpeg's
  software RGBA -> YUV conversion (log warns "No accelerated colorspace conversion
  found"), not by encoding itself.
- CPU compositing is ~0.7 ms/frame, ~8% of the thread. GPU compositing with a
  synchronous round trip (stage 1 as planned) cannot speed this up on its own.
- The win is in the pipes and color conversion (stage 3). Stage 1 stays as the
  infrastructure step but its success criterion is pixel parity and no regression,
  not fps.
- Measurement protocol from here: h264_videotoolbox, let the render complete, and
  use the summary's frames / total_secs rather than the UI's live number, which
  fluctuates (77-99 observed for a 119.5 fps average).
| 1     | Pipelined I/O (3 threads, bounded channels), h264_videotoolbox | 155.5 | 2.59x | Full run: 13035 frames in 83.9 s. +30% over stage 0 (119.5) |

Stage 1 breakdown (h264_videotoolbox, full run, ms per frame):

| Thread         | waits on input | work            | waits on output | encoder write |
|----------------|----------------|-----------------|-----------------|---------------|
| decoder reader | 2.74 (decoder) | -               | 3.69            | -             |
| compositor     | 1.86           | 0.67 (osd)      | 3.90            | -             |
| encoder writer | 0.72           | -               | -               | 5.72          |

The encoder writer is the bottleneck: 5.7 ms/frame blocked writing RGBA into the
encoder ffmpeg (pipe write + swscale rgba->yuv420p + VideoToolbox encode on that
side). Decoder and compositor are mostly idle. Next lever is what crosses that
pipe: fewer bytes (NV12 is 2.7x smaller than RGBA) and no swscale on the encoder
side, i.e. stage 3's format work, or removing the pipe entirely (stage 4).

| 1b    | Frame buffers 4 -> 8, h264_videotoolbox | 160.6 | 2.68x | Longer clip (18060 frames, 112.5 s). Writer idle 11% -> 3.6%, write 6.0 ms/frame |
| 1c    | Same build, headless bench, local SSD in/out, h264_videotoolbox | 209.7 | 3.50x | 62.2 s. At the ffmpeg h264 ceiling: writer 99.9% blocked on encoder |
| 1c    | Same build, headless bench, local SSD in/out, hevc_videotoolbox -prio_speed | 268.8 | 4.48x | 48.5 s. Bottleneck flipped: reader waits 3.7 ms/frame on the decoder pipe |
| 2     | GPU composite (wgpu/Metal), CPU round trip, hevc_videotoolbox | 258.4 | 4.31x | 50.5 s. Composite 2.9 ms/frame (CPU was 0.65) but hidden behind the decoder (3.8 ms). Parity tests pass |
| 2     | Same, h264_videotoolbox              | 209.5 | 3.49x | Unchanged, at the ffmpeg h264 ceiling |
| 2b    | GPU frames in flight                 |     |       |       |
| 3     | YUV pipes + hwaccel decode           |     |       |       |
| 4     | Zero-copy VideoToolbox (macOS)       |     |       |       |

## Known ceilings

- Raw RGBA frames cross ffmpeg -> Rust -> ffmpeg through pipes. 1080p frame ~8 MB,
  4K frame ~33 MB. At 4K60 that is ~2 GB/s each direction.
- ffmpeg converts YUV -> RGBA on decode and RGBA -> YUV on encode on the CPU.
- Going fully GPU (composite in a shader or via an ffmpeg overlay filter) avoids the
  round trip but is a redesign. Not planned yet.
