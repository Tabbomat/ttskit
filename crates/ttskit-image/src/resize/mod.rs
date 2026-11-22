//! Image resizing. PNG and JPEG in, the same format out. The aspect ratio is
//! kept, an image is never enlarged unless asked, and the output is
//! deterministic: the same input and options give the same bytes.

use std::io::Cursor;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{ColorType, DynamicImage, ImageDecoder, ImageFormat, ImageReader, Rgba32FImage};

use crate::optimize::{PNG_SIGNATURE, Skipped, is_animated};
use crate::{DEFAULT_JPEG_QUALITY, ImageError, Problem, Severity, format_name};

/// Largest side a resize may produce (the JPEG limit).
pub const MAX_RESULT_SIDE: u32 = 65_535;

/// How the new size is chosen. The aspect ratio is always kept.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// The longer side becomes this many pixels.
    MaxSide(u32),
    /// The width becomes this many pixels.
    Width(u32),
    /// The height becomes this many pixels.
    Height(u32),
    /// Both sides are multiplied by this factor.
    Scale(f64),
}

/// The resampling filter. All of them are deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// Sharp, the best default for shrinking.
    Lanczos3,
    CatmullRom,
    /// Bilinear.
    Triangle,
    /// No smoothing; for pixel art.
    Nearest,
}

