use std::sync::mpsc;

use bytemuck::{Pod, Zeroable};
use image::RgbaImage;

use super::{FontAtlas, GpuContext, GpuError, TextStrip};
use crate::{
    font::FontFile,
    osd::{self, OsdOptions},
};

/// One textured quad: an OSD glyph or the SRT text strip. Matches `Instance` in `overlay.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct GlyphInstance {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    viewport: [f32; 2],
    _pad: [f32; 2],
}

/// Build the quad list for an OSD frame, applying the position offset and the glyph mask exactly
/// like `overlay::overlay_osd`.
pub fn build_glyph_instances(
    osd_frame: &osd::Frame,
    osd_options: &OsdOptions,
    atlas: &FontAtlas,
) -> Vec<GlyphInstance> {
    let (w, h) = (atlas.character_width as f32, atlas.character_height as f32);
    osd_frame
        .glyphs
        .iter()
        .filter(|g| g.index != 0 && !osd_options.get_mask(&g.grid_position))
        .filter(|g| (g.index as u32) < atlas.character_count)
        .map(|g| {
            let (uv_min, uv_max) = atlas.uv(g.index as u32);
            GlyphInstance {
                pos: [
                    (g.grid_position.x as i32 * atlas.character_width as i32 + osd_options.position.x) as f32,
                    (g.grid_position.y as i32 * atlas.character_height as i32 + osd_options.position.y) as f32,
                ],
                size: [w, h],
                uv_min,
                uv_max,
            }
        })
        .collect()
}

struct TextQuad {
    bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    texture_size: (u32, u32),
    texture: wgpu::Texture,
}

/// Composites OSD glyphs and SRT text onto video frames on the GPU. One instance handles one
/// video size and one font. Frames go CPU -> GPU -> CPU; stage 2 of the roadmap.
pub struct GpuCompositor {
    ctx: GpuContext,
    width: u32,
    height: u32,
    pipeline: wgpu::RenderPipeline,
    uniform_bind_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    frame_texture: wgpu::Texture,
    frame_view: wgpu::TextureView,
    staging_buffer: wgpu::Buffer,
    padded_bytes_per_row: u32,
    atlas: FontAtlas,
    glyph_buffer: wgpu::Buffer,
    glyph_capacity: usize,
    glyph_count: u32,
    text: Option<TextQuad>,
}

