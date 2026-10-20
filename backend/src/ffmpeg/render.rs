use std::{
    io::{self, ErrorKind, Read, Write},
    path::PathBuf,
    thread,
    time::Instant,
};

use crossbeam_channel::{Receiver, Sender};
use ffmpeg_sidecar::{
    child::FfmpegChild,
    command::FfmpegCommand,
    event::{FfmpegEvent, LogLevel},
};

use super::{
    render_settings::RenderSettings, Encoder, FromFfmpegMessage, GpuMode, PixelFormat, ToFfmpegMessage, VideoFrame,
    VideoInfo,
};
use crate::{
    font,
    gpu::GpuCompositor,
    osd::{self, OsdOptions},
    overlay::{get_character_size, FrameCompositor, RenderTimings},
    srt::{self, SrtOptions},
};

/// Number of frames that may be buffered between each pair of pipeline stages. Each frame is
/// width * height * 4 bytes (8 MB at 1080p, 33 MB at 4K), so keep this small.
const FRAME_BUFFER_LEN: usize = 8;

#[tracing::instrument(skip(osd_frames, srt_frames, font_file), err)]
pub fn start_video_render(
    ffmpeg_path: &PathBuf,
    input_video: &PathBuf,
    output_video: &PathBuf,
    osd_frames: Vec<osd::Frame>,
    srt_frames: Vec<srt::SrtFrame>,
    font_file: font::FontFile,
    srt_font: rusttype::Font<'static>,
    osd_options: &OsdOptions,
    srt_options: &SrtOptions,
    video_info: &VideoInfo,
    render_settings: &RenderSettings,
) -> Result<(Sender<ToFfmpegMessage>, Receiver<FromFfmpegMessage>), io::Error> {
    // Resize the font to the video's character size once, instead of on every glyph of every frame
    let font_file = font_file.resized(&get_character_size(video_info.height));

    // Decide where to composite and which frame format to exchange with ffmpeg. If the GPU is not
    // available fall back to CPU compositing on RGBA frames.
    let requested_format = match render_settings.gpu_mode {
        GpuMode::Off | GpuMode::Rgba => PixelFormat::Rgba,
        GpuMode::Nv12 => PixelFormat::Nv12,
    };
    let gpu = if render_settings.gpu_mode != GpuMode::Off {
        match GpuCompositor::new(video_info.width, video_info.height, &font_file, requested_format) {
            Ok(gpu) => {
                tracing::info!(
                    "Compositing on the GPU ({}) with {:?} frames",
                    gpu.adapter_name(),
                    requested_format
                );
                Some(gpu)
            }
            Err(e) => {
                tracing::warn!("GPU compositing unavailable, falling back to the CPU: {e}");
                None
            }
        }
    } else {
        None
    };
    let pixel_format = if gpu.is_some() {
        requested_format
    } else {
        PixelFormat::Rgba
    };

    let mut decoder_process = spawn_decoder(ffmpeg_path, input_video, pixel_format)?;

    let mut encoder_process = spawn_encoder(
        ffmpeg_path,
        video_info.width,
        video_info.height,
        video_info.frame_rate,
        render_settings.bitrate_mbps,
        &render_settings.encoder,
        output_video,
        render_settings.upscale,
        pixel_format,
    )?;

    // Channels to communicate with ffmpeg handler threads
    let (from_ffmpeg_tx, from_ffmpeg_rx) = crossbeam_channel::unbounded();
    let (to_ffmpeg_tx, to_ffmpeg_rx) = crossbeam_channel::unbounded();

    let mut compositor = FrameCompositor::new(
        osd_frames,
        srt_frames,
        font_file,
        srt_font,
        osd_options,
        srt_options,
        if render_settings.use_chroma_key {
            Some(render_settings.chroma_key)
        } else {
            None
        },
        gpu,
    );

    // The render is a three stage pipeline: decoder reader -> compositor -> encoder writer.
    // The stages are decoupled by bounded channels so that the two ffmpeg processes and the
    // compositing work overlap instead of running in lockstep (OS pipe buffers are far smaller
    // than one video frame, so without this every stage waits on the others).
    let (decoded_tx, decoded_rx) = crossbeam_channel::bounded::<VideoFrame>(FRAME_BUFFER_LEN);
    let (composited_tx, composited_rx) = crossbeam_channel::bounded::<VideoFrame>(FRAME_BUFFER_LEN);
    // Frame buffers travel back from the writer to the reader to be reused, so that the steady
    // state allocates nothing.
    let (recycle_tx, recycle_rx) = crossbeam_channel::bounded::<Vec<u8>>(FRAME_BUFFER_LEN * 3);

    // Raw frames are read straight from the decoder's stdout; its stderr (progress and errors)
    // still goes through the sidecar's event iterator on its own thread.
    let decoder_stdout = decoder_process
        .take_stdout()
        .expect("Failed to take the decoder's stdout");
    let decoder_events = decoder_process
        .iter()
        .expect("Failed to create `FfmpegIterator` for decoder");
    let decoder_events_tx = from_ffmpeg_tx.clone();
    thread::Builder::new()
        .name("Decoder events".into())
        .spawn(move || {
            tracing::info_span!("Decoder events thread").in_scope(|| {
                for event in decoder_events {
                    handle_decoder_events(event, &decoder_events_tx);
                }
            });
        })
        .expect("Failed to spawn decoder events thread");

    let (width, height, frame_rate) = (video_info.width, video_info.height, video_info.frame_rate);
    let frame_bytes = pixel_format.frame_bytes(width, height);
    thread::Builder::new()
        .name("Decoder reader".into())
        .spawn(move || {
            tracing::info_span!("Decoder reader thread").in_scope(|| {
                let mut decoder_process = decoder_process;
                let mut decoder_stdout = decoder_stdout;
                let mut timings = RenderTimings::default();
                let mut frame_index = 0u32;
                loop {
                    // Between frames check if the render should be stopped
                    while let Ok(ToFfmpegMessage::AbortRender) = to_ffmpeg_rx.try_recv() {
                        decoder_process
                            .quit()
                            .map_err(|e| tracing::warn!("Failed to send quit to decoder: {e}"))
                            .ok();
                    }

                    let mut data = recycle_rx.try_recv().unwrap_or_default();
                    data.resize(frame_bytes, 0);
                    let wait_start = Instant::now();
                    match decoder_stdout.read_exact(&mut data) {
                        Ok(()) => {}
                        Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                        Err(e) => {
                            tracing::warn!("Failed to read frame from decoder: {e}");
                            break;
                        }
                    }
                    timings.decoder_wait += wait_start.elapsed();
                    timings.frames += 1;

                    let frame = VideoFrame {
                        width,
                        height,
                        timestamp: frame_index as f32 / frame_rate,
                        data,
                    };
                    frame_index += 1;

                    let send_start = Instant::now();
                    if decoded_tx.send(frame).is_err() {
                        // Compositor is gone, stop reading
                        break;
                    }
                    timings.output_wait += send_start.elapsed();
                }
                timings.log_summary("decoder reader");
            });
        })
        .expect("Failed to spawn decoder reader thread");

    thread::Builder::new()
        .name("Compositor".into())
        .spawn(move || {
            tracing::info_span!("Compositor thread").in_scope(|| {
                loop {
                    let wait_start = Instant::now();
                    let Ok(frame) = decoded_rx.recv() else { break };
                    compositor.timings_mut().decoder_wait += wait_start.elapsed();

                    let frame = compositor.composite(frame);

                    let send_start = Instant::now();
                    if composited_tx.send(frame).is_err() {
                        break;
                    }
                    compositor.timings_mut().output_wait += send_start.elapsed();
                }
                compositor.timings().log_summary("compositor");
            });
        })
        .expect("Failed to spawn compositor thread");

    // Feed composited frames to the encoder's stdin. Dropping the stdin handle when the channel
    // closes signals EOF to the encoder.
    let mut encoder_stdin = encoder_process.take_stdin().expect("Failed to get `stdin` for encoder");
    thread::Builder::new()
        .name("Encoder writer".into())
        .spawn(move || {
            tracing::info_span!("Encoder writer thread").in_scope(|| {
                let mut timings = RenderTimings::default();
                loop {
                    let wait_start = Instant::now();
                    let Ok(frame) = composited_rx.recv() else { break };
                    timings.decoder_wait += wait_start.elapsed();
                    timings.frames += 1;

                    let write_start = Instant::now();
                    if encoder_stdin.write_all(&frame.data).is_err() {
                        tracing::warn!("Failed to write frame to encoder, stopping");
                        break;
                    }
                    timings.encoder_write += write_start.elapsed();
                    recycle_tx.try_send(frame.data).ok();
                }
                timings.log_summary("encoder writer");
            });
        })
        .expect("Failed to spawn encoder writer thread");

    // On yet another thread run the encoder to completion
    thread::Builder::new()
        .name("Encoder handler".into())
        .spawn(move || {
            tracing::info_span!("Encoder handler thread").in_scope(|| {
                encoder_process
                    .iter()
                    .expect("Failed to create encoder iterator")
                    .for_each(|event| handle_encoder_events(event, &from_ffmpeg_tx));
            });
        })
        .expect("Failed to spawn encoder handler thread");

    Ok((to_ffmpeg_tx, from_ffmpeg_rx))
}

