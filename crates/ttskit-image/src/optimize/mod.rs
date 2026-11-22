//! Lossless PNG optimization with `oxipng`. The decoded pixels never change;
//! only the way they are stored does (filters, compression, colour type and
//! bit depth reductions, metadata chunks).
//!
//! The settings that affect the bytes (level, strip mode, oxipng version)
//! make the output deterministic, so an optimized file stays stable. A new
//! ttskit release with another oxipng version may produce other bytes.

use std::path::Path;

use oxipng::{Options, StripChunks};
use serde::Serialize;

use crate::ImageError;

/// Compression level used when none is given (oxipng's own default).
pub const DEFAULT_LEVEL: u8 = 2;
/// Highest level; slowest and smallest.
pub const MAX_LEVEL: u8 = 6;

pub(crate) const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Which metadata chunks to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StripMode {
    /// Keep every chunk.
    None,
    /// Remove chunks that do not affect how a viewer shows the image (text,
    /// time, EXIF, gamma, ...). The colour profile (`iCCP`, `sRGB`, `cICP`),
    /// the pixel density and the animation chunks stay.
    Safe,
    /// Remove every non-critical chunk, including the colour profile and the
    /// animation data. Pixels stay identical, but a viewer that honours the
    /// colour chunks may show different colours.
    All,
}

#[derive(Debug, Clone, Copy)]
pub struct OptimizeOptions {
    /// 0 (fastest) to 6 (smallest).
    pub level: u8,
    pub strip: StripMode,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            level: DEFAULT_LEVEL,
            strip: StripMode::Safe,
        }
    }
}

/// A PNG that was optimized. `bytes` is what the file should contain: the
/// optimized data when it is smaller, the original bytes otherwise.
#[derive(Debug, Clone)]
pub struct Optimized {
    pub bytes: Vec<u8>,
    pub original_bytes: u64,
    /// Size of the result, i.e. `bytes.len()`.
    pub optimized_bytes: u64,
    /// Whether the result is smaller than the original.
    pub smaller: bool,
    pub width: u32,
    pub height: u32,
}

/// A file that was left alone on purpose. `code` is stable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Optimized(Optimized),
    Skipped(Skipped),
}

/// Whether the file has an `acTL` chunk before its image data.
pub(crate) fn is_animated(data: &[u8]) -> bool {
    let mut pos = PNG_SIGNATURE.len();
    while let Some(header) = data.get(pos..pos + 8) {
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        match &header[4..8] {
            b"acTL" => return true,
            b"IDAT" | b"IEND" => return false,
            _ => {}
        }
        pos = pos.saturating_add(len).saturating_add(12);
    }
    false
}

fn decoded(data: &[u8]) -> Result<image::DynamicImage, image::ImageError> {
    image::load_from_memory_with_format(data, image::ImageFormat::Png)
}

/// Runs the lossless `oxipng` pass over PNG data in memory. The error is a
/// message without a path.
pub(crate) fn optimize_raw(data: &[u8], level: u8, strip: StripMode) -> Result<Vec<u8>, String> {
    let mut oxi = Options::from_preset(level);
    oxi.strip = match strip {
        StripMode::None => StripChunks::None,
        StripMode::Safe => StripChunks::Safe,
        StripMode::All => StripChunks::All,
    };
    // Keep the interlacing as it is; the pixels are identical either way,
    // but the result should not depend on a library default.
    oxi.interlace = None;
    // Altering the colour of transparent pixels would change the decoded data.
    oxi.optimize_alpha = false;
    oxi.fix_errors = false;
    oxipng::optimize_from_memory(data, &oxi).map_err(|e| format!("optimization failed: {e}"))
}

/// Optimizes the PNG at `path` in memory; nothing is written. Anything that
/// is not a PNG (decided by content) is skipped, not an error.
pub fn optimize(path: &Path, options: &OptimizeOptions) -> Result<Outcome, ImageError> {
    check_level(options)?;
    let original = std::fs::read(path).map_err(|source| ImageError::Io {
        path: path.to_owned(),
        source,
    })?;
    optimize_bytes(path, &original, options)
}

fn check_level(options: &OptimizeOptions) -> Result<(), ImageError> {
    if options.level > MAX_LEVEL {
        return Err(ImageError::Options(format!(
            "level must be between 0 and {MAX_LEVEL}, not {}",
            options.level
        )));
    }
    Ok(())
}

/// Like [`optimize`], for PNG data already in memory. `label` is only used in
/// error messages; it is never opened.
pub fn optimize_bytes(
    label: &Path,
    original: &[u8],
    options: &OptimizeOptions,
) -> Result<Outcome, ImageError> {
    let path = label;
    check_level(options)?;
    let fail = |message: String| ImageError::Optimize {
        path: path.to_owned(),
        message,
    };
    if !original.starts_with(&PNG_SIGNATURE) {
        return Ok(Outcome::Skipped(Skipped {
            code: "not-png",
            message: "not a PNG; only PNG files are optimized".to_owned(),
        }));
    }
    if options.strip == StripMode::All && is_animated(original) {
        return Ok(Outcome::Skipped(Skipped {
            code: "animated-strip-all",
            message:
                "animated PNG; --strip all would remove the animation, use --strip safe or none"
                    .to_owned(),
        }));
    }

    let before = decoded(original).map_err(|e| fail(format!("not a readable PNG: {e}")))?;
    let candidate = optimize_raw(original, options.level, options.strip).map_err(fail)?;

    let (width, height) = (before.width(), before.height());
    let original_bytes = original.len() as u64;
    if candidate.len() >= original.len() {
        return Ok(Outcome::Optimized(Optimized {
            bytes: original.to_vec(),
            original_bytes,
            optimized_bytes: original_bytes,
            smaller: false,
            width,
            height,
        }));
    }
    // The guarantee of this command: refuse to hand out a result whose
    // pixels differ from the input's.
    let after = decoded(&candidate)
        .map_err(|e| fail(format!("optimized data could not be read back: {e}")))?;
    if before.to_rgba16() != after.to_rgba16() {
        return Err(fail(
            "optimized pixels differ from the original; the file was left alone".to_owned(),
        ));
    }
    Ok(Outcome::Optimized(Optimized {
        optimized_bytes: candidate.len() as u64,
        bytes: candidate,
        original_bytes,
        smaller: true,
        width,
        height,
    }))
}

#[cfg(test)]
mod tests;
