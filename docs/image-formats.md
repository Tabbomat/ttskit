# Image formats and Tabletop Simulator

Findings from loading synthetic test images as custom objects in the game
(2026-11-22, unofficial observations, not a specification). The test images
were generated, not taken from mods. Files were loaded from local paths;
URLs were not tested separately.

## Observed behaviour

| Input | Result |
|---|---|
| PNG: RGB, RGBA, gray, gray+alpha, palette, 8-bit, interlaced (Adam7) | Loads |
| PNG: 16-bit RGB, RGBA, gray | Loads |
| JPEG: RGB, gray; baseline, progressive, 4:2:0 and 4:4:4 | Loads |
| JPEG: CMYK | Rejected ("Async loading images: JPEG decoding failed") |
| WebP, lossless and lossy, still | Loads |
| WebP, animated (lossless or lossy) | Rejected ("Async loading images: WEBP decoding failed") |
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
- An image whose fully opaque pixels are fewer than 2 px wide or tall also
  fails: a 1 px column, a single opaque pixel and a gradient whose only opaque
  pixels are its last column failed; 2 px columns and 4 px squares worked.
- An image without any fully opaque pixel (for example uniform alpha below
  255) fails with `Failed to create Token from Image` and the token
  falls back to a red square.

### Not tested

Nothing outstanding from the first rounds. Other JPEG or WebP variants (arithmetic coding, 12-bit, extended WebP features such as EXIF) were not tried.

## Policy for ttskit image commands

- Read PNG, JPEG and WebP; write PNG and JPEG. Other formats are an error that
  names the format.
- Animated input (APNG, animated WebP, GIF): use the first frame and print a
  warning. The game shows an animated PNG as a still frame and rejects
  animated WebP and GIF, and ttskit never writes animated output.
- 16-bit channels are kept.
- CMYK JPEG input is converted to RGB (the game rejects CMYK), with a note.
- ICC profiles are dropped with a note, since the game ignores them.
- Partial alpha is reported, and commands warn when an image has no fully
  opaque pixel, because a token made from it would fail.
- The format is decided by content, never by extension.

`ttskit image info` reports the facts these rules depend on: format, colour
layout, bit depth, frame count and ICC profile size.

## How `ttskit image info` reports this

Each finding has a stable code, a severity and a message.

| Code | Severity | Meaning |
|---|---|---|
| `format-rejected` | error | gif, bmp or tiff |
| `jpeg-cmyk` | error | CMYK JPEG |
| `webp-animated` | error | animated WebP |
| `png-animated` | warning | the game shows only a still image |
| `icc-ignored` | warning | embedded colour profile is ignored |
| `very-large` | warning | 8192 px or more on a side loads slowly |
| `token-no-opaque-pixel` | warning | no pixel with alpha 255: no token can be made |
| `token-opaque-too-small` | warning | the alpha-255 pixels span less than 2 px in width or height: no token can be made |
| `token-translucent-pixels` | note | a token outline ignores translucent pixels |
| `alpha-unchecked` | warning | the alpha channel could not be decoded |

A file is `compatible` unless it has an error. Other formats the image library
knows (for example ICO) are reported as unreadable with the reason.