impl GpuCompositor {
    /// Set up a compositor for `width` x `height` frames with the given font, which must already be
    /// resized to the character size matching the video height.
    #[tracing::instrument(skip(font), err)]
    pub fn new(width: u32, height: u32, font: &FontFile) -> Result<Self, GpuError> {
        let ctx = GpuContext::new()?;
        let device = &ctx.device;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("overlay.wgsl").into()),
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay"),
            bind_group_layouts: &[&uniform_layout, &texture_layout],
            push_constant_ranges: &[],
        });

        // Standard non-premultiplied "over" blending, matching image::Pixel::blend on an opaque frame
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
        });

        let uniforms = Uniforms {
            viewport: [width as f32, height as f32],
            _pad: [0.0; 2],
        };
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue
            .write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniforms"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        // Quads map 1:1 onto pixels, so nearest sampling reproduces the CPU blit exactly
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let atlas = FontAtlas::new(device, &ctx.queue, &texture_layout, &sampler, font)?;

        let frame_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let frame_view = frame_texture.create_view(&Default::default());

        // Readback rows must be aligned to COPY_BYTES_PER_ROW_ALIGNMENT
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = (width * 4).div_ceil(align) * align;
        let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded_bytes_per_row * height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let glyph_capacity = 2048;
        let glyph_buffer = Self::create_instance_buffer(device, "glyphs", glyph_capacity);

        Ok(Self {
            ctx,
            width,
            height,
            pipeline,
            uniform_bind_group,
            texture_layout,
            sampler,
            frame_texture,
            frame_view,
            staging_buffer,
            padded_bytes_per_row,
            atlas,
            glyph_buffer,
            glyph_capacity,
            glyph_count: 0,
            text: None,
        })
    }

    fn create_instance_buffer(device: &wgpu::Device, label: &str, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (capacity * std::mem::size_of::<GlyphInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn atlas(&self) -> &FontAtlas {
        &self.atlas
    }

    pub fn adapter_name(&self) -> &str {
        &self.ctx.adapter_info.name
    }

    /// Replace the OSD glyph quads. Call when the OSD frame changes, not on every video frame.
    pub fn set_glyphs(&mut self, glyphs: &[GlyphInstance]) {
        if glyphs.len() > self.glyph_capacity {
            self.glyph_capacity = glyphs.len().next_power_of_two();
            self.glyph_buffer = Self::create_instance_buffer(&self.ctx.device, "glyphs", self.glyph_capacity);
        }
        if !glyphs.is_empty() {
            self.ctx
                .queue
                .write_buffer(&self.glyph_buffer, 0, bytemuck::cast_slice(glyphs));
        }
        self.glyph_count = glyphs.len() as u32;
    }

    /// Replace the SRT text strip, drawn with its top-left corner at `anchor + strip.offset`.
    /// Call when the text changes, not on every video frame. `None` draws no text.
    pub fn set_text(&mut self, strip: Option<&TextStrip>, anchor: (i32, i32)) {
        let Some(strip) = strip else {
            self.text = None;
            return;
        };
        let size = strip.image.dimensions();
        let device = &self.ctx.device;

        let needs_new_texture = self.text.as_ref().map(|t| t.texture_size != size).unwrap_or(true);
        if needs_new_texture {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("srt text"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("srt text"),
                layout: &self.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            let instance_buffer = Self::create_instance_buffer(device, "srt text quad", 1);
            self.text = Some(TextQuad {
                bind_group,
                instance_buffer,
                texture_size: size,
                texture,
            });
        }

        let text = self.text.as_ref().unwrap();
        self.ctx.queue.write_texture(
            text.texture.as_image_copy(),
            strip.image.as_raw(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(size.0 * 4),
                rows_per_image: Some(size.1),
            },
            text.texture.size(),
        );
        let instance = GlyphInstance {
            pos: [(anchor.0 + strip.offset.0) as f32, (anchor.1 + strip.offset.1) as f32],
            size: [size.0 as f32, size.1 as f32],
            uv_min: [0.0, 0.0],
            uv_max: [1.0, 1.0],
        };
        self.ctx
            .queue
            .write_buffer(&text.instance_buffer, 0, bytemuck::bytes_of(&instance));
    }

    /// Draw the current glyphs and text onto `frame` (RGBA, width * height * 4 bytes) in place.
    /// With a chroma key the frame contents are ignored and the key color is used as background.
    pub fn composite(&mut self, frame: &mut Vec<u8>, chroma_key: Option<[u8; 3]>) {
        let extent = wgpu::Extent3d {
            width: self.width,
            height: self.height,
            depth_or_array_layers: 1,
        };

        let load = match chroma_key {
            Some([r, g, b]) => wgpu::LoadOp::Clear(wgpu::Color {
                r: r as f64 / 255.0,
                g: g as f64 / 255.0,
                b: b as f64 / 255.0,
                a: 1.0,
            }),
            None => {
                self.ctx.queue.write_texture(
                    self.frame_texture.as_image_copy(),
                    frame,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(self.width * 4),
                        rows_per_image: Some(self.height),
                    },
                    extent,
                );
                wgpu::LoadOp::Load
            }
        };

        let mut encoder = self.ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("composite"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.frame_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            if self.glyph_count > 0 {
                pass.set_bind_group(1, &self.atlas.bind_group, &[]);
                pass.set_vertex_buffer(0, self.glyph_buffer.slice(..));
                pass.draw(0..6, 0..self.glyph_count);
            }
            if let Some(text) = &self.text {
                pass.set_bind_group(1, &text.bind_group, &[]);
                pass.set_vertex_buffer(0, text.instance_buffer.slice(..));
                pass.draw(0..6, 0..1);
            }
        }
        encoder.copy_texture_to_buffer(
            self.frame_texture.as_image_copy(),
            wgpu::ImageCopyBuffer {
                buffer: &self.staging_buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            extent,
        );
        self.ctx.queue.submit(Some(encoder.finish()));

        // Synchronous readback (stage 2). Overlapping frames comes in the next stage.
        let slice = self.staging_buffer.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).ok();
        });
        self.ctx.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .expect("map_async callback dropped")
            .expect("Failed to map readback buffer");
        {
            let mapped = slice.get_mapped_range();
            let row_bytes = (self.width * 4) as usize;
            frame.clear();
            if self.padded_bytes_per_row as usize == row_bytes {
                frame.extend_from_slice(&mapped);
            } else {
                for row in mapped.chunks(self.padded_bytes_per_row as usize) {
                    frame.extend_from_slice(&row[..row_bytes]);
                }
            }
        }
        self.staging_buffer.unmap();
    }

    /// Convenience for tests and previews: composite onto an image.
    pub fn composite_image(&mut self, image: &mut RgbaImage, chroma_key: Option<[u8; 3]>) {
        let (w, h) = image.dimensions();
        let mut data = std::mem::take(image).into_raw();
        self.composite(&mut data, chroma_key);
        *image = RgbaImage::from_raw(w, h, data).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::*;
    use crate::{
        font::CharacterSize,
        gpu::rasterize_text,
        osd::{Frame, Glyph, GridPosition},
        overlay::{overlay_osd, overlay_srt_data, srt_layout, srt_text, SRT_TEXT_COLOR},
        srt::{SrtFrameData, SrtOptions},
        util::Coordinates,
    };

    fn synthetic_font(size: CharacterSize) -> FontFile {
        let (w, h) = (size.width(), size.height());
        let characters = (0..8u32)
            .map(|i| {
                RgbaImage::from_fn(w, h, |x, y| {
                    // Distinct pattern per character with a full range of alpha values
                    let alpha = ((x * 7 + y * 13 + i * 31) % 256) as u8;
                    Rgba([(i * 30) as u8, (x * 255 / w) as u8, (y * 255 / h) as u8, alpha])
                })
            })
            .collect();
        FontFile::from_characters(characters, size)
    }

    fn base_frame(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255])
        })
    }

    fn osd_frame() -> Frame {
        let glyph = |index: u16, x: u32, y: u32| Glyph {
            index,
            grid_position: GridPosition { x, y },
        };
        Frame {
            time_millis: 0,
            glyphs: vec![
                glyph(1, 0, 0),
                glyph(2, 1, 0),
                glyph(3, 5, 3),
                glyph(4, 52, 19), // bottom-right corner, partially off-screen at 1080p
                glyph(0, 7, 7),   // index 0 is never drawn
                glyph(5, 10, 10), // masked below
                glyph(200, 2, 2), // outside the synthetic font: skipped
            ],
        }
    }

    fn max_rgb_diff(a: &RgbaImage, b: &RgbaImage) -> (u8, usize) {
        let mut max = 0u8;
        let mut count = 0usize;
        for (pa, pb) in a.pixels().zip(b.pixels()) {
            let d = (0..3).map(|c| pa.0[c].abs_diff(pb.0[c])).max().unwrap();
            if d > 0 {
                count += 1;
            }
            max = max.max(d);
        }
        (max, count)
    }

    /// Like `max_rgb_diff`, but skipping pixels where the CPU text path over-covered: imageproc
    /// does not clamp rusttype's coverage, which exceeds 1.0 where glyph contours overlap, and
    /// over-brightens those pixels. Coverage <= 1 always leaves alpha >= the text color's alpha,
    /// so a lower alpha identifies them. The GPU path clamps and draws the text color instead.
    fn max_rgb_diff_ignoring_saturated(cpu: &RgbaImage, gpu: &RgbaImage) -> (u8, usize, usize) {
        let mut max = 0u8;
        let mut count = 0usize;
        let mut saturated = 0usize;
        for (pa, pb) in cpu.pixels().zip(gpu.pixels()) {
            if pa.0[3] < SRT_TEXT_COLOR.0[3] {
                saturated += 1;
                continue;
            }
            let d = (0..3).map(|c| pa.0[c].abs_diff(pb.0[c])).max().unwrap();
            if d > 0 {
                count += 1;
            }
            max = max.max(d);
        }
        (max, count, saturated)
    }

    fn gpu_or_skip(width: u32, height: u32, font: &FontFile) -> Option<GpuCompositor> {
        match GpuCompositor::new(width, height, font) {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                eprintln!("Skipping GPU test, no usable GPU: {e}");
                None
            }
        }
    }

    #[test]
    fn gpu_osd_overlay_matches_cpu() {
        let (width, height) = (1920, 1080);
        let font = synthetic_font(CharacterSize::Large);
        let Some(mut gpu) = gpu_or_skip(width, height, &font) else {
            return;
        };

        let mut options = OsdOptions {
            position: Coordinates::new(-3, 5),
            ..Default::default()
        };
        options.toggle_mask(Coordinates::new(10, 10));
        let frame = osd_frame();

        let mut cpu_image = base_frame(width, height);
        overlay_osd(&mut cpu_image, &frame, &font, &options);

        let mut gpu_image = base_frame(width, height);
        gpu.set_glyphs(&build_glyph_instances(&frame, &options, gpu.atlas()));
        gpu.composite_image(&mut gpu_image, None);

        let (max, count) = max_rgb_diff(&cpu_image, &gpu_image);
        assert!(max <= 1, "max channel difference {max} over {count} pixels");
    }

    #[test]
    fn gpu_chroma_key_matches_cpu() {
        let (width, height) = (1280, 720);
        let font = synthetic_font(CharacterSize::Small);
        let Some(mut gpu) = gpu_or_skip(width, height, &font) else {
            return;
        };
        let options = OsdOptions::default();
        let frame = osd_frame();
        let key = [1u8, 177, 64];

        let mut cpu_image = RgbaImage::from_pixel(width, height, Rgba([key[0], key[1], key[2], 255]));
        overlay_osd(&mut cpu_image, &frame, &font, &options);

        let mut gpu_image = base_frame(width, height);
        gpu.set_glyphs(&build_glyph_instances(&frame, &options, gpu.atlas()));
        gpu.composite_image(&mut gpu_image, Some(key));

        let (max, count) = max_rgb_diff(&cpu_image, &gpu_image);
        assert!(max <= 1, "max channel difference {max} over {count} pixels");
    }

    #[test]
    fn gpu_srt_text_matches_cpu() {
        let (width, height) = (1920, 1080);
        let font = synthetic_font(CharacterSize::Large);
        let Some(mut gpu) = gpu_or_skip(width, height, &font) else {
            return;
        };
        let srt_font =
            rusttype::Font::try_from_bytes(include_bytes!("../../../resources/fonts/AzeretMono-Regular.ttf")).unwrap();
        let srt_options = SrtOptions {
            show_time: true,
            show_sbat: true,
            show_gbat: true,
            ..Default::default()
        };
        let data = SrtFrameData {
            signal: 4,
            channel: 7,
            flight_time: 125,
            sky_bat: 16.4,
            ground_bat: 7.9,
            latency: 32,
            bitrate_mbps: 25.5,
            distance: 1234,
        };

        let mut cpu_image = base_frame(width, height);
        overlay_srt_data(&mut cpu_image, &data, &srt_font, &srt_options);

        let mut gpu_image = base_frame(width, height);
        let (x, y, scale) = srt_layout(&srt_options, width, height);
        let strip = rasterize_text(&srt_font, scale, &srt_text(&data, &srt_options)).unwrap();
        gpu.set_text(Some(&strip), (x, y));
        gpu.composite_image(&mut gpu_image, None);

        let (max, count, saturated) = max_rgb_diff_ignoring_saturated(&cpu_image, &gpu_image);
        assert!(count > 0, "text was not drawn");
        // AzeretMono has overlapping contours in many glyphs, so a sizeable share of text pixels
        // is over-covered on the CPU. Those pixels are white there and the text color on the GPU.
        assert!(
            saturated < count / 2,
            "{saturated} over-covered pixels is more than expected"
        );
        assert!(max <= 2, "max channel difference {max} over {count} pixels");
    }
}
