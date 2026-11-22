# Image formats and Tabletop Simulator

Findings from loading synthetic test images as custom objects in the game
(2026-11-22, unofficial observations, not a specification). The test images
were generated, not taken from mods. Files were loaded from local paths;
URLs were not tested separately.

## Observed behaviour

| Input | Result |
|---|---|
| PNG: RGB, RGBA, gray, gray+alpha, palette, 8-bit | Loads |
| PNG: 16-bit RGB, RGBA, gray | Loads |
| JPEG: RGB, gray (baseline) | Loads |
| WebP (lossless) | Loads |
| BMP, TIFF | Rejected ("unsupported format: UNKNOWN") |
| GIF, static or animated | Rejected ("unsupported format: GIF") |
| Animated PNG | Loads, shows a single still image |
| Odd sizes (300x200, 1x1), 4096x4096 | Load |
| 8192x8192 | Loads, noticeably slow |
| Wrong or missing file extension (PNG data named `.jpg`, `.dat` or without extension, JPEG data named `.png` or `.webp`) | Loads: the format is detected from the content |
| Embedded ICC colour profile (Display P3) | Ignored: looks identical to the same pixels without it |

### Alpha and tokens

Custom tiles keep a fixed shape. Custom tokens cut their outline from the
image alpha, and only pixels with alpha 255 count: in a test image with bands
of alpha 255, 254, 250, 245, 240, 230 and 200, only the 255 band stayed in the
token. Consequences:

- Hard-edged transparency works, including holes. Soft edges shrink the token
  to the fully opaque core.
- An image without any fully opaque region (uniform alpha below 255, or a
  gradient) fails with `Failed to create Token from Image` and the token
  falls back to a red square.

### Not tested

Progressive JPEG, interlaced PNG, animated WebP, CMYK JPEG, lossy WebP.

## Policy for ttskit image commands

- Read PNG, JPEG and WebP; write PNG and JPEG. Other formats are an error that
  names the format.
- Animated input (APNG, animated WebP): use the first frame and print a
  warning, since the game shows a still frame anyway.
- 16-bit channels are kept.
- ICC profiles are dropped with a note, since the game ignores them.
- Partial alpha is reported, and commands warn when an image has no fully
  opaque pixel, because a token made from it would fail.
- The format is decided by content, never by extension.

`ttskit image info` reports the facts these rules depend on: format, colour
layout, bit depth, frame count and ICC profile size.
