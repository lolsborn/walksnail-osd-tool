/// One raw video frame exchanged between the decoder, the compositor and the encoder.
#[derive(Debug)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// Presentation time in seconds, derived from the frame index and the video frame rate.
    pub timestamp: f32,
    /// Pixel data in the pipeline's `PixelFormat`.
    pub data: Vec<u8>,
}
