use thiserror::Error;

#[derive(Debug, Error)]
pub enum GpuError {
    #[error("No compatible GPU adapter found")]
    NoAdapter,
    #[error("Failed to create GPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[error("Font atlas of {width}x{height} exceeds the GPU's maximum texture size of {max}")]
    AtlasTooLarge { width: u32, height: u32, max: u32 },
}
