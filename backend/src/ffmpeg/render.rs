use std::{
    io::{self, Write},
    path::PathBuf,
    thread,
    time::Instant,
};

use crossbeam_channel::{Receiver, Sender};
use ffmpeg_sidecar::{
    child::FfmpegChild,
    command::FfmpegCommand,
    event::{FfmpegEvent, LogLevel, OutputVideoFrame},
};

use super::{render_settings::RenderSettings, Encoder, FromFfmpegMessage, ToFfmpegMessage, VideoInfo};
use crate::{
    font,
    osd::{self, OsdOptions},
    overlay::{get_character_size, FrameCompositor, RenderTimings},
    srt::{self, SrtOptions},
};

/// Number of frames that may be buffered between each pair of pipeline stages. Each frame is
/// width * height * 4 bytes (8 MB at 1080p, 33 MB at 4K), so keep this small.
const FRAME_BUFFER_LEN: usize = 4;

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
    let mut decoder_process = spawn_decoder(ffmpeg_path, input_video)?;

    let mut encoder_process = spawn_encoder(
        ffmpeg_path,
        video_info.width,
        video_info.height,
        video_info.frame_rate,
        render_settings.bitrate_mbps,
        &render_settings.encoder,
        output_video,
        render_settings.upscale,
    )?;

    // Channels to communicate with ffmpeg handler threads
    let (from_ffmpeg_tx, from_ffmpeg_rx) = crossbeam_channel::unbounded();
    let (to_ffmpeg_tx, to_ffmpeg_rx) = crossbeam_channel::unbounded();

    // Resize the font to the video's character size once, instead of on every glyph of every frame
    let font_file = font_file.resized(&get_character_size(video_info.height));

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
    );

    // The render is a three stage pipeline: decoder reader -> compositor -> encoder writer.
    // The stages are decoupled by bounded channels so that the two ffmpeg processes and the
    // compositing work overlap instead of running in lockstep (OS pipe buffers are far smaller
    // than one video frame, so without this every stage waits on the others).
    let (decoded_tx, decoded_rx) = crossbeam_channel::bounded::<OutputVideoFrame>(FRAME_BUFFER_LEN);
    let (composited_tx, composited_rx) = crossbeam_channel::bounded::<OutputVideoFrame>(FRAME_BUFFER_LEN);

    let decoder_iter = decoder_process
        .iter()
        .expect("Failed to create `FfmpegIterator` for decoder");
    let decoder_events_tx = from_ffmpeg_tx.clone();
    thread::Builder::new()
        .name("Decoder reader".into())
        .spawn(move || {
            tracing::info_span!("Decoder reader thread").in_scope(|| {
                let mut timings = RenderTimings::default();
                let mut wait_start = Instant::now();
                for event in decoder_iter {
                    // On every event check if the render should be stopped
                    while let Ok(ToFfmpegMessage::AbortRender) = to_ffmpeg_rx.try_recv() {
                        decoder_process
                            .quit()
                            .map_err(|e| tracing::warn!("Failed to send quit to decoder: {e}"))
                            .ok();
                    }
                    match event {
                        FfmpegEvent::OutputFrame(frame) => {
                            timings.decoder_wait += wait_start.elapsed();
                            timings.frames += 1;
                            let send_start = Instant::now();
                            if decoded_tx.send(frame).is_err() {
                                // Compositor is gone, stop reading
                                break;
                            }
                            timings.output_wait += send_start.elapsed();
                            wait_start = Instant::now();
                        }
                        other_event => handle_decoder_events(other_event, &decoder_events_tx),
                    }
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
pub fn spawn_decoder(ffmpeg_path: &PathBuf, input_video: &PathBuf) -> Result<FfmpegChild, io::Error> {
    let decoder = FfmpegCommand::new_with_path(ffmpeg_path)
        .create_no_window()
        .input(input_video.to_str().unwrap())
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
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
) -> Result<FfmpegChild, io::Error> {
    let mut encoder_command = FfmpegCommand::new_with_path(ffmpeg_path);

    encoder_command
        .create_no_window()
        .format("rawvideo")
        .pix_fmt("rgba")
        .size(width, height)
        .rate(frame_rate)
        .input("-");

    if upscale {
        encoder_command.args(["-vf", "scale=2560x1440:flags=bicubic"]);
    }

    encoder_command
        .pix_fmt("yuv420p")
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
