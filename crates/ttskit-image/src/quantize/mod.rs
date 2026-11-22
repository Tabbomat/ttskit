//! Lossy PNG colour quantization: the image is reduced to an indexed PNG with
//! at most 256 colours (alpha included, stored in `tRNS`) using the built-in
//! `exoquant` quantizer, then run through the same lossless `oxipng` pass as
//! `optimize`.
//!
//! Pipeline:
//!
//! 1. The input is decoded to 8-bit RGBA. 16-bit input is reduced to 8 bits.
//!    The colour of fully transparent pixels is irrelevant and normalised to
//!    transparent black.
//! 2. An image with at most `colors` distinct RGBA values keeps them exactly
//!    as its palette, without dithering. Otherwise a palette is generated
//!    (median cut refined by k-means) and the pixels are remapped to it with
//!    Floyd-Steinberg error diffusion scaled by `dither`.
//! 3. The result is encoded as an indexed PNG and optimized losslessly. The
//!    decoded final pixels must equal the quantized pixels, or the call fails.
//!
//! The output is deterministic for a given input, options and ttskit release
//! (built-in engine).
//!
//! # Engines
//!
//! The built-in engine is the default. `Engine::Pngquant` instead runs the
//! user's own `pngquant` executable (not shipped with ttskit) with explicit
//! arguments and no shell, passing the PNG on stdin and reading the result
//! from stdout. Its output depends on the installed pngquant version, so it
//! can differ between machines. The rest is shared: the result goes through
//! the lossless pass (the smaller file wins), the decoded final pixels must
//! equal those of pngquant's output, and the quality score below is computed
//! against the original whatever the engine.
//!
//! The file is re-encoded, so ancillary chunks of the input (colour profile,
//! pixel density, text, ...) are not carried over.
//!
//! # Quality score
//!
//! The quality of the result is a number from 0 to 100 derived from the mean
//! squared error (MSE) between the original and the quantized pixels. Both
//! are alpha-premultiplied first (colour channels multiplied by alpha / 255,
//! alpha kept), so the colour of invisible pixels does not count. The MSE is
//! taken over all four channels on the 0..=255 scale, then
//!
//! ```text
//! psnr  = 10 * log10(255^2 / mse)           (infinite when mse == 0)
//! score = floor(clamp((psnr - 20) * 100 / 25, 0, 100))
//! ```
//!
//! so 20 dB or less is 0, 45 dB or more is 100, and identical images are 100.
//!
//! With `quality = (min, max)`: a result scoring below `min` is skipped
//! (`quality-too-low`). Colours are then reduced further (halving `colors`
//! down to 2) as long as the score stays at or above `max`, to save bytes.

mod external;

use std::path::{Path, PathBuf};

use exoquant::ditherer::{self, Ditherer};
use exoquant::optimizer::{KMeans, Optimizer};
use exoquant::{Color, ColorCount, ColorMap, ColorSpace, Colorf, Remapper, SimpleColorSpace};

use crate::ImageError;
use crate::optimize::{
    DEFAULT_LEVEL, MAX_LEVEL, PNG_SIGNATURE, Skipped, StripMode, is_animated, optimize_bytes,
};

/// Palette size used when none is given.
pub const DEFAULT_COLORS: u16 = 256;
/// Fewest and most colours a palette can have.
pub const MIN_COLORS: u16 = 2;
pub const MAX_COLORS: u16 = 256;

/// PSNR (dB) that maps to score 0 and to score 100.
const PSNR_FLOOR: f64 = 20.0;
const PSNR_CEIL: f64 = 45.0;

/// Which quantizer produces the palette image.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Engine {
    /// The built-in quantizer.
    #[default]
    Builtin,
    /// The user's `pngquant` executable. `program` is its path; `None` looks
    /// `pngquant` up on `PATH`.
    Pngquant { program: Option<PathBuf> },
}

