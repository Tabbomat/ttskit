//! Image inspection for ttskit (unofficial Tabletop Simulator tooling).
//!
//! `inspect` reports what an image file contains and whether Tabletop
//! Simulator can use it, with the reason when it cannot. The rules come from
//! loading generated test images in the game; they are recorded in
//! `docs/image-formats.md`.

mod check;
mod convert;
mod deck;
mod optimize;
pub mod pipeline;
mod quantize;
mod resize;

pub use check::{CheckOptions, CheckReport, DEFAULT_MAX_SIDE, Finding, check};
pub use convert::{
    ConvertOptions, Converted, DEFAULT_JPEG_QUALITY, OutputFormat, convert, convert_bytes,
    parse_color,
};
pub use deck::{
    CardDescription, DESCRIPTION_VERSION, Deck, DeckOptions, Description, Fit, MAX_COLUMNS,
    MAX_ROWS, MAX_SHEET_SIDE, PlacedCard, Sheet, SheetDescription, SheetPlan, SlotPlan, Split,
    SplitOptions, SplitPlan, SplitSlot, build_deck, expand_card_inputs, grid_for, natural_cmp,
    plan_sheets, plan_split, read_card_list, slot_origin, split_sheet,
};
pub use optimize::{
    DEFAULT_LEVEL, MAX_LEVEL, OptimizeOptions, Optimized, Outcome, Skipped, StripMode, optimize,
    optimize_bytes,
};
pub use pipeline::{parse_card_size, parse_dither, parse_quality, parse_scale};
pub use quantize::{
    DEFAULT_COLORS, Engine, MAX_COLORS, MIN_COLORS, QuantizeOptions, QuantizeOutcome, Quantized,
    pngquant_version, quantize, quantize_bytes,
};
pub use resize::{
    Filter, MAX_RESULT_SIDE, Outcome as ResizeOutcome, ResizeOptions, Resized,
    Target as ResizeTarget, Unchanged, resize, resize_bytes, target_size,
};

use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, ColorType, ImageDecoder, ImageFormat, ImageReader};
use serde::Serialize;

/// Extensions `collect_inputs` picks up when scanning a directory.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"];

/// Formats the game loads (`docs/image-formats.md`).
const GAME_FORMATS: &str = "png, jpeg, webp";

/// Longest side from which loading is noticeably slow in the game (8192 px
/// was slow, 4096 px was fine).
const SLOW_SIDE: u32 = 8192;

/// The opaque pixels of a token image must span at least this many pixels in
/// both directions (1 px wide or tall failed in the game, 2 px worked).
const MIN_TOKEN_SPAN: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: unsupported or unrecognised image format", path.display())]
    UnknownFormat { path: PathBuf },
    #[error("{}: {format} is not an image format the game accepts (it accepts {GAME_FORMATS})", path.display())]
    Rejected { path: PathBuf, format: String },
    #[error("{}: {source}", path.display())]
    Decode {
        path: PathBuf,
        source: image::ImageError,
    },
    #[error("{}: could not encode the result: {source}", path.display())]
    Encode {
        path: PathBuf,
        source: image::ImageError,
    },
    #[error(
        "{}: {pixels} of {total} pixels are transparent or translucent and JPEG has no alpha channel; pass --background COLOR to flatten them onto a colour, or convert to png",
        path.display()
    )]
    AlphaLost {
        path: PathBuf,
        pixels: u64,
        total: u64,
    },
    #[error("{}: {message}", path.display())]
    Optimize { path: PathBuf, message: String },
    #[error("{0}")]
    Options(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The game rejects the file or fails to use it.
    Error,
    /// The game loads it, but not as the file suggests, or only slowly.
    Warning,
    /// Good to know; nothing is wrong.
    Note,
}

/// One finding about a file. `code` is stable and meant for scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
}

impl Problem {
    fn new(severity: Severity, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
        }
    }
}

/// Pixel counts by alpha value, for images with an alpha channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AlphaStats {
    /// Alpha 255.
    pub opaque: u64,
    /// Alpha between 1 and 254.
    pub translucent: u64,
    /// Alpha 0.
    pub transparent: u64,
    /// Width and height of the bounding box of the opaque pixels (0 if none).
    pub opaque_width: u32,
    pub opaque_height: u32,
}

