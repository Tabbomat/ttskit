//! Image inspection for ttskit (unofficial Tabletop Simulator tooling).
//!
//! This crate is the home for image processing. For now it only reads: it
//! reports what a file contains, without deciding what Tabletop Simulator
//! accepts. Which formats, animated images, 16-bit channels and colour
//! profiles the later conversion commands support is still to be tested
//! against the real game.

use std::fs::File;
use std::io::{BufReader, Seek};
use std::path::{Path, PathBuf};

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, ColorType, ImageDecoder, ImageFormat, ImageReader};
use serde::Serialize;

/// Extensions `collect_inputs` picks up when scanning a directory.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"];

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: unsupported or unrecognised image format", path.display())]
    UnknownFormat { path: PathBuf },
    #[error("{}: {source}", path.display())]
    Decode {
        path: PathBuf,
        source: image::ImageError,
    },
}

/// What a file contains. Stable, serialised by `ttskit image info --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageInfo {
    pub path: String,
    /// Lower-case format name, e.g. `png`, `jpeg`, `webp`.
    pub format: String,
    pub width: u32,
    pub height: u32,
    /// Channel layout, e.g. `rgba`, `rgb`, `luma`, `luma+alpha`.
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

/// Reads the header of `path` and reports what it contains. Pixel data is only
/// decoded for GIF, to count frames.
pub fn inspect(path: &Path) -> Result<ImageInfo, ImageError> {
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
    let mut inner = reader.into_inner();
    inner.rewind().map_err(io)?;

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
            let icc = Ok(None);
            let frames = decoder.into_frames().count() as u32;
            (w, h, color, icc, frames.max(1))
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
    let (layout, bit_depth, has_alpha) = color_layout(color);
    let icc = icc.map_err(decode)?;
    Ok(ImageInfo {
        path: path.display().to_string(),
        format: format_name(format),
        width,
        height,
        color: layout.to_owned(),
        bit_depth,
        has_alpha,
        frames,
        animated: frames > 1,
        icc_profile_bytes: icc.map(|p| p.len()),
        file_bytes,
    })
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