#[derive(Debug, Clone)]
pub struct QuantizeOptions {
    /// Largest palette size, 2 to 256.
    pub colors: u16,
    /// `(min, max)` quality, each 0 to 100. Built-in engine: results below
    /// `min` are skipped, and colours are halved while the score stays at or
    /// above `max`. `pngquant`: passed as `--quality MIN-MAX` on its own scale
    /// (its scores differ from the built-in ones); it does the reduction
    /// itself, and its exit code 99 is reported as `quality-too-low`.
    pub quality: Option<(u8, u8)>,
    /// Dithering strength, 0.0 (none) to 1.0 (full Floyd-Steinberg).
    pub dither: f32,
    /// Level of the final lossless pass, 0 to 6.
    pub level: u8,
    pub strip: StripMode,
    pub engine: Engine,
}

impl Default for QuantizeOptions {
    fn default() -> Self {
        Self {
            colors: DEFAULT_COLORS,
            quality: None,
            dither: 1.0,
            level: DEFAULT_LEVEL,
            strip: StripMode::Safe,
            engine: Engine::Builtin,
        }
    }
}

/// A PNG that was quantized. `bytes` is what the file should contain: the
/// quantized data when it is smaller, the original bytes otherwise (then
/// `colors_used` and `quality` still describe the quantized candidate).
#[derive(Debug, Clone)]
pub struct Quantized {
    pub bytes: Vec<u8>,
    pub original_bytes: u64,
    /// Size of the result, i.e. `bytes.len()`.
    pub quantized_bytes: u64,
    /// Whether the result is smaller than the original.
    pub smaller: bool,
    pub width: u32,
    pub height: u32,
    /// Palette entries of the quantized image.
    pub colors_used: u16,
    /// Quality score of the quantized image, 0 to 100.
    pub quality: u8,
}

#[derive(Debug, Clone)]
pub enum QuantizeOutcome {
    Quantized(Quantized),
    Skipped(Skipped),
}

/// Indexed image: `palette` entries are RGBA, `indices` one per pixel.
struct Indexed {
    palette: Vec<[u8; 4]>,
    indices: Vec<u8>,
}

impl Indexed {
    fn rgba(&self) -> Vec<[u8; 4]> {
        self.indices
            .iter()
            .map(|&i| self.palette[i as usize])
            .collect()
    }
}

/// Floyd-Steinberg error diffusion with an adjustable strength.
struct ScaledFloydSteinberg {
    strength: f64,
}

impl Ditherer for ScaledFloydSteinberg {
    fn remap<'a>(
        &'a self,
        image: Box<dyn Iterator<Item = Colorf> + 'a>,
        width: usize,
        map: &'a ColorMap,
        colorspace: &'a dyn ColorSpace,
    ) -> Box<dyn Iterator<Item = usize> + 'a> {
        // Error rows for the current and the next image row, with one spare
        // cell on each side so the neighbours need no bounds checks.
        let mut current = vec![Colorf::zero(); width + 2];
        let mut next = vec![Colorf::zero(); width + 2];
        Box::new(image.enumerate().map(move |(i, c)| {
            let x = i % width;
            if i > 0 && x == 0 {
                std::mem::swap(&mut current, &mut next);
                next.fill(Colorf::zero());
            }
            let c = colorspace.to_dither(c);
            let wanted = c + current[x + 1];
            let index = map.find_nearest(colorspace.from_dither(wanted));
            let got = colorspace.to_dither(map.float_color(index));
            // Pass on only part of the accumulated error, as exoquant's own
            // variant does, to limit colour bleeding.
            let error = (c + current[x + 1] * 0.8 - got) * self.strength;
            current[x + 2] += error * (7.0 / 16.0);
            next[x] += error * (3.0 / 16.0);
            next[x + 1] += error * (5.0 / 16.0);
            next[x + 2] += error * (1.0 / 16.0);
            index
        }))
    }
}

fn exact_palette(pixels: &[[u8; 4]], colors: usize) -> Option<Indexed> {
    let mut palette = pixels.to_vec();
    palette.sort_unstable();
    palette.dedup();
    if palette.len() > colors {
        return None;
    }
    let indices = pixels
        .iter()
        .map(|p| palette.binary_search(p).expect("colour is in the palette") as u8)
        .collect();
    Some(Indexed { palette, indices })
}