impl Filter {
    fn kind(self) -> FilterType {
        match self {
            Self::Lanczos3 => FilterType::Lanczos3,
            Self::CatmullRom => FilterType::CatmullRom,
            Self::Triangle => FilterType::Triangle,
            Self::Nearest => FilterType::Nearest,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResizeOptions {
    pub target: Target,
    /// Allow a result larger than the input. Off by default.
    pub allow_enlarge: bool,
    pub filter: Filter,
    /// JPEG quality, 1 to 100. Ignored for PNG.
    pub jpeg_quality: u8,
}

impl ResizeOptions {
    pub fn new(target: Target) -> Self {
        Self {
            target,
            allow_enlarge: false,
            filter: Filter::Lanczos3,
            jpeg_quality: DEFAULT_JPEG_QUALITY,
        }
    }
}

/// An image that was resized.
#[derive(Debug, Clone)]
pub struct Resized {
    pub bytes: Vec<u8>,
    /// `png` or `jpeg`, the same as the input.
    pub format: String,
    pub original_width: u32,
    pub original_height: u32,
    pub width: u32,
    pub height: u32,
    pub original_bytes: u64,
    /// What was changed or lost on the way. Codes are stable.
    pub notes: Vec<Problem>,
}

/// An image that is left as it is on purpose. `code` is stable:
/// `already-that-size` or `would-enlarge`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unchanged {
    pub code: &'static str,
    pub message: String,
    pub width: u32,
    pub height: u32,
    pub original_bytes: u64,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Resized(Resized),
    Unchanged(Unchanged),
    Skipped(Skipped),
}

/// The size `target` gives for an image of `width` x `height`: the aspect
/// ratio is kept, each side is at least 1 and rounded to the nearest pixel.
pub fn target_size(width: u32, height: u32, target: Target) -> Result<(u32, u32), ImageError> {
    let bad = |m: String| ImageError::Options(m);
    // `round(value * to / from)`, in integers so no platform can differ.
    let scaled = |value: u32, to: u32, from: u32| -> u32 {
        let (value, to, from) = (u64::from(value), u64::from(to), u64::from(from));
        let rounded = (value * to + from / 2) / from;
        u32::try_from(rounded.max(1)).unwrap_or(u32::MAX)
    };
    let (w, h) = match target {
        Target::MaxSide(n) | Target::Width(n) | Target::Height(n) if n == 0 => {
            return Err(bad("the target size must be at least 1 pixel".to_owned()));
        }
        Target::MaxSide(n) => {
            let long = width.max(height);
            (scaled(width, n, long), scaled(height, n, long))
        }
        Target::Width(n) => (n, scaled(height, n, width)),
        Target::Height(n) => (scaled(width, n, height), n),
        Target::Scale(f) => {
            if !f.is_finite() || f <= 0.0 {
                return Err(bad(format!("the scale must be a positive number, not {f}")));
            }
            let side = |v: u32| -> u32 {
                let r = (f64::from(v) * f).round().max(1.0);
                if r > f64::from(u32::MAX) {
                    u32::MAX
                } else {
                    r as u32
                }
            };
            (side(width), side(height))
        }
    };
    if w > MAX_RESULT_SIDE || h > MAX_RESULT_SIDE {
        return Err(bad(format!(
            "the result would be {w}x{h}; the longest side may be at most {MAX_RESULT_SIDE} px"
        )));
    }
    Ok((w, h))
}

fn note(code: &'static str, message: impl Into<String>) -> Problem {
    Problem::new(Severity::Note, code, message)
}

/// Resizes `img` with straight alpha handled correctly: colour is weighted by
/// alpha while sampling, so transparent pixels do not bleed into the edge.
pub(crate) fn resize_pixels(
    img: &DynamicImage,
    w: u32,
    h: u32,
    filter: FilterType,
) -> DynamicImage {
    let color = img.color();
    if !color.has_alpha() {
        return img.resize_exact(w, h, filter);
    }
    let wide = matches!(color, ColorType::La16 | ColorType::Rgba16);
    let mut buffer: Rgba32FImage = img.to_rgba32f();
    for p in buffer.pixels_mut() {
        let a = p.0[3];
        p.0[0] *= a;
        p.0[1] *= a;
        p.0[2] *= a;
    }
    let mut out = image::imageops::resize(&buffer, w, h, filter);
    for p in out.pixels_mut() {
        let a = p.0[3].clamp(0.0, 1.0);
        if a > 0.0 {
            p.0[0] = (p.0[0] / a).clamp(0.0, 1.0);
            p.0[1] = (p.0[1] / a).clamp(0.0, 1.0);
            p.0[2] = (p.0[2] / a).clamp(0.0, 1.0);
        }
        p.0[3] = a;
    }
    let out = DynamicImage::ImageRgba32F(out);
    match (color, wide) {
        (ColorType::La8, _) => DynamicImage::ImageLumaA8(out.to_luma_alpha8()),
        (ColorType::La16, _) => DynamicImage::ImageLumaA16(out.to_luma_alpha16()),
        (_, true) => DynamicImage::ImageRgba16(out.to_rgba16()),
        _ => DynamicImage::ImageRgba8(out.to_rgba8()),
    }
}

/// Resizes the PNG or JPEG at `path` in memory; nothing is written. Any
/// other format (decided by content) is skipped, not an error. An animated
/// PNG is resized by its first frame. The colour profile and EXIF data are
/// dropped, as `image convert` does; an EXIF orientation is applied first.
pub fn resize(path: &Path, options: &ResizeOptions) -> Result<Outcome, ImageError> {
    if !(1..=100).contains(&options.jpeg_quality) {
        return Err(ImageError::Options(format!(
            "quality must be between 1 and 100, not {}",
            options.jpeg_quality
        )));
    }
    let io = |source| ImageError::Io {
        path: path.to_owned(),
        source,
    };
    let decode = |source| ImageError::Decode {
        path: path.to_owned(),
        source,
    };
    let data = std::fs::read(path).map_err(io)?;
    let format = image::guess_format(&data).map_err(|_| ImageError::UnknownFormat {
        path: path.to_owned(),
    })?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg) {
        return Ok(Outcome::Skipped(Skipped {
            code: "unsupported-format",
            message: format!(
                "{}: only png and jpeg are resized; run `ttskit image convert` first",
                format_name(format)
            ),
        }));
    }
    let original_bytes = data.len() as u64;
    let mut notes = Vec::new();
    if format == ImageFormat::Png && data.starts_with(&PNG_SIGNATURE) && is_animated(&data) {
        notes.push(Problem::new(
            Severity::Warning,
            "animated-first-frame",
            "animated png: only the first frame is resized",
        ));
    }

    let mut decoder = ImageReader::with_format(Cursor::new(&data), format)
        .into_decoder()
        .map_err(decode)?;
    let icc = decoder.icc_profile().map_err(decode)?;
    let exif = decoder.exif_metadata().map_err(decode)?;
    let orientation = decoder.orientation().map_err(decode)?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(decode)?;
    if orientation != image::metadata::Orientation::NoTransforms {
        img.apply_orientation(orientation);
        notes.push(note(
            "orientation-applied",
            "EXIF orientation applied to the pixels",
        ));
    }
    if let Some(profile) = &icc {
        notes.push(note(
            "icc-dropped",
            format!(
                "colour profile ({} bytes) dropped; the game ignores it",
                profile.len()
            ),
        ));
    }
    if exif.is_some() {
        notes.push(note("exif-dropped", "EXIF data dropped"));
    }

    let (width, height) = (img.width(), img.height());
    let (new_w, new_h) = target_size(width, height, options.target)?;
    let unchanged = |code: &'static str, message: String| {
        Ok(Outcome::Unchanged(Unchanged {
            code,
            message,
            width,
            height,
            original_bytes,
        }))
    };
    if (new_w, new_h) == (width, height) {
        return unchanged(
            "already-that-size",
            format!("already {width}x{height}; nothing to do"),
        );
    }
    if (new_w > width || new_h > height) && !options.allow_enlarge {
        return unchanged(
            "would-enlarge",
            format!(
                "{width}x{height} would become {new_w}x{new_h}, which is larger; pass --allow-enlarge to do that"
            ),
        );
    }

    let img = resize_pixels(&img, new_w, new_h, options.filter.kind());
    let encode = |source| ImageError::Encode {
        path: path.to_owned(),
        source,
    };
    let mut bytes = Vec::new();
    match format {
        ImageFormat::Png => {
            let img = match img.color() {
                ColorType::Rgb32F => DynamicImage::ImageRgb16(img.to_rgb16()),
                ColorType::Rgba32F => DynamicImage::ImageRgba16(img.to_rgba16()),
                _ => img,
            };
            img.write_with_encoder(PngEncoder::new(&mut bytes))
                .map_err(encode)?;
        }
        _ => {
            // A JPEG decodes to 8-bit gray or RGB, which the encoder takes as is.
            img.write_with_encoder(JpegEncoder::new_with_quality(
                &mut bytes,
                options.jpeg_quality,
            ))
            .map_err(encode)?;
        }
    }
    Ok(Outcome::Resized(Resized {
        bytes,
        format: format_name(format),
        original_width: width,
        original_height: height,
        width: new_w,
        height: new_h,
        original_bytes,
        notes,
    }))
}

#[cfg(test)]
mod tests;
