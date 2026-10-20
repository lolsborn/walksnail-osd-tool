// Port of backend/src/font: OSD font PNGs are a vertical strip of character cells, optionally
// with several pages side by side.

export type CharacterSize = "race" | "small" | "large" | "xlarge" | "ultra";
export type FontType = "standard" | "twoPages" | "threePages" | "fourColor";

export const CHARACTER_SIZES: Record<CharacterSize, { width: number; height: number; label: string }> = {
  race: { width: 18, height: 27, label: "540p" },
  small: { width: 24, height: 36, label: "720p" },
  large: { width: 36, height: 54, label: "1080p" },
  xlarge: { width: 48, height: 72, label: "2.7K" },
  ultra: { width: 72, height: 108, label: "4K" },
};

const PAGES: Record<FontType, number> = { standard: 1, twoPages: 2, threePages: 3, fourColor: 4 };

export function pages(type: FontType): number {
  return PAGES[type];
}

export interface FontDimensions {
  size: CharacterSize;
  type: FontType;
  /** Characters per page. */
  characterCount: number;
}

/** Same decision table as the native `detect_dimensions`. */
export function detectDimensions(width: number, height: number): FontDimensions {
  const order: CharacterSize[] = ["small", "large", "xlarge", "ultra", "race"];
  const types: FontType[] = ["standard", "twoPages", "threePages", "fourColor"];
  for (const type of types) {
    for (const size of order) {
      const dims = CHARACTER_SIZES[size];
      if (width === dims.width * PAGES[type] && Math.floor(height / dims.height) % 256 === 0) {
        if (height % dims.height !== 0) throw new Error(`Invalid font file height: ${height}`);
        return { size, type, characterCount: height / dims.height };
      }
    }
  }
  throw new Error(`Invalid font file width: ${width}`);
}

/** Character size for a video of the given height in lines, like the native `get_character_size`. */
export function characterSizeForVideoHeight(lines: number): CharacterSize {
  switch (lines) {
    case 540:
      return "race";
    case 720:
      return "small";
    case 1080:
      return "large";
    case 1440:
      return "xlarge";
    case 2160:
      return "ultra";
    default:
      return "large";
  }
}

export interface FontFile {
  dimensions: FontDimensions;
  /** The character cells, all pages flattened: page 0's characters first, then page 1's, etc. */
  characters: ImageBitmap[];
  /** The character size the cells currently have (after `resizeFont`). */
  size: CharacterSize;
}

/** Load a font PNG (a Blob, Image or ImageBitmap) into individual character bitmaps. Browser only. */
export async function loadFontFile(source: ImageBitmapSource): Promise<FontFile> {
  const image = await createImageBitmap(source);
  const dimensions = detectDimensions(image.width, image.height);
  const { width, height } = CHARACTER_SIZES[dimensions.size];
  const characters: ImageBitmap[] = [];
  for (let page = 0; page < PAGES[dimensions.type]; page++) {
    for (let i = 0; i < dimensions.characterCount; i++) {
      characters.push(await createImageBitmap(image, page * width, i * height, width, height));
    }
  }
  image.close();
  return { dimensions, characters, size: dimensions.size };
}

/** A copy of the font with every character resized to `size`. Done once per video, not per frame. */
export async function resizeFont(font: FontFile, size: CharacterSize): Promise<FontFile> {
  if (font.size === size) return font;
  const { width, height } = CHARACTER_SIZES[size];
  const characters = await Promise.all(
    font.characters.map((c) =>
      createImageBitmap(c, { resizeWidth: width, resizeHeight: height, resizeQuality: "high" }),
    ),
  );
  return { dimensions: font.dimensions, characters, size };
}
