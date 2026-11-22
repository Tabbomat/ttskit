//! Image conversion to PNG or JPEG, the formats Tabletop Simulator loads
//! (`docs/image-formats.md`). Output is deterministic: the same input and
//! options give the same bytes.

use std::io::Cursor;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ColorType, DynamicImage, ImageDecoder, ImageEncoder, ImageReader, Rgba};

use crate::{ImageError, Problem, Severity, inspect_bytes};

/// JPEG quality used when none is given.
pub const DEFAULT_JPEG_QUALITY: u8 = 85;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Png,
    Jpeg,
}

impl OutputFormat {
    /// File extension without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub to: OutputFormat,
    /// JPEG quality, 1 to 100. Ignored for PNG.
    pub quality: u8,
    /// Colour to flatten transparency onto (needed for JPEG output of an
    /// image with transparent pixels).
    pub background: Option<[u8; 3]>,
    /// Keep the colour profile and EXIF data instead of dropping them.
    pub keep_metadata: bool,
}

impl ConvertOptions {
    pub fn new(to: OutputFormat) -> Self {
        Self {
            to,
            quality: DEFAULT_JPEG_QUALITY,
            background: None,
            keep_metadata: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Converted {
    pub bytes: Vec<u8>,
    /// Format of the input, e.g. `png`.
    pub from: String,
    pub width: u32,
    pub height: u32,
    /// What was changed or lost on the way. Codes are stable.
    pub notes: Vec<Problem>,
}

/// Parses `#RRGGBB`, `RRGGBB`, `#RGB`, `RGB`, `white` or `black`.
pub fn parse_color(text: &str) -> Result<[u8; 3], String> {
    let lower = text.trim().to_ascii_lowercase();
    match lower.as_str() {
        "white" => return Ok([255, 255, 255]),
        "black" => return Ok([0, 0, 0]),
        _ => {}
    }
    let hex = lower.strip_prefix('#').unwrap_or(&lower);
    let bad = || format!("invalid colour '{text}': use #RRGGBB, #RGB, white or black");
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let digit = |i: usize| u8::from_str_radix(&hex[i..=i], 16).unwrap();
    let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap();
    match hex.len() {
        3 => Ok([digit(0) * 17, digit(1) * 17, digit(2) * 17]),
        6 => Ok([pair(0), pair(2), pair(4)]),
        _ => Err(bad()),
    }
}

fn note(code: &'static str, message: impl Into<String>) -> Problem {
    Problem::new(Severity::Note, code, message)
}

/// Reads `path` and re-encodes it as `options.to`.
///
/// Rules (see `docs/image-formats.md`): the input format is decided by
/// content; animated input uses its first frame; CMYK JPEG becomes RGB;
/// metadata is dropped unless `keep_metadata`; PNG output keeps 16-bit
/// channels and alpha; JPEG output is 8-bit and refuses an image with
/// transparent pixels unless a background colour is given.
pub fn convert(path: &Path, options: &ConvertOptions) -> Result<Converted, ImageError> {
    check_quality(options)?;
    let data = std::fs::read(path).map_err(|source| ImageError::Io {
        path: path.to_owned(),
        source,
    })?;
    convert_bytes(path, &data, options)
}

fn check_quality(options: &ConvertOptions) -> Result<(), ImageError> {
    if !(1..=100).contains(&options.quality) {
        return Err(ImageError::Options(format!(
            "quality must be between 1 and 100, not {}",
            options.quality
        )));
    }
    Ok(())
}

/// Like [`convert`], for image data already in memory. `label` is only used
/// in error messages; it is never opened.
pub fn convert_bytes(
    label: &Path,
    data: &[u8],
    options: &ConvertOptions,
) -> Result<Converted, ImageError> {
    let path = label;
    check_quality(options)?;
    let io = |source| ImageError::Io {
        path: path.to_owned(),
        source,
    };
    let decode = |source| ImageError::Decode {
        path: path.to_owned(),
        source,
    };
    // Header facts (format, frame count); also rejects formats we do not read.
    let info = inspect_bytes(path, data)?;
    let mut notes = Vec::new();
    if info.animated {
        notes.push(Problem::new(
            Severity::Warning,
            "animated-first-frame",
            format!(
                "animated {} ({} frames): only the first frame is converted",
                info.format, info.frames
            ),
        ));
    }
    if info.color == "cmyk" {
        notes.push(note("cmyk-to-rgb", "CMYK JPEG converted to RGB"));
    }

    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(io)?;
    let mut decoder = reader.into_decoder().map_err(decode)?;
    let icc = decoder.icc_profile().map_err(decode)?;
    let exif = decoder.exif_metadata().map_err(decode)?;
    let orientation = decoder.orientation().map_err(decode)?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(decode)?;

    let keep = options.keep_metadata;
    if !keep || exif.is_none() {
        // The orientation lives in the EXIF data. Without it, rotate the
        // pixels so the image looks the same in every viewer.
        if orientation != image::metadata::Orientation::NoTransforms {
            img.apply_orientation(orientation);
            notes.push(note(
                "orientation-applied",
                "EXIF orientation applied to the pixels",
            ));
        }
    }
    if !keep {
        if let Some(profile) = &icc {
            notes.push(note(
                "icc-dropped",
                format!(
                    "colour profile ({} bytes) dropped; the game ignores it (use --keep-metadata to keep it)",
                    profile.len()
                ),
            ));
        }
        if exif.is_some() {
            notes.push(note("exif-dropped", "EXIF data dropped"));
        }
    }
    let (icc, exif) = if keep { (icc, exif) } else { (None, None) };

    let img = match options.to {
        OutputFormat::Png => for_png(img, &mut notes),
        OutputFormat::Jpeg => for_jpeg(img, options.background, &mut notes, path)?,
    };

    let (width, height) = (img.width(), img.height());
    let mut bytes = Vec::new();
    let encode = |source| ImageError::Encode {
        path: path.to_owned(),
        source,
    };
    match options.to {
        OutputFormat::Png => {
            let mut encoder = PngEncoder::new(&mut bytes);
            if let Some(icc) = icc {
                let _ = encoder.set_icc_profile(icc);
            }
            if let Some(exif) = exif {
                let _ = encoder.set_exif_metadata(exif);
            }
            img.write_with_encoder(encoder).map_err(encode)?;
        }
        OutputFormat::Jpeg => {
            let mut encoder = JpegEncoder::new_with_quality(&mut bytes, options.quality);
            if let Some(icc) = icc {
                let _ = encoder.set_icc_profile(icc);
            }
            if let Some(exif) = exif {
                let _ = encoder.set_exif_metadata(exif);
            }
            img.write_with_encoder(encoder).map_err(encode)?;
        }
    }
    Ok(Converted {
        bytes,
        from: info.format,
        width,
        height,
        notes,
    })
}

/// PNG keeps alpha and 16-bit channels; only float images need reducing.
fn for_png(img: DynamicImage, notes: &mut Vec<Problem>) -> DynamicImage {
    match img.color() {
        ColorType::Rgb32F => {
            notes.push(note(
                "bit-depth-reduced",
                "32-bit float channels stored as 16-bit",
            ));
            DynamicImage::ImageRgb16(img.to_rgb16())
        }
        ColorType::Rgba32F => {
            notes.push(note(
                "bit-depth-reduced",
                "32-bit float channels stored as 16-bit",
            ));
            DynamicImage::ImageRgba16(img.to_rgba16())
        }
        _ => img,
    }
}

/// JPEG has 8-bit channels and no alpha.
fn for_jpeg(
    img: DynamicImage,
    background: Option<[u8; 3]>,
    notes: &mut Vec<Problem>,
    path: &Path,
) -> Result<DynamicImage, ImageError> {
    let color = img.color();
    let bits = color.bits_per_pixel() / u16::from(color.channel_count());
    if bits > 8 {
        notes.push(note(
            "bit-depth-reduced",
            format!("{bits}-bit channels reduced to 8-bit (JPEG has no more)"),
        ));
    }
    if !color.has_alpha() {
        return Ok(match color {
            ColorType::L8 | ColorType::Rgb8 => img,
            ColorType::L16 => DynamicImage::ImageLuma8(img.to_luma8()),
            _ => DynamicImage::ImageRgb8(img.to_rgb8()),
        });
    }
    let rgba = img.to_rgba16();
    let transparent = rgba.pixels().filter(|p| p.0[3] != u16::MAX).count() as u64;
    if transparent == 0 {
        // An alpha channel that is fully opaque loses nothing.
        return Ok(match color {
            ColorType::La8 | ColorType::La16 => DynamicImage::ImageLuma8(img.to_luma8()),
            _ => DynamicImage::ImageRgb8(img.to_rgb8()),
        });
    }
    let Some(bg) = background else {
        return Err(ImageError::AlphaLost {
            path: path.to_owned(),
            pixels: transparent,
            total: u64::from(rgba.width()) * u64::from(rgba.height()),
        });
    };
    notes.push(note(
        "alpha-flattened",
        format!(
            "{transparent} transparent or translucent pixels flattened onto #{:02x}{:02x}{:02x}",
            bg[0], bg[1], bg[2]
        ),
    ));
    let max = u32::from(u16::MAX);
    let out = image::ImageBuffer::from_fn(rgba.width(), rgba.height(), |x, y| {
        let Rgba([r, g, b, a]) = *rgba.get_pixel(x, y);
        let a = u32::from(a);
        let mix = |c: u16, bg: u8| {
            let bg = u32::from(bg) * 257;
            let v = (u32::from(c) * a + bg * (max - a) + max / 2) / max;
            // 16-bit to 8-bit with rounding.
            ((v * 255 + max / 2) / max) as u8
        };
        image::Rgb([mix(r, bg[0]), mix(g, bg[1]), mix(b, bg[2])])
    });
    Ok(DynamicImage::ImageRgb8(out))
}

#[cfg(test)]
mod tests;