/// One box of colours in the median cut, as in exoquant's `Quantizer`.
/// exoquant's own `Histogram` iterates a randomly seeded `HashMap`, which
/// makes its palettes differ from run to run, so the cut is done here on a
/// sorted histogram. The splitting rule is the same as exoquant's.
struct CutNode {
    histogram: Vec<ColorCount>,
    avg: Colorf,
    /// Improvement of the total variance when this node is split.
    vdif: f64,
    /// Index at which the (sorted) histogram is split.
    split: usize,
}

impl CutNode {
    fn new(mut histogram: Vec<ColorCount>) -> Self {
        let mut n = 0usize;
        let mut fsum = Colorf::zero();
        let mut fsum2 = Colorf::zero();
        for entry in &histogram {
            n += entry.count;
            fsum += entry.color * entry.count as f64;
            fsum2 += entry.color * entry.color * entry.count as f64;
        }
        if n == 0 {
            return Self {
                histogram,
                avg: Colorf::zero(),
                vdif: 0.0,
                split: 0,
            };
        }
        let avg = fsum * (1.0 / n as f64);
        let vc = fsum2 - fsum * avg;
        let v = vc.r + vc.g + vc.b + vc.a;

        // Sort by the channel with the largest variance, then by the main
        // direction of the distribution.
        if vc.r > vc.g && vc.r > vc.b && vc.r > vc.a {
            histogram.sort_by(|a, b| a.color.r.total_cmp(&b.color.r));
        } else if vc.g > vc.b && vc.g > vc.a {
            histogram.sort_by(|a, b| a.color.g.total_cmp(&b.color.g));
        } else if vc.b > vc.a {
            histogram.sort_by(|a, b| a.color.b.total_cmp(&b.color.b));
        } else {
            histogram.sort_by(|a, b| a.color.a.total_cmp(&b.color.a));
        }
        let mut dir = Colorf::zero();
        for entry in &histogram {
            let mut tmp = (entry.color - avg) * entry.count as f64;
            if tmp.dot(&dir) < 0.0 {
                tmp *= -1.0;
            }
            dir += tmp;
        }
        let length = dir.dot(&dir).sqrt();
        dir *= if length < 1e-9 { 1.0 } else { 1.0 / length };
        histogram.sort_by(|a, b| a.color.dot(&dir).total_cmp(&b.color.dot(&dir)));

        // The split index with the lowest total variance of both halves.
        let mut sum = Colorf::zero();
        let mut sum2 = Colorf::zero();
        let mut vdif = -v;
        let mut n2 = 0;
        let mut split = 0usize;
        for (i, entry) in histogram.iter().enumerate() {
            n2 += entry.count;
            sum += entry.color * entry.count as f64;
            sum2 += entry.color * entry.color * entry.count as f64;
            if n2 < n {
                let low = sum2 - sum * sum * (1.0 / n2 as f64);
                let rest = fsum - sum;
                let high = (fsum2 - sum2) - rest * rest * (1.0 / (n - n2) as f64);
                let nv = low.r + low.g + low.b + low.a + high.r + high.g + high.b + high.a;
                if -nv > vdif {
                    vdif = -nv;
                    split = i + 1;
                }
            }
        }
        Self {
            histogram,
            avg,
            vdif: vdif + v,
            split,
        }
    }
}

/// Median cut with k-means refinement along the way, like exoquant's
/// `generate_palette`, on a histogram in a fixed order.
fn generate_palette(
    histogram: &[ColorCount],
    colorspace: &SimpleColorSpace,
    colors: usize,
) -> Vec<Color> {
    let kmeans_step = if colors > 64 {
        colors
    } else if colors <= 16 {
        1
    } else {
        (colors as f64).sqrt().round() as usize
    };
    let mut nodes = vec![CutNode::new(histogram.to_vec())];
    while nodes.len() < colors {
        let mut best = 0;
        let mut best_gain = 0.0;
        for (i, node) in nodes.iter().enumerate() {
            if node.vdif >= best_gain {
                best_gain = node.vdif;
                best = i;
            }
        }
        let node = nodes.swap_remove(best);
        let mut low = node.histogram;
        let high = low.split_off(node.split);
        nodes.push(CutNode::new(low));
        nodes.push(CutNode::new(high));
        if nodes.len() % kmeans_step == 0 {
            nodes = refine(nodes, histogram, 4);
        }
    }
    nodes
        .iter()
        .map(|node| colorspace.from_float(node.avg))
        .collect()
}