#[tracing::instrument(skip(ffmpeg_path))]
pub fn spawn_decoder(
    ffmpeg_path: &PathBuf,
    input_video: &PathBuf,
    pixel_format: PixelFormat,
) -> Result<FfmpegChild, io::Error> {
    let decoder = FfmpegCommand::new_with_path(ffmpeg_path)
        .create_no_window()
        .input(input_video.to_str().unwrap())
        .args(["-f", "rawvideo", "-pix_fmt", pixel_format.ffmpeg_name(), "-"])
        .spawn()?;
    Ok(decoder)
}

#[tracing::instrument(skip(ffmpeg_path))]
pub fn spawn_encoder(
    ffmpeg_path: &PathBuf,
    width: u32,
    height: u32,
    frame_rate: f32,
    bitrate_mbps: u32,
    video_encoder: &Encoder,
    output_video: &PathBuf,
    upscale: bool,
    input_pixel_format: PixelFormat,
) -> Result<FfmpegChild, io::Error> {
    let mut encoder_command = FfmpegCommand::new_with_path(ffmpeg_path);

    encoder_command
        .create_no_window()
        .format("rawvideo")
        .pix_fmt(input_pixel_format.ffmpeg_name())
        .size(width, height)
        .rate(frame_rate)
        .input("-");

    if upscale {
        encoder_command.args(["-vf", "scale=2560x1440:flags=bicubic"]);
    }

    // RGBA input needs a conversion to a format the encoders accept. NV12 is accepted directly by
    // the hardware encoders and by libx264/libx265, so no conversion is requested.
    if input_pixel_format == PixelFormat::Rgba {
        encoder_command.pix_fmt("yuv420p");
    }

    encoder_command
        .codec_video(&video_encoder.name)
        .args(["-b:v", &format!("{}M", bitrate_mbps)])
        .args(&video_encoder.extra_args)
        .overwrite()
        .output(output_video.to_str().unwrap());

    let encoder = encoder_command.spawn()?;
    Ok(encoder)
}