/// What a file contains. Stable, serialised by `ttskit image info --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageInfo {
    pub path: String,
    /// Lower-case format name, e.g. `png`, `jpeg`, `webp`.
    pub format: String,
    pub width: u32,
    pub height: u32,
    /// Channel layout, e.g. `rgba`, `rgb`, `luma`, `luma+alpha`, `cmyk`.
    pub color: String,
    /// Bits per channel (8 or 16 for the formats handled here).
    pub bit_depth: u16,
    pub has_alpha: bool,
    /// Number of frames; 1 for a still image.
    pub frames: u32,
    pub animated: bool,
    /// Size of the embedded ICC colour profile in bytes, if any.
    pub icc_profile_bytes: Option<usize>,
    /// Size of the file in bytes.
    pub file_bytes: u64,
    /// Present for PNG and WebP images with an alpha channel.
    pub alpha: Option<AlphaStats>,
    /// Whether the game can use the file: false if any problem is an error.
    pub compatible: bool,
    pub problems: Vec<Problem>,
}

fn format_name(format: ImageFormat) -> String {
    match format {
        ImageFormat::Jpeg => "jpeg".to_owned(),
        ImageFormat::Tiff => "tiff".to_owned(),
        other => other
            .extensions_str()
            .first()
            .copied()
            .unwrap_or("unknown")
            .to_owned(),
    }
}

fn color_layout(color: ColorType) -> (&'static str, u16, bool) {
    match color {
        ColorType::L8 => ("luma", 8, false),
        ColorType::La8 => ("luma+alpha", 8, true),
        ColorType::Rgb8 => ("rgb", 8, false),
        ColorType::Rgba8 => ("rgba", 8, true),
        ColorType::L16 => ("luma", 16, false),
        ColorType::La16 => ("luma+alpha", 16, true),
        ColorType::Rgb16 => ("rgb", 16, false),
        ColorType::Rgba16 => ("rgba", 16, true),
        ColorType::Rgb32F => ("rgb", 32, false),
        ColorType::Rgba32F => ("rgba", 32, true),
        _ => ("unknown", 0, false),
    }
}

/// Width, height and component count from the first start-of-frame marker of
/// a JPEG. The image decoder converts CMYK to RGB, so the header is the only
/// place that tells them apart.
fn jpeg_frame_header(reader: &mut impl Read) -> Option<(u32, u32, u8)> {
    fn byte(r: &mut impl Read) -> Option<u8> {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).ok().map(|()| b[0])
    }
    if (byte(reader)?, byte(reader)?) != (0xFF, 0xD8) {
        return None;
    }
    loop {
        // Find the next marker: 0xFF followed by a byte that is not 0xFF.
        let mut b = byte(reader)?;
        while b != 0xFF {
            b = byte(reader)?;
        }
        let mut marker = byte(reader)?;
        while marker == 0xFF {
            marker = byte(reader)?;
        }
        if marker == 0 || marker == 0x01 || (0xD0..=0xD8).contains(&marker) {
            continue; // stuffed byte, TEM, RSTn, SOI: no length field
        }
        if marker == 0xD9 || marker == 0xDA {
            return None; // end of image or start of scan before any frame header
        }
        let len = u16::from_be_bytes([byte(reader)?, byte(reader)?]) as usize;
        let mut body = vec![0u8; len.checked_sub(2)?];
        reader.read_exact(&mut body).ok()?;
        let is_sof = (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker);
        if is_sof && body.len() >= 6 {
            let height = u16::from_be_bytes([body[1], body[2]]) as u32;
            let width = u16::from_be_bytes([body[3], body[4]]) as u32;
            return Some((width, height, body[5]));
        }
    }
}

