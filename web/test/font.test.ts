import { describe, expect, it } from "vitest";

import { characterSizeForVideoHeight, detectDimensions, type CharacterSize, type FontType } from "../src/parsers/font";

// Test table from backend/src/font/dimensions.rs
const valid: Array<[number, number, CharacterSize, FontType, number]> = [
  [72, 27648, "ultra", "standard", 256],
  [48, 18432, "xlarge", "standard", 256],
  [36, 13824, "large", "standard", 256],
  [24, 18432, "small", "standard", 512],
  [36, 27648, "large", "standard", 512],
  [48, 36864, "xlarge", "standard", 512],
  [72, 55296, "ultra", "standard", 512],
  [48, 9216, "small", "twoPages", 256],
  [72, 13824, "large", "twoPages", 256],
  [96, 18432, "xlarge", "twoPages", 256],
  [144, 27648, "ultra", "twoPages", 256],
  [72, 9216, "small", "threePages", 256],
  [108, 13824, "large", "threePages", 256],
  [144, 18432, "xlarge", "threePages", 256],
  [216, 27648, "ultra", "threePages", 256],
  [96, 9216, "small", "fourColor", 256],
  [144, 13824, "large", "fourColor", 256],
  [192, 18432, "xlarge", "fourColor", 256],
  [288, 27648, "ultra", "fourColor", 256],
  [18, 6912, "race", "standard", 256],
  [36, 6912, "race", "twoPages", 256],
  [54, 6912, "race", "threePages", 256],
  [72, 6912, "race", "fourColor", 256],
  [18, 13824, "race", "standard", 512],
];

describe("font dimensions", () => {
  it("detects valid font sizes", () => {
    for (const [width, height, size, type, count] of valid) {
      expect(detectDimensions(width, height), `${width}x${height}`).toEqual({ size, type, characterCount: count });
    }
  });

  it("rejects invalid font sizes", () => {
    for (const [width, height] of [
      [36, 13824 + 1],
      [24 + 1, 18432],
      [36 - 12, 27648 - 10],
    ]) {
      expect(() => detectDimensions(width!, height!)).toThrow();
    }
  });

  it("picks the character size for a video height", () => {
    expect(characterSizeForVideoHeight(1080)).toBe("large");
    expect(characterSizeForVideoHeight(720)).toBe("small");
    expect(characterSizeForVideoHeight(2160)).toBe("ultra");
    expect(characterSizeForVideoHeight(999)).toBe("large");
  });
});
