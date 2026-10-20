//! Headless benchmark mode: render a video with the same pipeline the GUI uses and print timing
//! results, so render performance can be measured without clicking through the UI.
//!
//! Usage:
//!   walksnail-osd-tool --bench --video <path> [--osd <path>] [--font <path>] [--srt <path>]
//!                      [--encoder <name>] [--bitrate <mbps>] [--output <path>]
//!
//! OSD and SRT default to the files next to the video. The font defaults to the one saved in the
//! app config. OSD and SRT options come from the app config so results match GUI renders.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use backend::{
    config::AppConfig,
    ffmpeg::{start_video_render, Encoder, FromFfmpegMessage, ToFfmpegMessage, VideoInfo},
    font::FontFile,
    osd::OsdFile,
    srt::SrtFile,
};

use crate::util::{get_dependency_path, get_output_video_path};

pub fn run(args: &[String]) -> Result<(), String> {
    tracing_subscriber::fmt()
        .compact()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    let video_path = PathBuf::from(get("--video").ok_or("--bench requires --video <path>")?);
    let osd_path = get("--osd")
        .map(PathBuf::from)
        .unwrap_or_else(|| video_path.with_extension("osd"));
    let srt_path = get("--srt")
        .map(PathBuf::from)
        .unwrap_or_else(|| video_path.with_extension("srt"));
    let output_path = get("--output")
        .map(PathBuf::from)
        .unwrap_or_else(|| get_output_video_path(&video_path));
    let encoder_name = get("--encoder").unwrap_or_else(|| "h264_videotoolbox".to_string());

    let config = AppConfig::load_or_create();
    let font_path = get("--font").unwrap_or(config.font_path.clone());
    if font_path.is_empty() {
        return Err("No font file: pass --font <path> or load one in the app first".into());
    }

    let ffmpeg_path = get_dependency_path("ffmpeg");
    let ffprobe_path = get_dependency_path("ffprobe");

    let encoders = Encoder::get_available_encoders(&ffmpeg_path);
    let encoder = encoders
        .iter()
        .find(|e| e.name == encoder_name && e.detected)
        .cloned()
        .ok_or_else(|| {
            let available: Vec<_> = encoders
                .iter()
                .filter(|e| e.detected)
                .map(|e| e.name.as_str())
                .collect();
            format!(
                "Encoder {encoder_name} not available. Detected: {}",
                available.join(", ")
            )
        })?;

    let video_info = VideoInfo::get(&video_path, &ffprobe_path).map_err(|e| format!("Video: {e}"))?;
    let osd_file = OsdFile::open(osd_path.clone()).map_err(|e| format!("OSD: {e}"))?;
    let font_file = FontFile::open(PathBuf::from(&font_path)).map_err(|e| format!("Font: {e}"))?;
    let srt_file = if srt_path.exists() {
        Some(SrtFile::open(srt_path.clone()).map_err(|e| format!("SRT: {e}"))?)
    } else {
        None
    };
    let srt_font: rusttype::Font<'static> =
        rusttype::Font::try_from_bytes(include_bytes!("../../resources/fonts/AzeretMono-Regular.ttf")).unwrap();

    let mut render_settings = config.render_options.clone();
    render_settings.encoder = encoder;
    if let Some(bitrate) = get("--bitrate") {
        render_settings.bitrate_mbps = bitrate.parse().map_err(|_| "--bitrate must be a number")?;
    }

    let mut osd_options = config.osd_options.clone();
    osd_options.osd_playback_speed_factor = if osd_options.adjust_playback_speed {
        video_info.duration.as_secs_f32() / osd_file.duration.as_secs_f32()
    } else {
        1.0
    };

    println!(
        "BENCH start video={} ({}x{} @ {} fps, {} frames) encoder={} bitrate={}M srt={} output={}",
        file_name(&video_path),
        video_info.width,
        video_info.height,
        video_info.frame_rate,
        video_info.total_frames,
        render_settings.encoder.name,
        render_settings.bitrate_mbps,
        srt_file.is_some(),
        output_path.display(),
    );

    let start = Instant::now();
    let (to_ffmpeg, from_ffmpeg) = start_video_render(
        &ffmpeg_path,
        &video_path,
        &output_path,
        osd_file.frames.clone(),
        srt_file.map(|s| s.frames).unwrap_or_default(),
        font_file,
        srt_font,
        &osd_options,
        &config.srt_options,
        &video_info,
        &render_settings,
    )
    .map_err(|e| format!("Failed to start render: {e}"))?;

    let mut last_progress = Instant::now();
    let mut frames_done = 0u32;
    let result = loop {
        match from_ffmpeg.recv() {
            Ok(FromFfmpegMessage::Progress(p)) => {
                frames_done = p.frame;
                if last_progress.elapsed() > Duration::from_secs(10) {
                    println!(
                        "BENCH progress frame={} fps={:.1} speed={:.2}x",
                        p.frame, p.fps, p.speed
                    );
                    last_progress = Instant::now();
                }
            }
            Ok(FromFfmpegMessage::DecoderFinished) => {}
            Ok(FromFfmpegMessage::EncoderFinished) => {
                to_ffmpeg.send(ToFfmpegMessage::AbortRender).ok();
                break Ok(());
            }
            Ok(FromFfmpegMessage::DecoderFatalError(e)) | Ok(FromFfmpegMessage::EncoderFatalError(e)) => {
                to_ffmpeg.send(ToFfmpegMessage::AbortRender).ok();
                break Err(format!("ffmpeg fatal error: {e}"));
            }
            Err(_) => break Err("Render pipeline stopped unexpectedly".into()),
        }
    };
    let wall = start.elapsed();

    // Give the pipeline threads a moment to log their timing summaries
    std::thread::sleep(Duration::from_millis(300));

    let frames = video_info.total_frames.max(frames_done);
    println!(
        "BENCH done encoder={} frames={} wall_secs={:.2} fps={:.1} speed={:.2}x",
        render_settings.encoder.name,
        frames,
        wall.as_secs_f64(),
        frames as f64 / wall.as_secs_f64(),
        video_info.duration.as_secs_f64() / wall.as_secs_f64(),
    );
    result
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default()
}