fn alpha_stats(data: &[u8], format: ImageFormat) -> Result<AlphaStats, image::ImageError> {
    let img = ImageReader::with_format(Cursor::new(data), format).decode()?;
    let mut stats = AlphaStats {
        opaque: 0,
        translucent: 0,
        transparent: 0,
        opaque_width: 0,
        opaque_height: 0,
    };
    // Bounding box of the opaque pixels: (min x, min y, max x, max y).
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for (x, y, pixel) in img.to_rgba8().enumerate_pixels() {
        match pixel.0[3] {
            255 => {
                stats.opaque += 1;
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
            0 => stats.transparent += 1,
            _ => stats.translucent += 1,
        }
    }
    if let Some((x0, y0, x1, y1)) = bounds {
        stats.opaque_width = x1 - x0 + 1;
        stats.opaque_height = y1 - y0 + 1;
    }
    Ok(stats)
}

/// Reads `path` and reports what it contains and whether the game can use it.
///
/// Only a file that cannot be read at all, or whose format neither the game
/// nor this crate knows, is an `Err`; every other finding is a `Problem` in
/// the result.
pub fn inspect(path: &Path) -> Result<ImageInfo, ImageError> {
    let data = std::fs::read(path).map_err(|source| ImageError::Io {
        path: path.to_owned(),
        source,
    })?;
    inspect_bytes(path, &data)
}

/// Like [`inspect`], for image data already in memory. `label` is only used
/// in the result and in error messages; it is never opened.
pub fn inspect_bytes(label: &Path, data: &[u8]) -> Result<ImageInfo, ImageError> {
    let path = label;
    let io = |source| ImageError::Io {
        path: path.to_owned(),
        source,
    };
    let decode = |source| ImageError::Decode {
        path: path.to_owned(),
        source,
    };
    let file_bytes = data.len() as u64;
    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(io)?;
    let format = reader.format().ok_or_else(|| ImageError::UnknownFormat {
        path: path.to_owned(),
    })?;
    let mut problems = Vec::new();
    let name = format_name(format);
    match format {
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP => {}
        ImageFormat::Gif | ImageFormat::Bmp | ImageFormat::Tiff => problems.push(Problem::new(
            Severity::Error,
            "format-rejected",
            format!("the game does not load {name} (it loads {GAME_FORMATS})"),
        )),
        _ => {
            return Err(ImageError::Rejected {
                path: path.to_owned(),
                format: name,
            });
        }
    }
    let mut inner = reader.into_inner();
    inner.rewind().map_err(io)?;

    if format == ImageFormat::Jpeg {
        if let Some((width, height, 4)) = jpeg_frame_header(&mut inner) {
            problems.push(Problem::new(
                Severity::Error,
                "jpeg-cmyk",
                "CMYK JPEG: the game fails to decode it; convert it to RGB",
            ));
            return Ok(finish(
                path,
                name,
                (width, height),
                ("cmyk", 8, false),
                1,
                None,
                file_bytes,
                None,
                problems,
            ));
        }
        inner.rewind().map_err(io)?;
    }

    let (width, height, color, icc, frames) = match format {
        ImageFormat::Png => {
            let mut decoder = PngDecoder::new(inner).map_err(decode)?;
            let (w, h) = decoder.dimensions();
            let color = decoder.color_type();
            let icc = decoder.icc_profile();
            let frames = if decoder.is_apng().map_err(decode)? {
                decoder.apng().map_err(decode)?.into_frames().count() as u32
            } else {
                1
            };
            (w, h, color, icc, frames)
        }
        ImageFormat::Gif => {
            let decoder = GifDecoder::new(inner).map_err(decode)?;
            let (w, h) = decoder.dimensions();
            let color = decoder.color_type();
            let frames = decoder.into_frames().count() as u32;
            (w, h, color, Ok(None), frames.max(1))
        }
        ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(inner).map_err(decode)?;
            let (w, h) = decoder.dimensions();
            let color = decoder.color_type();
            let icc = decoder.icc_profile();
            let frames = if decoder.has_animation() {
                (decoder.into_frames().count() as u32).max(1)
            } else {
                1
            };
            (w, h, color, icc, frames)
        }
        _ => {
            let mut decoder = image::ImageReader::with_format(inner, format)
                .into_decoder()
                .map_err(decode)?;
            let (w, h) = decoder.dimensions();
            (w, h, decoder.color_type(), decoder.icc_profile(), 1)
        }
    };
    let layout = color_layout(color);
    let icc = icc.map_err(decode)?.map(|p| p.len());
    let animated = frames > 1;

    if animated && format == ImageFormat::WebP {
        problems.push(Problem::new(
            Severity::Error,
            "webp-animated",
            "animated WebP: the game fails to decode it; use a still image",
        ));
    } else if animated && format == ImageFormat::Png {
        problems.push(Problem::new(
            Severity::Warning,
            "png-animated",
            format!("animated PNG ({frames} frames): the game shows only a still image"),
        ));
    }
    if let Some(bytes) = icc {
        problems.push(Problem::new(
            Severity::Warning,
            "icc-ignored",
            format!("embedded colour profile ({bytes} bytes): the game ignores it, so colours may differ from other viewers"),
        ));
    }
    if width.max(height) >= SLOW_SIDE {
        problems.push(Problem::new(
            Severity::Warning,
            "very-large",
            format!("{width}x{height}: images of {SLOW_SIDE} px or more load slowly in the game"),
        ));
    }
    let checks_alpha = layout.2
        && matches!(format, ImageFormat::Png | ImageFormat::WebP)
        && !(animated && format == ImageFormat::WebP);
    let alpha = if checks_alpha {
        match alpha_stats(data, format) {
            Ok(stats) => Some(stats),
            Err(error) => {
                problems.push(Problem::new(
                    Severity::Warning,
                    "alpha-unchecked",
                    format!("could not check the alpha channel: {error}"),
                ));
                None
            }
        }
    } else {
        None
    };
    if let Some(stats) = alpha {
        if stats.opaque == 0 {
            problems.push(Problem::new(
                Severity::Warning,
                "token-no-opaque-pixel",
                "no fully opaque pixel: the game cannot make a token from it (tiles and other objects work)",
            ));
        } else if stats.opaque_width < MIN_TOKEN_SPAN || stats.opaque_height < MIN_TOKEN_SPAN {
            problems.push(Problem::new(
                Severity::Warning,
                "token-opaque-too-small",
                format!(
                    "the fully opaque pixels span only {}x{} px: the game cannot make a token from it (it needs at least {MIN_TOKEN_SPAN} px in both directions)",
                    stats.opaque_width, stats.opaque_height
                ),
            ));
        } else if stats.translucent > 0 {
            problems.push(Problem::new(
                Severity::Note,
                "token-translucent-pixels",
                format!(
                    "{} translucent pixels: a token outline follows only fully opaque pixels",
                    stats.translucent
                ),
            ));
        }
    }
    Ok(finish(
        path,
        name,
        (width, height),
        layout,
        frames,
        icc,
        file_bytes,
        alpha,
        problems,
    ))
}

