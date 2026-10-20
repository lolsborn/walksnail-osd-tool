use image::RgbaImage;

use super::GpuError;
use crate::font::FontFile;

/// Grid layout of the atlas: characters are placed left to right, top to bottom.
const ATLAS_COLUMNS: u32 = 16;

/// All characters of a font, at one character size, packed into a single GPU texture.
pub struct FontAtlas {
    pub bind_group: wgpu::BindGroup,
    pub character_width: u32,
    pub character_height: u32,
    pub character_count: u32,
    texture_width: f32,
    texture_height: f32,
}

impl FontAtlas {
    /// Pack `font` at its own character size. Resize the font first (`FontFile::resized`) to build
    /// an atlas for a different video size.
    #[tracing::instrument(skip_all, level = "debug")]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        font: &FontFile,
    ) -> Result<Self, GpuError> {
        let character_width = font.character_size.width();
        let character_height = font.character_size.height();
        let character_count = font.num_characters() as u32;
        let rows = character_count.div_ceil(ATLAS_COLUMNS).max(1);
        let width = ATLAS_COLUMNS * character_width;
        let height = rows * character_height;

        let max = device.limits().max_texture_dimension_2d;
        if width > max || height > max {
            return Err(GpuError::AtlasTooLarge { width, height, max });
        }

        let mut atlas_image = RgbaImage::new(width, height);
        for index in 0..character_count {
            if let Some(character) = font.get_character(index as usize, &font.character_size) {
                let (x, y) = Self::cell_origin(index, character_width, character_height);
                image::imageops::replace(&mut atlas_image, &*character, x as i64, y as i64);
            }
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("font atlas"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            atlas_image.as_raw(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            texture.size(),
        );
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("font atlas"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });

        Ok(Self {
            bind_group,
            character_width,
            character_height,
            character_count,
            texture_width: width as f32,
            texture_height: height as f32,
        })
    }

    fn cell_origin(index: u32, character_width: u32, character_height: u32) -> (u32, u32) {
        let column = index % ATLAS_COLUMNS;
        let row = index / ATLAS_COLUMNS;
        (column * character_width, row * character_height)
    }

    /// Texture coordinates (min, max) of a character cell.
    pub fn uv(&self, index: u32) -> ([f32; 2], [f32; 2]) {
        let (x, y) = Self::cell_origin(index, self.character_width, self.character_height);
        let min = [x as f32 / self.texture_width, y as f32 / self.texture_height];
        let max = [
            (x + self.character_width) as f32 / self.texture_width,
            (y + self.character_height) as f32 / self.texture_height,
        ];
        (min, max)
    }
}
