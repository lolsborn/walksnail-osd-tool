use std::{iter::Peekable, time::Instant, vec::IntoIter};

use ffmpeg_sidecar::event::OutputVideoFrame;
use image::{Rgba, RgbaImage};

use super::{overlay_osd, overlay_srt_data, RenderTimings};
use crate::{
    font,
    osd::{self, OsdOptions},
    srt::{self, SrtOptions},
};

/// Draws the OSD and SRT overlays onto decoded video frames, keeping track of which OSD and SRT
/// record applies to the current video timestamp.
pub struct FrameCompositor<'a> {
    osd_frames_iter: Peekable<IntoIter<osd::Frame>>,
    srt_frames_iter: Peekable<IntoIter<srt::SrtFrame>>,
    font_file: font::FontFile,
    osd_options: OsdOptions,
    srt_options: SrtOptions,
    srt_font: rusttype::Font<'a>,
    current_osd_frame: osd::Frame,
    current_srt_frame: Option<srt::SrtFrame>,
    chroma_key: Option<Rgba<u8>>,
    timings: RenderTimings,
}

impl<'a> FrameCompositor<'a> {
    #[tracing::instrument(skip(osd_frames, srt_frames, font_file, srt_font), level = "debug")]
    pub fn new(
        osd_frames: Vec<osd::Frame>,
        srt_frames: Vec<srt::SrtFrame>,
        font_file: font::FontFile,
        srt_font: rusttype::Font<'a>,
        osd_options: &OsdOptions,
        srt_options: &SrtOptions,
        chroma_key: Option<[f32; 3]>,
    ) -> Self {
        let mut osd_frames_iter = osd_frames.into_iter();
        let mut srt_frames_iter = srt_frames.into_iter();
        let first_osd_frame = osd_frames_iter.next().unwrap();
        // The SRT file is optional, so there may be no SRT frames at all
        let first_srt_frame = srt_frames_iter.next();
        let chroma_key =
            chroma_key.map(|c| Rgba([(c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8, 255]));
        Self {
            osd_frames_iter: osd_frames_iter.peekable(),
            srt_frames_iter: srt_frames_iter.peekable(),
            font_file,
            osd_options: osd_options.clone(),
            srt_options: srt_options.clone(),
            srt_font,
            current_osd_frame: first_osd_frame,
            current_srt_frame: first_srt_frame,
            chroma_key,
            timings: RenderTimings::default(),
        }
    }

    /// Time spent so far in each stage, as measured on the thread calling `composite`.
    pub fn timings(&self) -> &RenderTimings {
        &self.timings
    }

    pub fn timings_mut(&mut self) -> &mut RenderTimings {
        &mut self.timings
    }

    /// Draw the overlays for this video frame in place and return it.
    pub fn composite(&mut self, mut video_frame: OutputVideoFrame) -> OutputVideoFrame {
        let start = Instant::now();
        self.timings.frames += 1;

        // For every video frame check if frame time is later than the next OSD frame time.
        // If so advance the iterator over the OSD frames so we use the correct OSD frame
        // for this video frame
        if let Some(next_osd_frame) = self.osd_frames_iter.peek() {
            let next_osd_frame_secs = next_osd_frame.time_millis as f32 / 1000.0;
            if video_frame.timestamp > next_osd_frame_secs * self.osd_options.osd_playback_speed_factor {
                self.current_osd_frame = self.osd_frames_iter.next().unwrap();
            }
        }

        if let Some(next_srt_frame) = self.srt_frames_iter.peek() {
            let next_srt_start_time_secs = next_srt_frame.start_time_secs;
            if video_frame.timestamp > next_srt_start_time_secs {
                self.current_srt_frame = self.srt_frames_iter.next();
            }
        }

        let mut frame_image = if let Some(chroma_key) = self.chroma_key {
            RgbaImage::from_pixel(video_frame.width, video_frame.height, chroma_key)
        } else {
            RgbaImage::from_raw(
                video_frame.width,
                video_frame.height,
                std::mem::take(&mut video_frame.data),
            )
            .unwrap()
        };

        overlay_osd(
            &mut frame_image,
            &self.current_osd_frame,
            &self.font_file,
            &self.osd_options,
        );
        let osd_done = Instant::now();
        self.timings.osd_overlay += osd_done - start;

        if let Some(srt_data) = self.current_srt_frame.as_ref().and_then(|f| f.data.as_ref()) {
            overlay_srt_data(&mut frame_image, srt_data, &self.srt_font, &self.srt_options);
        }
        self.timings.srt_overlay += osd_done.elapsed();

        // Hand the buffer over without copying it
        video_frame.data = frame_image.into_raw();
        video_frame
    }
}
