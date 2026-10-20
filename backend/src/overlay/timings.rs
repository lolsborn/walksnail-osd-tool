use std::time::Duration;

/// Accumulated wall-clock time spent in each stage of the render pipeline, as seen from the
/// compositing thread. Used to find the bottleneck when optimizing the renderer.
#[derive(Debug, Default, Clone)]
pub struct RenderTimings {
    pub frames: u64,
    /// Time blocked waiting for the next decoded frame from ffmpeg (decode + pipe read).
    pub decoder_wait: Duration,
    /// Time drawing OSD glyphs onto the frame.
    pub osd_overlay: Duration,
    /// Time drawing SRT text onto the frame.
    pub srt_overlay: Duration,
    /// Time blocked writing the finished frame to the encoder's stdin (encode + pipe write).
    pub encoder_write: Duration,
}

impl RenderTimings {
    pub fn total(&self) -> Duration {
        self.decoder_wait + self.osd_overlay + self.srt_overlay + self.encoder_write
    }

    fn per_frame_ms(&self, d: Duration) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            d.as_secs_f64() * 1000.0 / self.frames as f64
        }
    }

    fn pct(&self, d: Duration) -> f64 {
        let total = self.total().as_secs_f64();
        if total == 0.0 {
            0.0
        } else {
            d.as_secs_f64() / total * 100.0
        }
    }

    pub fn log_summary(&self) {
        let total = self.total();
        let fps = if total.is_zero() {
            0.0
        } else {
            self.frames as f64 / total.as_secs_f64()
        };
        tracing::info!(
            frames = self.frames,
            total_secs = format_args!("{:.2}", total.as_secs_f64()),
            fps = format_args!("{:.1}", fps),
            "Render timing summary"
        );
        for (name, d) in [
            ("decoder wait", self.decoder_wait),
            ("osd overlay", self.osd_overlay),
            ("srt overlay", self.srt_overlay),
            ("encoder write", self.encoder_write),
        ] {
            tracing::info!(
                stage = name,
                ms_per_frame = format_args!("{:.3}", self.per_frame_ms(d)),
                pct = format_args!("{:.1}", self.pct(d)),
                "Render stage timing"
            );
        }
    }
}
