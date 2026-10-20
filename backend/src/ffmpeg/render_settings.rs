use serde::{Deserialize, Serialize};

use crate::ffmpeg::{Codec, Encoder};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RenderSettings {
    pub encoder: Encoder,
    pub selected_encoder_idx: usize,
    pub show_undetected_encoders: bool,
    pub bitrate_mbps: u32,
    pub upscale: bool,
    pub use_chroma_key: bool,
    pub chroma_key: [f32; 3],
    /// Draw the overlays on the GPU (wgpu) instead of the CPU.
    #[serde(default)]
    pub use_gpu: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        // Hardware H.264 on macOS, software elsewhere. On startup the app re-selects the encoder by
        // name from the list of detected encoders and falls back to the first one if it's missing.
        #[cfg(target_os = "macos")]
        let encoder = Encoder {
            name: "h264_videotoolbox".to_string(),
            codec: Codec::H264,
            hardware: true,
            detected: false,
            extra_args: Vec::new(),
        };
        #[cfg(not(target_os = "macos"))]
        let encoder = Encoder {
            name: "libx264".to_string(),
            codec: Codec::H264,
            hardware: false,
            detected: false,
            extra_args: Vec::new(),
        };
        Self {
            encoder,
            selected_encoder_idx: 0,
            show_undetected_encoders: false,
            bitrate_mbps: 40,
            upscale: false,
            use_chroma_key: false,
            chroma_key: [1.0 / 255.0, 177.0 / 255.0, 64.0 / 255.0],
            use_gpu: false,
        }
    }
}
