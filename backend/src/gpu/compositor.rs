use std::sync::mpsc;

use bytemuck::{Pod, Zeroable};
use image::RgbaImage;

use super::{FontAtlas, GpuContext, GpuError, TextStrip};
use crate::{
    ffmpeg::PixelFormat,
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

/// Resources for exchanging NV12 frames with ffmpeg: the input planes, the conversion passes and
/// the output planes.
struct Nv12Io {
    y_in: wgpu::Texture,
    uv_in: wgpu::Texture,
    y_out_view: wgpu::TextureView,
    uv_out_view: wgpu::TextureView,
    y_out: wgpu::Texture,
    uv_out: wgpu::Texture,
    /// Y and UV input planes, read by the NV12 -> RGB pass.
    input_bind_group: wgpu::BindGroup,
    /// The composited frame, read by the RGB -> NV12 passes. Kept separate from the input group
    /// because the frame texture is the render target while the input planes are read.
    output_bind_group: wgpu::BindGroup,
    nv12_to_rgb: wgpu::RenderPipeline,
    rgb_to_y: wgpu::RenderPipeline,
    rgb_to_uv: wgpu::RenderPipeline,
    /// Row stride of the Y plane in the readback buffer.
    y_padded_bytes_per_row: u32,
    /// Row stride of the UV plane in the readback buffer, which starts at `uv_offset`.
    uv_padded_bytes_per_row: u32,
    uv_offset: u64,
}

/// Composites OSD glyphs and SRT text onto video frames on the GPU. One instance handles one
/// video size and one font. Frames go CPU -> GPU -> CPU; stage 2 of the roadmap.
pub struct GpuCompositor {
    ctx: GpuContext,
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    nv12: Option<Nv12Io>,
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
    /// resized to the character size matching the video height. `pixel_format` is the format of the
    /// frames passed to and returned from `composite`.
    #[tracing::instrument(skip(font), err)]
    pub fn new(width: u32, height: u32, font: &FontFile, pixel_format: PixelFormat) -> Result<Self, GpuError> {
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
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let frame_view = frame_texture.create_view(&Default::default());

        // Readback rows must be aligned to COPY_BYTES_PER_ROW_ALIGNMENT
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = (width * 4).div_ceil(align) * align;
        let nv12 = match pixel_format {
            PixelFormat::Rgba => None,
            PixelFormat::Nv12 => Some(Self::create_nv12_io(device, width, height, &frame_texture)),
        };
        let readback_size = match &nv12 {
            None => (padded_bytes_per_row * height) as u64,
            Some(io) => io.uv_offset + (io.uv_padded_bytes_per_row * (height / 2)) as u64,
        };
        let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let glyph_capacity = 2048;
        let glyph_buffer = Self::create_instance_buffer(device, "glyphs", glyph_capacity);

        Ok(Self {
            ctx,
            width,
            height,
            pixel_format,
            nv12,
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

    fn create_plane_texture(
        device: &wgpu::Device,
        label: &str,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    }

    fn create_nv12_io(device: &wgpu::Device, width: u32, height: u32, frame_texture: &wgpu::Texture) -> Nv12Io {
        let (uv_width, uv_height) = (width / 2, height / 2);
        let input_usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        let output_usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        let y_in = Self::create_plane_texture(
            device,
            "nv12 y in",
            width,
            height,
            wgpu::TextureFormat::R8Unorm,
            input_usage,
        );
        let uv_in = Self::create_plane_texture(
            device,
            "nv12 uv in",
            uv_width,
            uv_height,
            wgpu::TextureFormat::Rg8Unorm,
            input_usage,
        );
        let y_out = Self::create_plane_texture(
            device,
            "nv12 y out",
            width,
            height,
            wgpu::TextureFormat::R8Unorm,
            output_usage,
        );
        let uv_out = Self::create_plane_texture(
            device,
            "nv12 uv out",
            uv_width,
            uv_height,
            wgpu::TextureFormat::Rg8Unorm,
            output_usage,
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12 conversion shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("convert.wgsl").into()),
        });
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let input_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nv12 input planes"),
            entries: &[texture_entry(0), texture_entry(1)],
        });
        let output_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composited frame"),
            entries: &[texture_entry(2)],
        });
        let input_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12 input planes"),
            layout: &input_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_in.create_view(&Default::default())),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_in.create_view(&Default::default())),
                },
            ],
        });
        let output_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composited frame"),
            layout: &output_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&frame_texture.create_view(&Default::default())),
            }],
        });
        let input_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nv12 to rgb"),
            bind_group_layouts: &[&input_layout],
            push_constant_ranges: &[],
        });
        let output_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rgb to nv12"),
            bind_group_layouts: &[&output_layout],
            push_constant_ranges: &[],
        });
        let make_pipeline =
            |label: &str, entry_point: &str, format: wgpu::TextureFormat, layout: &wgpu::PipelineLayout| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: "fullscreen_vs",
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        ..Default::default()
                    },
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point,
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview: None,
                })
            };
        let nv12_to_rgb = make_pipeline(
            "nv12 to rgb",
            "nv12_to_rgb_fs",
            wgpu::TextureFormat::Rgba8Unorm,
            &input_pipeline_layout,
        );
        let rgb_to_y = make_pipeline(
            "rgb to y",
            "rgb_to_y_fs",
            wgpu::TextureFormat::R8Unorm,
            &output_pipeline_layout,
        );
        let rgb_to_uv = make_pipeline(
            "rgb to uv",
            "rgb_to_uv_fs",
            wgpu::TextureFormat::Rg8Unorm,
            &output_pipeline_layout,
        );

        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let y_padded_bytes_per_row = width.div_ceil(align) * align;
        let uv_padded_bytes_per_row = (uv_width * 2).div_ceil(align) * align;
        // Buffer offsets for texture copies must be aligned too (align is a multiple of that)
        let uv_offset = (y_padded_bytes_per_row * height) as u64;

        Nv12Io {
            y_out_view: y_out.create_view(&Default::default()),
            uv_out_view: uv_out.create_view(&Default::default()),
            y_in,
            uv_in,
            y_out,
            uv_out,
            input_bind_group,
            output_bind_group,
            nv12_to_rgb,
            rgb_to_y,
            rgb_to_uv,
            y_padded_bytes_per_row,
            uv_padded_bytes_per_row,
            uv_offset,
        }
    }

    pub fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
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

    /// Draw the current glyphs and text onto `frame` in place. The frame is in the compositor's
    /// pixel format (RGBA: width * height * 4 bytes; NV12: a Y plane followed by a UV plane).
    /// With a chroma key the frame contents are ignored and the key color is used as background.
    pub fn composite(&mut self, frame: &mut Vec<u8>, chroma_key: Option<[u8; 3]>) {
        debug_assert_eq!(frame.len(), self.pixel_format.frame_bytes(self.width, self.height));
        let extent = wgpu::Extent3d {
            width: self.width,
            height: self.height,
            depth_or_array_layers: 1,
        };

        // Upload the input frame. With a chroma key the input is ignored and the frame texture is
        // cleared instead. NV12 input is converted to RGB by the first draw of the overlay pass.
        let mut convert_input = false;
        let load = match chroma_key {
            Some([r, g, b]) => wgpu::LoadOp::Clear(wgpu::Color {
                r: r as f64 / 255.0,
                g: g as f64 / 255.0,
                b: b as f64 / 255.0,
                a: 1.0,
            }),
            None => match &self.nv12 {
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
                Some(io) => {
                    let y_bytes = (self.width * self.height) as usize;
                    self.ctx.queue.write_texture(
                        io.y_in.as_image_copy(),
                        &frame[..y_bytes],
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(self.width),
                            rows_per_image: Some(self.height),
                        },
                        extent,
                    );
                    self.ctx.queue.write_texture(
                        io.uv_in.as_image_copy(),
                        &frame[y_bytes..],
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(self.width),
                            rows_per_image: Some(self.height / 2),
                        },
                        io.uv_in.size(),
                    );
                    convert_input = true;
                    // Every pixel is overwritten by the conversion, no need to load or clear
                    wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                }
            },
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
            if convert_input {
                let io = self.nv12.as_ref().unwrap();
                pass.set_pipeline(&io.nv12_to_rgb);
                pass.set_bind_group(0, &io.input_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
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
        match &self.nv12 {
            None => encoder.copy_texture_to_buffer(
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
            ),
            Some(io) => {
                // Convert the composited frame back to NV12 planes, then copy them out
                for (view, pipeline) in [(&io.y_out_view, &io.rgb_to_y), (&io.uv_out_view, &io.rgb_to_uv)] {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("rgb to nv12"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, &io.output_bind_group, &[]);
                    pass.draw(0..3, 0..1);
                }
                encoder.copy_texture_to_buffer(
                    io.y_out.as_image_copy(),
                    wgpu::ImageCopyBuffer {
                        buffer: &self.staging_buffer,
                        layout: wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(io.y_padded_bytes_per_row),
                            rows_per_image: Some(self.height),
                        },
                    },
                    extent,
                );
                encoder.copy_texture_to_buffer(
                    io.uv_out.as_image_copy(),
                    wgpu::ImageCopyBuffer {
                        buffer: &self.staging_buffer,
                        layout: wgpu::ImageDataLayout {
                            offset: io.uv_offset,
                            bytes_per_row: Some(io.uv_padded_bytes_per_row),
                            rows_per_image: Some(self.height / 2),
                        },
                    },
                    io.uv_out.size(),
                );
            }
        }
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
            frame.clear();
            match &self.nv12 {
                None => Self::unpad_rows(
                    frame,
                    &mapped,
                    (self.width * 4) as usize,
                    self.padded_bytes_per_row as usize,
                ),
                Some(io) => {
                    let y_region = &mapped[..io.uv_offset as usize];
                    Self::unpad_rows(frame, y_region, self.width as usize, io.y_padded_bytes_per_row as usize);
                    let uv_region = &mapped[io.uv_offset as usize..];
                    Self::unpad_rows(
                        frame,
                        uv_region,
                        self.width as usize,
                        io.uv_padded_bytes_per_row as usize,
                    );
                }
            }
        }
        self.staging_buffer.unmap();
    }

    /// Append the rows of a padded readback region to `out`, dropping the row padding.
    fn unpad_rows(out: &mut Vec<u8>, region: &[u8], row_bytes: usize, padded_row_bytes: usize) {
        if padded_row_bytes == row_bytes {
            out.extend_from_slice(region);
        } else {
            for row in region.chunks(padded_row_bytes) {
                out.extend_from_slice(&row[..row_bytes]);
            }
        }
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
        gpu_or_skip_with(width, height, font, PixelFormat::Rgba)
    }

    fn gpu_or_skip_with(width: u32, height: u32, font: &FontFile, format: PixelFormat) -> Option<GpuCompositor> {
        match GpuCompositor::new(width, height, font, format) {
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

    // CPU reference implementation of the BT.709 limited range conversions in convert.wgsl

    fn rgb_to_yuv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
        let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let cb = (b - y) / 1.8556;
        let cr = (r - y) / 1.5748;
        (16.0 + y * 219.0, 128.0 + cb * 224.0, 128.0 + cr * 224.0)
    }

    fn yuv_to_rgb(y: u8, u: u8, v: u8) -> Rgba<u8> {
        let yy = (y as f32 - 16.0) / 219.0;
        let cb = (u as f32 - 128.0) / 224.0;
        let cr = (v as f32 - 128.0) / 224.0;
        let to_u8 = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgba([
            to_u8(yy + 1.5748 * cr),
            to_u8(yy - 0.1873 * cb - 0.4681 * cr),
            to_u8(yy + 1.8556 * cb),
            255,
        ])
    }

    fn rgba_to_nv12(image: &RgbaImage) -> Vec<u8> {
        let (w, h) = image.dimensions();
        let mut out = Vec::with_capacity((w * h * 3 / 2) as usize);
        for p in image.pixels() {
            out.push(rgb_to_yuv(p.0[0], p.0[1], p.0[2]).0.round() as u8);
        }
        for by in 0..h / 2 {
            for bx in 0..w / 2 {
                let (mut u, mut v) = (0.0, 0.0);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let p = image.get_pixel(bx * 2 + dx, by * 2 + dy);
                    let (_, pu, pv) = rgb_to_yuv(p.0[0], p.0[1], p.0[2]);
                    u += pu;
                    v += pv;
                }
                out.push((u / 4.0).round() as u8);
                out.push((v / 4.0).round() as u8);
            }
        }
        out
    }

    fn nv12_to_rgba(data: &[u8], w: u32, h: u32) -> RgbaImage {
        let y_plane = &data[..(w * h) as usize];
        let uv_plane = &data[(w * h) as usize..];
        RgbaImage::from_fn(w, h, |x, y| {
            let yy = y_plane[(y * w + x) as usize];
            let uv_index = ((y / 2) * (w / 2) + x / 2) as usize * 2;
            yuv_to_rgb(yy, uv_plane[uv_index], uv_plane[uv_index + 1])
        })
    }

    fn max_plane_diff(a: &[u8], b: &[u8]) -> u8 {
        a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
    }

    #[test]
    fn gpu_nv12_round_trip_matches_cpu_reference() {
        let (width, height) = (1920, 1080);
        let font = synthetic_font(CharacterSize::Large);
        let Some(mut gpu) = gpu_or_skip_with(width, height, &font, PixelFormat::Nv12) else {
            return;
        };
        assert_eq!(gpu.pixel_format(), PixelFormat::Nv12);
        let options = OsdOptions::default();
        let frame = osd_frame();

        // Input: a gradient frame as NV12
        let input = rgba_to_nv12(&base_frame(width, height));

        // CPU reference: NV12 -> RGB (chroma replicated) -> overlay -> NV12
        let mut reference = nv12_to_rgba(&input, width, height);
        overlay_osd(&mut reference, &frame, &font, &options);
        let expected = rgba_to_nv12(&reference);

        let mut output = input.clone();
        gpu.set_glyphs(&build_glyph_instances(&frame, &options, gpu.atlas()));
        gpu.composite(&mut output, None);

        assert_eq!(output.len(), expected.len());
        let y_len = (width * height) as usize;
        let y_diff = max_plane_diff(&output[..y_len], &expected[..y_len]);
        let uv_diff = max_plane_diff(&output[y_len..], &expected[y_len..]);
        assert!(y_diff <= 2, "Y plane differs by up to {y_diff}");
        assert!(uv_diff <= 2, "UV plane differs by up to {uv_diff}");

        // Without an overlay the GPU round trip must match the CPU reference round trip. The raw
        // input is not recoverable in general: out-of-gamut YUV combinations clamp in RGB.
        gpu.set_glyphs(&[]);
        let mut plain = input.clone();
        gpu.composite(&mut plain, None);
        let reference = rgba_to_nv12(&nv12_to_rgba(&input, width, height));
        let y_diff = max_plane_diff(&plain[..y_len], &reference[..y_len]);
        let uv_diff = max_plane_diff(&plain[y_len..], &reference[y_len..]);
        assert!(
            y_diff <= 2 && uv_diff <= 2,
            "round trip differs from reference: Y {y_diff}, UV {uv_diff}"
        );

        // An in-gamut, low-saturation input must come back unchanged up to rounding, which checks
        // that the two conversion matrices are inverses of each other.
        let soft = RgbaImage::from_fn(width, height, |x, y| {
            Rgba([
                (64 + x % 128) as u8,
                (64 + y % 128) as u8,
                (96 + (x + y) % 64) as u8,
                255,
            ])
        });
        let soft_input = rgba_to_nv12(&soft);
        let mut soft_output = soft_input.clone();
        gpu.composite(&mut soft_output, None);
        let y_diff = max_plane_diff(&soft_output[..y_len], &soft_input[..y_len]);
        let uv_diff = max_plane_diff(&soft_output[y_len..], &soft_input[y_len..]);
        assert!(
            y_diff <= 1 && uv_diff <= 1,
            "in-gamut round trip changed the video: Y {y_diff}, UV {uv_diff}"
        );
    }
}
