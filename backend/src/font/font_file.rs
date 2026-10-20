use std::{borrow::Cow, path::PathBuf};

use derivative::Derivative;
use image::{imageops::FilterType, io::Reader, DynamicImage, GenericImageView, ImageBuffer, Rgba, RgbaImage};

use super::{
    dimensions::{detect_dimensions, CharacterSize, FontType},
    error::FontFileError,
};

#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub struct FontFile {
    pub file_path: PathBuf,
    pub character_count: u32,
    pub character_size: CharacterSize,
    pub font_type: FontType,
    #[derivative(Debug = "ignore")]
    characters: Vec<RgbaImage>,
}

impl FontFile {
    #[tracing::instrument(ret, err)]
    pub fn open(path: PathBuf) -> Result<Self, FontFileError> {
        let font_image = Reader::open(&path)?.decode()?;
        let (width, height) = font_image.dimensions();
        let (character_size, font_type, character_count) = detect_dimensions(width, height)?;

        let characters = split_characters(&font_image, &character_size, &font_type, character_count);

        Ok(Self {
            file_path: path,
            character_count,
            character_size,
            font_type,
            characters,
        })
    }

    /// Get a character image at the requested size. If the font's native size matches, the image is borrowed
    /// without any copy. Otherwise it is resized on the fly, which is expensive; for rendering many frames use
    /// [`FontFile::resized`] once up front so every lookup is a cheap borrow.
    pub fn get_character(&self, index: usize, size: &CharacterSize) -> Option<Cow<'_, RgbaImage>> {
        self.characters.get(index).map(|original_image| {
            if self.matches_size(size) {
                Cow::Borrowed(original_image)
            } else {
                Cow::Owned(resize_character(original_image, size))
            }
        })
    }

    /// Return a copy of this font with every character resized to `size`. Resizing happens once here instead of
    /// on every glyph of every frame.
    #[tracing::instrument(skip(self), level = "debug")]
    pub fn resized(&self, size: &CharacterSize) -> FontFile {
        if self.matches_size(size) {
            return self.clone();
        }
        FontFile {
            file_path: self.file_path.clone(),
            character_count: self.character_count,
            character_size: size.clone(),
            font_type: self.font_type.clone(),
            characters: self.characters.iter().map(|c| resize_character(c, size)).collect(),
        }
    }

    /// Number of character images in this font (characters per page times pages).
    pub fn num_characters(&self) -> usize {
        self.characters.len()
    }

    /// Build a font from in-memory character images, for tests.
    #[cfg(test)]
    pub(crate) fn from_characters(characters: Vec<RgbaImage>, size: CharacterSize) -> Self {
        Self {
            file_path: PathBuf::from("synthetic.png"),
            character_count: characters.len() as u32,
            character_size: size,
            font_type: FontType::Standard,
            characters,
        }
    }

    fn matches_size(&self, size: &CharacterSize) -> bool {
        size.width() == self.character_size.width() && size.height() == self.character_size.height()
    }
}

fn resize_character(image: &RgbaImage, size: &CharacterSize) -> RgbaImage {
    image::imageops::resize(image, size.width(), size.height(), FilterType::Lanczos3)
}

fn split_characters(
    font_image: &DynamicImage,
    character_size: &CharacterSize,
    font_type: &FontType,
    character_count: u32,
) -> Vec<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let pages = font_type.pages();
    let char_width = character_size.width();
    let char_height = character_size.height();

    let mut char_vec = Vec::with_capacity((character_count * pages) as usize);

    for page_idx in 0..pages {
        let x = page_idx * char_width;
        for char_idx in 0..character_count {
            let y = char_idx * char_height;
            let char = font_image.view(x, y, char_width, char_height).to_image();
            char_vec.push(char);
        }
    }

    char_vec
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use image::Rgba;

    use super::*;

    fn synthetic_font(size: CharacterSize) -> FontFile {
        let characters = (0..3u8)
            .map(|i| RgbaImage::from_pixel(size.width(), size.height(), Rgba([i, i, i, 255])))
            .collect();
        FontFile {
            file_path: PathBuf::from("synthetic.png"),
            character_count: 3,
            character_size: size,
            font_type: FontType::Standard,
            characters,
        }
    }

    #[test]
    fn get_character_borrows_when_size_matches() {
        let font = synthetic_font(CharacterSize::Large);
        let character = font.get_character(1, &CharacterSize::Large).unwrap();
        assert!(matches!(character, Cow::Borrowed(_)));
        assert_eq!(
            character.dimensions(),
            (CharacterSize::Large.width(), CharacterSize::Large.height())
        );
    }

    #[test]
    fn get_character_resizes_when_size_differs() {
        let font = synthetic_font(CharacterSize::Large);
        let character = font.get_character(1, &CharacterSize::Small).unwrap();
        assert!(matches!(character, Cow::Owned(_)));
        assert_eq!(
            character.dimensions(),
            (CharacterSize::Small.width(), CharacterSize::Small.height())
        );
    }

    #[test]
    fn resized_font_serves_borrowed_characters_at_new_size() {
        let font = synthetic_font(CharacterSize::Large).resized(&CharacterSize::Ultra);
        assert_eq!(font.character_size, CharacterSize::Ultra);
        assert_eq!(font.character_count, 3);
        assert_eq!(font.characters.len(), 3);
        let character = font.get_character(2, &CharacterSize::Ultra).unwrap();
        assert!(matches!(character, Cow::Borrowed(_)));
        assert_eq!(
            character.dimensions(),
            (CharacterSize::Ultra.width(), CharacterSize::Ultra.height())
        );
        // Solid-color input must stay solid after resizing
        assert_eq!(character.get_pixel(10, 10), &Rgba([2, 2, 2, 255]));
    }

    #[test]
    fn resized_to_same_size_is_a_plain_copy() {
        let font = synthetic_font(CharacterSize::Small).resized(&CharacterSize::Small);
        assert_eq!(font.character_size, CharacterSize::Small);
        assert_eq!(font.characters.len(), 3);
    }
}