/// A few k-means steps over the whole histogram, then new boxes from the
/// resulting clusters.
fn refine(nodes: Vec<CutNode>, histogram: &[ColorCount], steps: usize) -> Vec<CutNode> {
    let mut centres: Vec<Colorf> = nodes.iter().map(|node| node.avg).collect();
    // The nodes' histograms hold every entry exactly once, in node order.
    let all: Vec<ColorCount> = nodes.into_iter().flat_map(|node| node.histogram).collect();
    for _ in 0..steps {
        centres = KMeans.step(centres, &all);
    }
    let mut clusters: Vec<Vec<ColorCount>> = vec![Vec::new(); centres.len()];
    let map = ColorMap::from_float_colors(centres);
    for entry in all {
        clusters[map.find_nearest(entry.color)].push(entry);
    }
    debug_assert_eq!(
        clusters.iter().map(Vec::len).sum::<usize>(),
        histogram.len()
    );
    clusters.into_iter().map(CutNode::new).collect()
}

fn quantize_pixels(pixels: &[[u8; 4]], width: usize, colors: usize, dither: f64) -> Indexed {
    if let Some(exact) = exact_palette(pixels, colors) {
        return exact;
    }
    let colorspace = SimpleColorSpace::default();
    // Every distinct colour once with its count, in sorted order.
    let mut sorted = pixels.to_vec();
    sorted.sort_unstable();
    let mut histogram: Vec<ColorCount> = Vec::new();
    let mut last = None;
    for p in &sorted {
        if last == Some(p) {
            histogram.last_mut().expect("entry exists").count += 1;
        } else {
            histogram.push(ColorCount {
                color: colorspace.to_float(Color::new(p[0], p[1], p[2], p[3])),
                count: 1,
            });
            last = Some(p);
        }
    }
    let mut palette = generate_palette(&histogram, &colorspace, colors);
    let mut centres: Vec<Colorf> = palette.iter().map(|&c| colorspace.to_float(c)).collect();
    for _ in 0..8 {
        centres = KMeans.step(centres, &histogram);
    }
    palette = centres.iter().map(|&c| colorspace.from_float(c)).collect();
    // exoquant converts back to 8 bits by truncation, which turns opaque
    // into alpha 254; opaque input must stay opaque.
    if pixels.iter().all(|p| p[3] == 255) {
        for c in &mut palette {
            c.a = 255;
        }
    }
    let input: Vec<Color> = pixels
        .iter()
        .map(|p| Color::new(p[0], p[1], p[2], p[3]))
        .collect();
    let ditherer: Box<dyn Ditherer> = if dither <= 0.0 {
        Box::new(ditherer::None)
    } else {
        Box::new(ScaledFloydSteinberg { strength: dither })
    };
    let indices = Remapper::new(&palette, &colorspace, &*ditherer).remap(&input, width);
    let (palette, indices) = exoquant::sort_palette(&palette, &indices);

    // Drop entries no pixel uses and renumber the rest in order.
    let mut used = [false; 256];
    for &i in &indices {
        used[i as usize] = true;
    }
    let mut renumber = [0u8; 256];
    let mut kept = Vec::new();
    for (i, c) in palette.iter().enumerate() {
        if used[i] {
            renumber[i] = kept.len() as u8;
            kept.push([c.r, c.g, c.b, c.a]);
        }
    }
    Indexed {
        palette: kept,
        indices: indices.iter().map(|&i| renumber[i as usize]).collect(),
    }
}

fn premultiplied(p: &[u8; 4]) -> [f64; 4] {
    let a = f64::from(p[3]);
    [
        f64::from(p[0]) * a / 255.0,
        f64::from(p[1]) * a / 255.0,
        f64::from(p[2]) * a / 255.0,
        a,
    ]
}

