use image::{Rgba, RgbaImage};
use rusttype::{point, Font, Scale};

use crate::overlay::SRT_TEXT_COLOR;

/// A line of text rasterized into a small RGBA image with alpha = glyph coverage, ready to be
/// blended onto a frame. `offset` is where the image's top-left corner goes relative to the text
/// anchor, so that it lines up exactly with what `imageproc::drawing::draw_text_mut` would draw.
pub struct TextStrip {
    pub image: RgbaImage,
    pub offset: (i32, i32),
}

/// Rasterize `text` the same way `imageproc::drawing::draw_text_mut` lays it out: glyphs are
/// positioned with the baseline at the font's ascent below the anchor.
pub fn rasterize_text(font: &Font, scale: f32, text: &str) -> Option<TextStrip> {
    let scale = Scale::uniform(scale);
    let v_metrics = font.v_metrics(scale);
    let glyphs: Vec<_> = font
        .layout(text, scale, point(0.0, v_metrics.ascent))
        .filter_map(|g| g.pixel_bounding_box().map(|bb| (g, bb)))
        .collect();
    if glyphs.is_empty() {
        return None;
    }

    let min_x = glyphs.iter().map(|(_, bb)| bb.min.x).min().unwrap();
    let min_y = glyphs.iter().map(|(_, bb)| bb.min.y).min().unwrap();
    let max_x = glyphs.iter().map(|(_, bb)| bb.max.x).max().unwrap();
    let max_y = glyphs.iter().map(|(_, bb)| bb.max.y).max().unwrap();
    let width = (max_x - min_x).max(1) as u32;
    let height = (max_y - min_y).max(1) as u32;

    let [r, g, b, _] = SRT_TEXT_COLOR.0;
    let mut image = RgbaImage::new(width, height);
    for (glyph, bb) in &glyphs {
        glyph.draw(|gx, gy, coverage| {
            let x = (bb.min.x - min_x) as u32 + gx;
            let y = (bb.min.y - min_y) as u32 + gy;
            if x < width && y < height {
                // Where glyph boxes overlap the CPU path blends the same color twice in sequence,
                // which equals one blend with alpha 1 - (1 - a1)(1 - a2).
                let existing = image.get_pixel(x, y).0[3] as f32 / 255.0;
                let combined = 1.0 - (1.0 - existing) * (1.0 - coverage);
                let alpha = (combined * 255.0).round() as u8;
                image.put_pixel(x, y, Rgba([r, g, b, alpha]));
            }
        });
    }

    Some(TextStrip {
        image,
        offset: (min_x, min_y),
    })
}
