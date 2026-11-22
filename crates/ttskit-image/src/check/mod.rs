//! `check`: finds images that are larger than the game can use well.
//!
//! The report comes from the file header (dimensions, format, colour layout);
//! only an image with an alpha channel is decoded, to see whether any pixel is
//! actually transparent. The limits and their sources are in
//! `docs/image-formats.md`.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use image::codecs::webp::WebPDecoder;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use serde::Serialize;

use crate::{ImageError, format_name};

/// Longest side `check` accepts when no limit is given. 4096 px is the
/// largest size observed to load without trouble in the game; 8192 px loaded
/// noticeably slowly (`docs/image-formats.md`). This is an observed practical
/// limit, not a documented hard limit of the game.
pub const DEFAULT_MAX_SIDE: u32 = 4096;

/// What `check` looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckOptions {
    /// Longest allowed side in pixels.
    pub max_side: u32,
    /// Largest allowed file size in bytes; no limit if `None`.
    pub max_bytes: Option<u64>,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            max_side: DEFAULT_MAX_SIDE,
            max_bytes: None,
        }
    }
}

/// One finding. `rule` is stable and meant for scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub rule: &'static str,
    pub message: String,
}

/// What `check` found out about one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckReport {
    pub path: String,
    /// Lower-case format name decided by content, e.g. `png`, `jpeg`.
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub file_bytes: u64,
    pub has_alpha: bool,
    pub findings: Vec<Finding>,
}

/// File extensions that match a format name (as `format_name` spells it).
fn extension_matches(format: &str, extension: &str) -> bool {
    let known: &[&str] = match format {
        "png" => &["png"],
        "jpeg" => &["jpg", "jpeg", "jpe", "jfif"],
        "webp" => &["webp"],
        "gif" => &["gif"],
        "bmp" => &["bmp"],
        "tiff" => &["tif", "tiff"],
        _ => return true, // a format ttskit never writes: nothing to compare with
    };
    known.contains(&extension.to_ascii_lowercase().as_str())
}

/// Whether any pixel of `img` has an alpha value below the maximum.
fn has_translucent_pixel(img: &DynamicImage) -> bool {
    match img {
        DynamicImage::ImageRgba8(b) => b.pixels().any(|p| p.0[3] != u8::MAX),
        DynamicImage::ImageLumaA8(b) => b.pixels().any(|p| p.0[1] != u8::MAX),
        DynamicImage::ImageRgba16(b) => b.pixels().any(|p| p.0[3] != u16::MAX),
        DynamicImage::ImageLumaA16(b) => b.pixels().any(|p| p.0[1] != u16::MAX),
        other => other.to_rgba32f().pixels().any(|p| p.0[3] < 1.0),
    }
}

/// Checks the image at `path` against `options`. Only a file that cannot be
/// read, or whose format is unknown, is an `Err`; everything else is a
/// finding.
pub fn check(path: &Path, options: &CheckOptions) -> Result<CheckReport, ImageError> {
    let io = |source| ImageError::Io {
        path: path.to_owned(),
        source,
    };
    let decode = |source| ImageError::Decode {
        path: path.to_owned(),
        source,
    };
    let file_bytes = std::fs::metadata(path).map_err(io)?.len();
    let file = File::open(path).map_err(io)?;
    let reader = ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .map_err(io)?;
    let format = reader.format().ok_or_else(|| ImageError::UnknownFormat {
        path: path.to_owned(),
    })?;

    // Header only: dimensions and colour layout, no pixels.
    let (width, height, has_alpha, animated_webp) = if format == ImageFormat::WebP {
        let decoder = WebPDecoder::new(reader.into_inner()).map_err(decode)?;
        let (w, h) = decoder.dimensions();
        (
            w,
            h,
            decoder.color_type().has_alpha(),
            decoder.has_animation(),
        )
    } else {
        let decoder = reader.into_decoder().map_err(decode)?;
        let (w, h) = decoder.dimensions();
        (w, h, decoder.color_type().has_alpha(), false)
    };
    let name = format_name(format);

    let mut findings = Vec::new();
    if width.max(height) > options.max_side {
        findings.push(Finding {
            rule: "side-too-long",
            message: format!(
                "{width}x{height}: the longest side is over the limit of {} px",
                options.max_side
            ),
        });
    }
    if let Some(limit) = options.max_bytes
        && file_bytes > limit
    {
        findings.push(Finding {
            rule: "file-too-large",
            message: format!("{file_bytes} bytes: the file is over the limit of {limit} bytes"),
        });
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(extension) if extension_matches(&name, extension) => {}
        Some(extension) => findings.push(Finding {
            rule: "extension-mismatch",
            message: format!("the extension is .{extension}, but the content is {name}"),
        }),
        None => findings.push(Finding {
            rule: "extension-mismatch",
            message: format!("the file has no extension, but the content is {name}"),
        }),
    }
    // An alpha channel needs a pixel scan. Animated WebP is not decoded: the
    // game rejects it anyway (`image info` says so).
    let alpha_scan = has_alpha && matches!(format, ImageFormat::Png | ImageFormat::WebP);
    if alpha_scan && !animated_webp {
        let file = File::open(path).map_err(io)?;
        let img = ImageReader::with_format(BufReader::new(file), format)
            .decode()
            .map_err(decode)?;
        if !has_translucent_pixel(&img) {
            findings.push(Finding {
                rule: "alpha-opaque",
                message: "the alpha channel is fully opaque; the file would be smaller without it (try `ttskit image optimize`)"
                    .to_owned(),
            });
        }
    }
    Ok(CheckReport {
        path: path.display().to_string(),
        format: name,
        width,
        height,
        file_bytes,
        has_alpha,
        findings,
    })
}

#[cfg(test)]
mod tests;