/// Quality score, 0 to 100, of `result` against `original` (module docs).
fn score(original: &[[u8; 4]], result: &[[u8; 4]]) -> u8 {
    let total: f64 = original
        .iter()
        .zip(result)
        .map(|(a, b)| {
            let (a, b) = (premultiplied(a), premultiplied(b));
            (0..4).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>()
        })
        .sum();
    let mse = total / (original.len() * 4) as f64;
    if mse == 0.0 {
        return 100;
    }
    let psnr = 10.0 * (255.0f64 * 255.0 / mse).log10();
    ((psnr - PSNR_FLOOR) * 100.0 / (PSNR_CEIL - PSNR_FLOOR))
        .clamp(0.0, 100.0)
        .floor() as u8
}

fn encode(indexed: &Indexed, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_palette(
        indexed
            .palette
            .iter()
            .flat_map(|c| [c[0], c[1], c[2]])
            .collect::<Vec<u8>>(),
    );
    if indexed.palette.iter().any(|c| c[3] != 255) {
        encoder.set_trns(indexed.palette.iter().map(|c| c[3]).collect::<Vec<u8>>());
    }
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(&indexed.indices)
        .map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

fn validate(options: &QuantizeOptions) -> Result<(), ImageError> {
    let bad = |message: String| Err(ImageError::Options(message));
    if !(MIN_COLORS..=MAX_COLORS).contains(&options.colors) {
        return bad(format!(
            "colors must be between {MIN_COLORS} and {MAX_COLORS}, not {}",
            options.colors
        ));
    }
    if !(0.0..=1.0).contains(&options.dither) {
        return bad(format!(
            "dither must be between 0 and 1, not {}",
            options.dither
        ));
    }
    if options.level > MAX_LEVEL {
        return bad(format!(
            "level must be between 0 and {MAX_LEVEL}, not {}",
            options.level
        ));
    }
    if let Some((min, max)) = options.quality
        && (min > max || max > 100)
    {
        return bad(format!(
            "quality must be MIN-MAX with 0 <= MIN <= MAX <= 100, not {min}-{max}"
        ));
    }
    Ok(())
}

/// Quantizes the PNG at `path` in memory; nothing is written. Anything that
/// is not a PNG (decided by content), an animated PNG (re-encoding would drop
/// the frames) and an image that cannot reach the quality floor is skipped,
/// not an error.
pub fn quantize(path: &Path, options: &QuantizeOptions) -> Result<QuantizeOutcome, ImageError> {
    validate(options)?;
    let fail = |message: String| ImageError::Optimize {
        path: path.to_owned(),
        message,
    };
    let skip = |code, message: &str| {
        Ok(QuantizeOutcome::Skipped(Skipped {
            code,
            message: message.to_owned(),
        }))
    };
    let original = std::fs::read(path).map_err(|source| ImageError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !original.starts_with(&PNG_SIGNATURE) {
        return skip("not-png", "not a PNG; only PNG files are quantized");
    }
    if is_animated(&original) {
        return skip(
            "animated",
            "animated PNG; quantizing would remove the animation",
        );
    }
    let decoded = image::load_from_memory_with_format(&original, image::ImageFormat::Png)
        .map_err(|e| fail(format!("not a readable PNG: {e}")))?;
    let (width, height) = (decoded.width(), decoded.height());
    let pixels: Vec<[u8; 4]> = decoded
        .to_rgba8()
        .pixels()
        .map(|p| if p.0[3] == 0 { [0; 4] } else { p.0 })
        .collect();

    let produced = match &options.engine {
        Engine::Builtin => builtin_candidate(&pixels, width, height, options, &fail)?,
        Engine::Pngquant { program } => {
            external_candidate(program.as_deref(), &original, &pixels, options, &fail)?
        }
    };
    let Candidate {
        encoded,
        pixels: result,
        colors_used,
        quality,
    } = match produced {
        Ok(candidate) => candidate,
        Err(skipped) => return Ok(QuantizeOutcome::Skipped(skipped)),
    };

    let candidate = optimize_bytes(&encoded, options.level, options.strip).map_err(&fail)?;
    let candidate = if candidate.len() < encoded.len() {
        candidate
    } else {
        encoded
    };

    let original_bytes = original.len() as u64;
    if candidate.len() >= original.len() {
        return Ok(QuantizeOutcome::Quantized(Quantized {
            bytes: original,
            original_bytes,
            quantized_bytes: original_bytes,
            smaller: false,
            width,
            height,
            colors_used,
            quality,
        }));
    }
    // Refuse to hand out a file whose pixels differ from what was computed.
    let after = image::load_from_memory_with_format(&candidate, image::ImageFormat::Png)
        .map_err(|e| fail(format!("quantized data could not be read back: {e}")))?;
    let after: Vec<[u8; 4]> = after.to_rgba8().pixels().map(|p| p.0).collect();
    if after != result {
        return Err(fail(
            "quantized pixels differ from the computed result; the file was left alone".to_owned(),
        ));
    }
    Ok(QuantizeOutcome::Quantized(Quantized {
        quantized_bytes: candidate.len() as u64,
        bytes: candidate,
        original_bytes,
        smaller: true,
        width,
        height,
        colors_used,
        quality,
    }))
}

/// What an engine produced: an encoded PNG, the pixels it decodes to, and its
/// palette size and score.
struct Candidate {
    encoded: Vec<u8>,
    pixels: Vec<[u8; 4]>,
    colors_used: u16,
    quality: u8,
}

/// `Err(Skipped)` when the engine's result is rejected, e.g. below the quality
/// floor.
type Produced = Result<Candidate, Skipped>;

fn builtin_candidate(
    pixels: &[[u8; 4]],
    width: u32,
    height: u32,
    options: &QuantizeOptions,
    fail: &dyn Fn(String) -> ImageError,
) -> Result<Produced, ImageError> {
    let run = |colors: usize| {
        let indexed = quantize_pixels(pixels, width as usize, colors, f64::from(options.dither));
        let quality = score(pixels, &indexed.rgba());
        (indexed, quality)
    };
    let (mut indexed, mut quality) = run(usize::from(options.colors));
    if let Some((min, max)) = options.quality {
        if quality < min {
            return Ok(Err(Skipped {
                code: "quality-too-low",
                message: format!(
                    "quality {quality} with {} colors is below the minimum {min}",
                    options.colors
                ),
            }));
        }
        let mut colors = usize::from(options.colors);
        while colors / 2 >= usize::from(MIN_COLORS) {
            colors /= 2;
            let (candidate, candidate_quality) = run(colors);
            if candidate_quality < max {
                break;
            }
            (indexed, quality) = (candidate, candidate_quality);
        }
    }
    let encoded = encode(&indexed, width, height)
        .map_err(|e| fail(format!("could not encode the result: {e}")))?;
    Ok(Ok(Candidate {
        encoded,
        pixels: indexed.rgba(),
        colors_used: indexed.palette.len() as u16,
        quality,
    }))
}

fn external_candidate(
    program: Option<&Path>,
    original: &[u8],
    pixels: &[[u8; 4]],
    options: &QuantizeOptions,
    fail: &dyn Fn(String) -> ImageError,
) -> Result<Produced, ImageError> {
    let encoded = match external::run(program, original, options) {
        Ok(external::Run::Png(bytes)) => bytes,
        Ok(external::Run::BelowQuality(message)) => {
            return Ok(Err(Skipped {
                code: "quality-too-low",
                message,
            }));
        }
        Err(external::RunError::NotFound(message)) => return Err(ImageError::Options(message)),
        Err(external::RunError::Failed(message)) => return Err(fail(message)),
    };
    if !encoded.starts_with(&PNG_SIGNATURE) {
        return Err(fail("pngquant did not write a PNG".to_owned()));
    }
    let decoded = image::load_from_memory_with_format(&encoded, image::ImageFormat::Png)
        .map_err(|e| fail(format!("pngquant wrote an unreadable PNG: {e}")))?;
    let result: Vec<[u8; 4]> = decoded.to_rgba8().pixels().map(|p| p.0).collect();
    if result.len() != pixels.len() {
        return Err(fail(
            "pngquant changed the image dimensions; the file was left alone".to_owned(),
        ));
    }
    let mut distinct = result.clone();
    distinct.sort_unstable();
    distinct.dedup();
    // The score is premultiplied, so the colour of invisible pixels does not
    // matter.
    Ok(Ok(Candidate {
        quality: score(pixels, &result),
        colors_used: u16::try_from(distinct.len()).unwrap_or(u16::MAX),
        encoded,
        pixels: result,
    }))
}

#[cfg(test)]
mod tests;