#[allow(clippy::too_many_arguments)]
fn finish(
    path: &Path,
    format: String,
    (width, height): (u32, u32),
    (layout, bit_depth, has_alpha): (&str, u16, bool),
    frames: u32,
    icc_profile_bytes: Option<usize>,
    file_bytes: u64,
    alpha: Option<AlphaStats>,
    problems: Vec<Problem>,
) -> ImageInfo {
    ImageInfo {
        path: path.display().to_string(),
        format,
        width,
        height,
        color: layout.to_owned(),
        bit_depth,
        has_alpha,
        frames,
        animated: frames > 1,
        icc_profile_bytes,
        file_bytes,
        alpha,
        compatible: !problems.iter().any(|p| p.severity == Severity::Error),
        problems,
    }
}

/// Expands command line inputs: files are kept as given, directories are
/// scanned recursively for files with an image extension. The result is
/// sorted by path, so batch output is deterministic.
pub fn collect_inputs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, ImageError> {
    let mut found = Vec::new();
    for input in inputs {
        if input.is_dir() {
            scan_dir(input, &mut found)?;
        } else {
            found.push(input.clone());
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

fn scan_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ImageError> {
    let io = |source| ImageError::Io {
        path: dir.to_owned(),
        source,
    };
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        if path.is_dir() {
            scan_dir(&path, out)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame, ImageBuffer, Rgb, Rgba};
    use std::fs::File;

    fn rgba(w: u32, h: u32) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        ImageBuffer::from_fn(w, h, |x, y| Rgba([x as u8, y as u8, 7, 200]))
    }

    #[test]
    fn png_rgba() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        rgba(5, 3).save(&path).unwrap();
        let info = inspect(&path).unwrap();
        assert_eq!(
            (info.format.as_str(), info.width, info.height),
            ("png", 5, 3)
        );
        assert_eq!(info.color, "rgba");
        assert_eq!(info.bit_depth, 8);
        assert!(info.has_alpha && !info.animated);
        assert_eq!(info.frames, 1);
        assert_eq!(info.icc_profile_bytes, None);
        assert_eq!(info.file_bytes, std::fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn inspect_bytes_matches_inspect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        rgba(5, 3).save(&path).unwrap();
        let data = std::fs::read(&path).unwrap();
        let from_path = inspect(&path).unwrap();
        let from_bytes = inspect_bytes(&path, &data).unwrap();
        assert_eq!(from_bytes, from_path);
        // The label is never opened.
        let label = dir.path().join("label.png");
        let labelled = inspect_bytes(&label, &data).unwrap();
        assert_eq!(labelled.path, label.display().to_string());
        assert_eq!(labelled.file_bytes, data.len() as u64);
        let error = inspect_bytes(&label, b"not an image").unwrap_err();
        assert!(error.to_string().contains("label.png"), "{error}");
    }

    #[test]
    fn png_16_bit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        ImageBuffer::<Rgb<u16>, Vec<u16>>::from_pixel(2, 2, Rgb([1000, 2000, 3000]))
            .save(&path)
            .unwrap();
        let info = inspect(&path).unwrap();
        assert_eq!((info.color.as_str(), info.bit_depth), ("rgb", 16));
        assert!(!info.has_alpha);
    }

    #[test]
    fn jpeg_and_lossless_webp() {
        let dir = tempfile::tempdir().unwrap();
        let jpg = dir.path().join("a.jpg");
        image::DynamicImage::from(rgba(8, 8).clone())
            .to_rgb8()
            .save(&jpg)
            .unwrap();
        assert_eq!(inspect(&jpg).unwrap().format, "jpeg");
        let webp = dir.path().join("a.webp");
        rgba(4, 4).save(&webp).unwrap();
        let info = inspect(&webp).unwrap();
        assert_eq!((info.format.as_str(), info.frames), ("webp", 1));
        assert!(!info.animated);
    }

    #[test]
    fn animated_gif_counts_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.gif");
        let mut encoder = GifEncoder::new(File::create(&path).unwrap());
        for shade in [10u8, 200] {
            let buf = ImageBuffer::from_pixel(3, 3, Rgba([shade, shade, shade, 255]));
            encoder
                .encode_frame(Frame::from_parts(
                    buf,
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
        drop(encoder);
        let info = inspect(&path).unwrap();
        assert_eq!((info.format.as_str(), info.frames), ("gif", 2));
        assert!(info.animated);
    }

    #[test]
    fn unknown_format_and_missing_file_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("a.png");
        std::fs::write(&text, b"not an image").unwrap();
        assert!(matches!(
            inspect(&text),
            Err(ImageError::UnknownFormat { .. })
        ));
        assert!(matches!(
            inspect(&dir.path().join("missing.png")),
            Err(ImageError::Io { .. })
        ));
    }

    #[test]
    fn collect_inputs_scans_directories_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        for name in ["b.PNG", "a.jpg", "notes.txt", "sub/c.webp"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let explicit = dir.path().join("notes.txt");
        let got = collect_inputs(&[dir.path().to_owned(), explicit.clone()]).unwrap();
        let names: Vec<String> = got
            .iter()
            .map(|p| {
                p.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/")
            })
            .collect();
        assert_eq!(names, ["a.jpg", "b.PNG", "notes.txt", "sub/c.webp"]);
    }
}

#[cfg(test)]
pub(crate) mod compatibility_tests {
    use super::*;
    use image::codecs::png::PngEncoder;
    use image::{ImageBuffer, ImageEncoder, Rgb, Rgba};
    use std::fs::File;

    /// A 2x2, two-frame lossless animated WebP (generated, 140 bytes).
    pub(crate) const ANIMATED_WEBP: &[u8] = &[
        0x52, 0x49, 0x46, 0x46, 0x84, 0x0, 0x0, 0x0, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38,
        0x58, 0xa, 0x0, 0x0, 0x0, 0x2, 0x0, 0x0, 0x0, 0x1, 0x0, 0x0, 0x1, 0x0, 0x0, 0x41, 0x4e,
        0x49, 0x4d, 0x6, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x41, 0x4e, 0x4d, 0x46, 0x28,
        0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1, 0x0, 0x0, 0x1, 0x0, 0x0, 0x64, 0x0, 0x0,
        0x2, 0x56, 0x50, 0x38, 0x4c, 0xf, 0x0, 0x0, 0x0, 0x2f, 0x1, 0x40, 0x0, 0x0, 0x7, 0x10,
        0xfd, 0x8f, 0xfe, 0x7, 0x22, 0xa2, 0xff, 0x1, 0x0, 0x41, 0x4e, 0x4d, 0x46, 0x28, 0x0, 0x0,
        0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1, 0x0, 0x0, 0x1, 0x0, 0x0, 0x64, 0x0, 0x0, 0x0, 0x56,
        0x50, 0x38, 0x4c, 0xf, 0x0, 0x0, 0x0, 0x2f, 0x1, 0x40, 0x0, 0x0, 0x7, 0x10, 0xd1, 0xff,
        0xfe, 0x7, 0x22, 0xa2, 0xff, 0x1, 0x0,
    ];

    fn codes(info: &ImageInfo) -> Vec<&'static str> {
        info.problems.iter().map(|p| p.code).collect()
    }

    fn save_rgba(path: &Path, alpha: impl Fn(u32, u32) -> u8) {
        ImageBuffer::from_fn(8, 8, |x, y| Rgba([10, 20, 30, alpha(x, y)]))
            .save(path)
            .unwrap();
    }

    #[test]
    fn opaque_png_has_no_problems() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        ImageBuffer::from_pixel(8, 8, Rgb([1u8, 2, 3]))
            .save(&path)
            .unwrap();
        let info = inspect(&path).unwrap();
        assert!(info.compatible && info.problems.is_empty());
        assert_eq!(info.alpha, None);
    }

    #[test]
    fn png_with_hard_alpha_has_no_problems() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        save_rgba(&path, |x, _| if x < 4 { 255 } else { 0 });
        let info = inspect(&path).unwrap();
        assert!(info.compatible && info.problems.is_empty());
        let alpha = info.alpha.unwrap();
        assert_eq!(
            (alpha.opaque, alpha.translucent, alpha.transparent),
            (32, 0, 32)
        );
    }

    #[test]
    fn soft_alpha_is_a_note_and_still_compatible() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        save_rgba(&path, |x, _| if x < 4 { 255 } else { 128 });
        let info = inspect(&path).unwrap();
        assert!(info.compatible);
        assert_eq!(codes(&info), ["token-translucent-pixels"]);
        assert_eq!(info.problems[0].severity, Severity::Note);
    }

    #[test]
    fn image_without_opaque_pixel_warns_about_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        save_rgba(&path, |_, _| 128);
        let info = inspect(&path).unwrap();
        assert!(info.compatible);
        assert_eq!(codes(&info), ["token-no-opaque-pixel"]);
        assert_eq!(info.problems[0].severity, Severity::Warning);
    }

    #[test]
    fn opaque_area_one_pixel_wide_or_tall_cannot_make_a_token() {
        let dir = tempfile::tempdir().unwrap();
        // The game failed on a single opaque column and a single opaque pixel,
        // and on a gradient whose only opaque pixels are its last column.
        type AlphaFn = fn(u32, u32) -> u8;
        let cases: [(&str, AlphaFn); 3] = [
            ("column", |x, _| if x == 7 { 255 } else { 0 }),
            ("pixel", |x, y| if (x, y) == (3, 3) { 255 } else { 0 }),
            ("gradient", |x, _| (x * 255 / 7) as u8),
        ];
        for (name, alpha) in cases {
            let path = dir.path().join(format!("{name}.png"));
            save_rgba(&path, alpha);
            let info = inspect(&path).unwrap();
            assert_eq!(codes(&info), ["token-opaque-too-small"], "{name}");
            assert!(info.compatible, "{name}");
        }
    }

    #[test]
    fn opaque_area_two_pixels_wide_is_enough() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        save_rgba(&path, |x, _| if x >= 6 { 255 } else { 0 });
        let info = inspect(&path).unwrap();
        assert!(info.problems.is_empty());
        let alpha = info.alpha.unwrap();
        assert_eq!((alpha.opaque_width, alpha.opaque_height), (2, 8));
    }

    #[test]
    fn gif_bmp_and_tiff_are_rejected_with_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        for ext in ["gif", "bmp", "tif"] {
            let path = dir.path().join(format!("a.{ext}"));
            ImageBuffer::from_pixel(4, 4, Rgba([1u8, 2, 3, 255]))
                .save(&path)
                .unwrap();
            let info = inspect(&path).unwrap();
            assert!(!info.compatible, "{ext}");
            assert_eq!(codes(&info), ["format-rejected"], "{ext}");
            assert!(info.problems[0].message.contains("png, jpeg, webp"));
        }
    }

    #[test]
    fn format_is_judged_by_content_not_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("really_a_png.gif");
        ImageBuffer::from_pixel(4, 4, Rgb([1u8, 2, 3]))
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        let info = inspect(&path).unwrap();
        assert_eq!(info.format, "png");
        assert!(info.compatible);
    }

    #[test]
    fn animated_webp_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.webp");
        std::fs::write(&path, ANIMATED_WEBP).unwrap();
        let info = inspect(&path).unwrap();
        assert_eq!((info.frames, info.animated), (2, true));
        assert!(!info.compatible);
        assert_eq!(codes(&info), ["webp-animated"]);
    }

    #[test]
    fn animated_png_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        let mut encoder = png::Encoder::new(File::create(&path).unwrap(), 2, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_animated(2, 0).unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0; 12]).unwrap();
        writer.write_image_data(&[255; 12]).unwrap();
        drop(writer);
        let info = inspect(&path).unwrap();
        assert_eq!(info.frames, 2);
        assert!(info.compatible);
        assert_eq!(codes(&info), ["png-animated"]);
    }

    #[test]
    fn cmyk_jpeg_is_an_error() {
        // SOI, SOF0 (8-bit, 3 x 2, four components), EOI: enough for the header.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x14, 8, 0, 2, 0, 3, 4];
        for id in 1..=4u8 {
            jpeg.extend_from_slice(&[id, 0x11, 0]);
        }
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        std::fs::write(&path, &jpeg).unwrap();
        let info = inspect(&path).unwrap();
        assert_eq!((info.width, info.height), (3, 2));
        assert_eq!(info.color, "cmyk");
        assert!(!info.compatible);
        assert_eq!(codes(&info), ["jpeg-cmyk"]);
    }

    #[test]
    fn rgb_jpeg_is_not_flagged_as_cmyk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        ImageBuffer::from_pixel(8, 8, Rgb([1u8, 2, 3]))
            .save(&path)
            .unwrap();
        let info = inspect(&path).unwrap();
        assert!(info.compatible && info.problems.is_empty());
        assert_eq!(info.color, "rgb");
    }

    #[test]
    fn icc_profile_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        let mut encoder = PngEncoder::new(File::create(&path).unwrap());
        encoder.set_icc_profile(vec![7; 64]).unwrap();
        encoder
            .write_image(&[1, 2, 3], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        let info = inspect(&path).unwrap();
        assert!(info.compatible);
        assert_eq!(codes(&info), ["icc-ignored"]);
    }

    #[test]
    fn very_large_image_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        ImageBuffer::from_pixel(SLOW_SIDE, 1, Rgb([1u8, 2, 3]))
            .save(&path)
            .unwrap();
        let info = inspect(&path).unwrap();
        assert!(info.compatible);
        assert_eq!(codes(&info), ["very-large"]);
    }

    #[test]
    fn sixteen_bit_png_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        ImageBuffer::<Rgb<u16>, Vec<u16>>::from_pixel(4, 4, Rgb([1, 2, 3]))
            .save(&path)
            .unwrap();
        let info = inspect(&path).unwrap();
        assert_eq!(info.bit_depth, 16);
        assert!(info.compatible && info.problems.is_empty());
    }

    #[test]
    fn unrelated_image_format_is_an_error_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.ico");
        // An ICO header with one 1x1 entry is enough for format detection.
        let ico = [
            0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 32, 0, 0, 0, 0, 0, 22, 0, 0, 0,
        ];
        std::fs::write(&path, ico).unwrap();
        assert!(matches!(inspect(&path), Err(ImageError::Rejected { .. })));
    }
}