fn handle_encoder_events(ffmpeg_event: FfmpegEvent, ffmpeg_sender: &Sender<FromFfmpegMessage>) {
    match ffmpeg_event {
        FfmpegEvent::Log(level, e) => {
            if level == LogLevel::Fatal
            // there are some fatal errors that ffmpeg considers normal errors
            || e.contains("Error initializing output stream")
            || e.contains("[error] Cannot load")
            {
                tracing::error!("ffmpeg fatal error: {}", &e);
                ffmpeg_sender.send(FromFfmpegMessage::EncoderFatalError(e)).unwrap();
            }
        }
        FfmpegEvent::LogEOF => {
            tracing::info!("ffmpeg encoder EOF reached");
            ffmpeg_sender.send(FromFfmpegMessage::EncoderFinished).unwrap();
        }
        _ => {}
    }
}

pub fn handle_decoder_events(ffmpeg_event: FfmpegEvent, ffmpeg_sender: &Sender<FromFfmpegMessage>) {
    match ffmpeg_event {
        FfmpegEvent::Progress(p) => {
            ffmpeg_sender.send(FromFfmpegMessage::Progress(p)).unwrap();
        }
        FfmpegEvent::Done | FfmpegEvent::LogEOF => {
            ffmpeg_sender.send(FromFfmpegMessage::DecoderFinished).unwrap();
        }
        FfmpegEvent::Log(LogLevel::Fatal, e) => {
            tracing::error!("ffmpeg fatal error: {}", &e);
            ffmpeg_sender.send(FromFfmpegMessage::DecoderFatalError(e)).unwrap();
        }
        FfmpegEvent::Log(LogLevel::Warning | LogLevel::Error, e) => {
            tracing::warn!("ffmpeg log: {}", e);
        }
        _ => {}
    }
}
