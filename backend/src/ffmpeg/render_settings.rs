use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::ffmpeg::{Codec, Encoder};

/// Raw pixel format of the frames exchanged with the ffmpeg decoder and encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    /// 4 bytes per pixel, what the CPU compositor works on.
    Rgba,
    /// Planar Y followed by interleaved half-resolution UV, 1.5 bytes per pixel. 2.7x fewer bytes
    /// over the pipes than RGBA and no color conversion inside ffmpeg.
    Nv12,
}

impl PixelFormat {
    pub fn ffmpeg_name(&self) -> &'static str {
        match self {
            PixelFormat::Rgba => "rgba",
            PixelFormat::Nv12 => "nv12",
        }
    }

    pub fn frame_bytes(&self, width: u32, height: u32) -> usize {
        match self {
            PixelFormat::Rgba => (width * height * 4) as usize,
            PixelFormat::Nv12 => (width * height * 3 / 2) as usize,
        }
    }
}

/// Where and how the overlays are composited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GpuMode {
    /// CPU compositing on RGBA frames.
    #[default]
    Off,
    /// GPU compositing, RGBA frames to and from ffmpeg.
    Rgba,
    /// GPU compositing, NV12 frames to and from ffmpeg; color conversion happens in the shader.
    Nv12,
}

impl Display for GpuMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuMode::Off => write!(f, "CPU"),
            GpuMode::Rgba => write!(f, "GPU (RGBA frames)"),
            GpuMode::Nv12 => write!(f, "GPU (NV12 frames)"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RenderSettings {
    pub encoder: Encoder,
    pub selected_encoder_idx: usize,
    pub show_undetected_encoders: bool,
    pub bitrate_mbps: u32,
    pub upscale: bool,
    pub use_chroma_key: bool,
    pub chroma_key: [f32; 3],
    /// Draw the overlays on the GPU (wgpu) instead of the CPU, and which frame format to use.
    #[serde(default)]
    pub gpu_mode: GpuMode,
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
            // GPU compositing with NV12 frames is the fastest validated path on macOS. Other
            // platforms keep the CPU path until the GPU path has been tested there.
            #[cfg(target_os = "macos")]
            gpu_mode: GpuMode::Nv12,
            #[cfg(not(target_os = "macos"))]
            gpu_mode: GpuMode::Off,
        }
    }
}
