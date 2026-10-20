//! GPU (wgpu) implementation of the overlay compositing.
//!
//! Stage 2 of the GPU roadmap (see PERFORMANCE_NOTES.md): the decoded RGBA frame is uploaded to a
//! texture, the OSD glyphs and SRT text are drawn as textured quads, and the result is read back
//! to the CPU for the encoder. Output must match the CPU path in `overlay` to within rounding.

mod atlas;
mod compositor;
mod context;
mod error;
mod text;

pub use atlas::FontAtlas;
pub use compositor::{build_glyph_instances, GlyphInstance, GpuCompositor};
pub use context::GpuContext;
pub use error::GpuError;
pub use text::{rasterize_text, TextStrip};
