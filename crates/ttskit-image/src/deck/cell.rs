//! Reading a card and fitting it into a cell of the sheet.

use std::io::Cursor;
use std::path::Path;

use image::codecs::webp::WebPDecoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Rgba, RgbaImage};

use crate::optimize::{PNG_SIGNATURE, is_animated};
use crate::resize::resize_pixels;
use crate::{ImageError, Problem, Severity, format_name};

/// How a card that is not the size of the cell is fitted into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Scaled to lie inside the cell, aspect ratio kept, centred on the fill colour.
    Contain,
    /// Scaled to cover the cell, aspect ratio kept, centre-cropped.
    Cover,
    /// Resized to exactly the cell, whatever the aspect ratio.
    Stretch,
}

fn note(code: &'static str, path: &Path, message: &str) -> Problem {
    Problem::new(
        Severity::Note,
        code,
        format!("{}: {message}", path.display()),
    )
}

/// Reads a PNG, JPEG or WebP file with its colour type and bit depth. The
/// EXIF orientation is applied to the pixels; the colour profile and EXIF data
/// are dropped; an animated image gives its first frame. Each change is a note
/// in `notes`.
pub(super) fn decode_keeping_depth(
    path: &Path,
    notes: &mut Vec<Problem>,
) -> Result<DynamicImage, ImageError> {
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
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    ) {
        return Err(ImageError::Rejected {
            path: path.to_owned(),
            format: format_name(format),
        });
    }
    let animated = match format {
        ImageFormat::Png => data.starts_with(&PNG_SIGNATURE) && is_animated(&data),
        ImageFormat::WebP => {
            WebPDecoder::new(Cursor::new(&data)).is_ok_and(|decoder| decoder.has_animation())
        }
        _ => false,
    };
    if animated {
        notes.push(Problem::new(
            Severity::Warning,
            "animated-first-frame",
            format!(
                "{}: animated image, only the first frame is used",
                path.display()
            ),
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
            path,
            "EXIF orientation applied to the pixels",
        ));
    }
    if let Some(profile) = &icc {
        notes.push(note(
            "icc-dropped",
            path,
            &format!(
                "colour profile ({} bytes) dropped; the game ignores it",
                profile.len()
            ),
        ));
    }
    if exif.is_some() {
        notes.push(note("exif-dropped", path, "EXIF data dropped"));
    }
    Ok(img)
}

/// True when a channel of `img` has more than 8 bits.
pub(super) fn is_deep(img: &DynamicImage) -> bool {
    let color = img.color();
    color.bits_per_pixel() / u16::from(color.channel_count()) > 8
}

/// Reads a PNG, JPEG or WebP file as 8-bit RGBA (see [`decode_keeping_depth`]).
pub(super) fn decode(path: &Path, notes: &mut Vec<Problem>) -> Result<RgbaImage, ImageError> {
    let img = decode_keeping_depth(path, notes)?;
    if is_deep(&img) {
        notes.push(note(
            "reduced-to-8-bit",
            path,
            "channels reduced to 8 bits for the sheet",
        ));
    }
    Ok(img.into_rgba8())
}

/// Resamples to `w` x `h` with Lanczos3 and premultiplied alpha.
fn resample(img: RgbaImage, w: u32, h: u32) -> RgbaImage {
    if img.dimensions() == (w, h) {
        return img;
    }
    resize_pixels(&DynamicImage::ImageRgba8(img), w, h, FilterType::Lanczos3).into_rgba8()
}

/// `value * to / from`, rounded to the nearest integer, at least 1.
fn scaled(value: u32, to: u32, from: u32) -> u32 {
    let (value, to, from) = (u64::from(value), u64::from(to), u64::from(from));
    u32::try_from(((value * to + from / 2) / from).max(1)).unwrap_or(u32::MAX)
}

/// Brings `img` to the size of the cell. An image of that size is returned
/// untouched. Without a `fit` the image is resized to the cell (it already
/// had the card size, the cell is smaller by `--max-sheet-side`).
pub(super) fn fit_cell(
    img: RgbaImage,
    cell: (u32, u32),
    fit: Option<Fit>,
    fill: [u8; 3],
) -> RgbaImage {
    let (cw, ch) = cell;
    let (w, h) = img.dimensions();
    if (w, h) == cell {
        return img;
    }
    match fit {
        None | Some(Fit::Stretch) => resample(img, cw, ch),
        Some(Fit::Contain) => {
            // The side that limits the scale reaches the cell exactly.
            let (nw, nh) = if u64::from(cw) * u64::from(h) <= u64::from(ch) * u64::from(w) {
                (cw, scaled(h, cw, w).min(ch))
            } else {
                (scaled(w, ch, h).min(cw), ch)
            };
            let inner = resample(img, nw, nh);
            let mut out = RgbaImage::from_pixel(cw, ch, Rgba([fill[0], fill[1], fill[2], 255]));
            image::imageops::replace(
                &mut out,
                &inner,
                i64::from((cw - nw) / 2),
                i64::from((ch - nh) / 2),
            );
            out
        }
        Some(Fit::Cover) => {
            let (nw, nh) = if u64::from(cw) * u64::from(h) >= u64::from(ch) * u64::from(w) {
                (cw, scaled(h, cw, w).max(ch))
            } else {
                (scaled(w, ch, h).max(cw), ch)
            };
            let big = resample(img, nw, nh);
            image::imageops::crop_imm(&big, (nw - cw) / 2, (nh - ch) / 2, cw, ch).to_image()
        }
    }
}
