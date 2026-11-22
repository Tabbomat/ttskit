use super::*;
use image::{ImageBuffer, Rgba};

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn write(path: &Path, img: &ImageBuffer<Rgba<u8>, Vec<u8>>) {
    img.save(path).unwrap();
}

/// Many colours: a 64x64 gradient in all three colour channels.
fn gradient() -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    ImageBuffer::from_fn(64, 64, |x, y| {
        Rgba([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8, 255])
    })
}

fn quantized(outcome: QuantizeOutcome) -> Quantized {
    match outcome {
        QuantizeOutcome::Quantized(q) => q,
        QuantizeOutcome::Skipped(s) => panic!("skipped: {}", s.message),
    }
}

fn skipped(outcome: QuantizeOutcome) -> Skipped {
    match outcome {
        QuantizeOutcome::Skipped(s) => s,
        QuantizeOutcome::Quantized(_) => panic!("not skipped"),
    }
}

fn rgba(bytes: &[u8]) -> Vec<[u8; 4]> {
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .unwrap()
        .to_rgba8()
        .pixels()
        .map(|p| p.0)
        .collect()
}

/// Colour type, palette length and whether a `tRNS` chunk exists.
fn indexed_info(bytes: &[u8]) -> (png::ColorType, usize, bool) {
    let reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let info = reader.info();
    (
        info.color_type,
        info.palette.as_ref().map_or(0, |p| p.len() / 3),
        info.trns.is_some(),
    )
}

fn with_colors(colors: u16) -> QuantizeOptions {
    QuantizeOptions {
        colors,
        ..QuantizeOptions::default()
    }
}

#[test]
fn few_colours_stay_pixel_exact() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    let img = ImageBuffer::from_fn(64, 64, |x, y| match (x / 8 + y / 8) % 3 {
        0 => Rgba([200, 30, 30, 255]),
        1 => Rgba([10, 10, 200, 255]),
        _ => Rgba([10, 200, 10, 128]),
    });
    write(&path, &img);
    let original = std::fs::read(&path).unwrap();
    let q = quantized(quantize(&path, &QuantizeOptions::default()).unwrap());
    assert!(q.smaller);
    assert_eq!(q.colors_used, 3);
    assert_eq!(q.quality, 100);
    assert_eq!(rgba(&q.bytes), rgba(&original));
    assert_eq!(q.quantized_bytes, q.bytes.len() as u64);
    assert_eq!((q.width, q.height), (64, 64));
}

#[test]
fn gradient_is_reduced_and_smaller() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write(&path, &gradient());
    let q = quantized(quantize(&path, &with_colors(32)).unwrap());
    assert!(q.smaller);
    assert!(q.quantized_bytes < q.original_bytes);
    assert!(q.colors_used <= 32 && q.colors_used > 2);
    let (color, palette, _) = indexed_info(&q.bytes);
    assert_eq!(color, png::ColorType::Indexed);
    assert!(palette <= 32);
    assert!(q.quality < 100);
}

#[test]
fn alpha_fade_keeps_varying_alpha() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    let img = ImageBuffer::from_fn(64, 64, |x, y| {
        Rgba([
            (x * 4) as u8,
            (y * 4) as u8,
            (x * y) as u8,
            ((x + y) * 2) as u8,
        ])
    });
    write(&path, &img);
    let q = quantized(quantize(&path, &with_colors(64)).unwrap());
    assert!(q.smaller);
    assert!(indexed_info(&q.bytes).2, "tRNS chunk expected");
    let alphas: std::collections::BTreeSet<u8> = rgba(&q.bytes).iter().map(|p| p[3]).collect();
    assert!(alphas.len() > 8, "{} distinct alpha values", alphas.len());
}

#[test]
fn output_is_deterministic() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write(&path, &gradient());
    for dither in [0.0, 0.5, 1.0] {
        let options = QuantizeOptions {
            colors: 24,
            dither,
            ..QuantizeOptions::default()
        };
        let first = quantized(quantize(&path, &options).unwrap());
        for _ in 0..5 {
            let again = quantized(quantize(&path, &options).unwrap());
            assert_eq!(first.bytes, again.bytes, "dither {dither}");
        }
    }
}

#[test]
fn dither_strength_changes_the_result() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write(&path, &gradient());
    let original = rgba(&std::fs::read(&path).unwrap());
    let run = |dither| {
        quantized(
            quantize(
                &path,
                &QuantizeOptions {
                    colors: 8,
                    dither,
                    ..QuantizeOptions::default()
                },
            )
            .unwrap(),
        )
    };
    let (flat, dithered) = (run(0.0), run(1.0));
    assert_ne!(flat.bytes, dithered.bytes);
    for q in [&flat, &dithered] {
        assert!(q.colors_used <= 8);
        assert_eq!(rgba(&q.bytes).len(), original.len());
    }
}

#[test]
fn unreachable_quality_floor_is_skipped() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write(&path, &gradient());
    let options = QuantizeOptions {
        colors: 4,
        quality: Some((100, 100)),
        ..QuantizeOptions::default()
    };
    let s = skipped(quantize(&path, &options).unwrap());
    assert_eq!(s.code, "quality-too-low");
    assert!(s.message.contains("quality "), "{}", s.message);
}

#[test]
fn quality_max_reduces_colours() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    // Four flat colours, plus noise-free duplicates: 256 colours are far more
    // than needed to reach the maximum.
    let img = ImageBuffer::from_fn(64, 64, |x, y| {
        Rgba([(x / 32 * 255) as u8, (y / 32 * 255) as u8, 0, 255])
    });
    write(&path, &img);
    let options = QuantizeOptions {
        quality: Some((0, 90)),
        ..QuantizeOptions::default()
    };
    let q = quantized(quantize(&path, &options).unwrap());
    assert_eq!(q.colors_used, 4);
    assert_eq!(q.quality, 100);
}

#[test]
fn invalid_options_are_errors() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write(&path, &gradient());
    let bad = [
        with_colors(1),
        with_colors(257),
        QuantizeOptions {
            dither: 1.5,
            ..QuantizeOptions::default()
        },
        QuantizeOptions {
            dither: f32::NAN,
            ..QuantizeOptions::default()
        },
        QuantizeOptions {
            quality: Some((80, 70)),
            ..QuantizeOptions::default()
        },
        QuantizeOptions {
            quality: Some((0, 101)),
            ..QuantizeOptions::default()
        },
        QuantizeOptions {
            level: MAX_LEVEL + 1,
            ..QuantizeOptions::default()
        },
    ];
    for options in bad {
        assert!(
            matches!(quantize(&path, &options), Err(ImageError::Options(_))),
            "{options:?}"
        );
    }
}

#[test]
fn not_png_is_skipped() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    std::fs::write(&path, b"plainly not an image").unwrap();
    let s = skipped(quantize(&path, &QuantizeOptions::default()).unwrap());
    assert_eq!(s.code, "not-png");
}

#[test]
fn animated_png_is_skipped() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    let mut encoder = png::Encoder::new(std::fs::File::create(&path).unwrap(), 2, 2);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(2, 0).unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[0; 12]).unwrap();
    writer.write_image_data(&[255; 12]).unwrap();
    drop(writer);
    let s = skipped(quantize(&path, &QuantizeOptions::default()).unwrap());
    assert_eq!(s.code, "animated");
}

#[test]
fn result_that_is_not_smaller_returns_the_original() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    let img = ImageBuffer::from_fn(2, 2, |x, _| Rgba([x as u8 * 255, 0, 0, 255]));
    write(&path, &img);
    // Make the input as small as it gets, so quantizing cannot win.
    let tight = optimize_bytes(&std::fs::read(&path).unwrap(), MAX_LEVEL, StripMode::All).unwrap();
    std::fs::write(&path, &tight).unwrap();
    let q = quantized(quantize(&path, &with_colors(2)).unwrap());
    assert!(!q.smaller);
    assert_eq!(q.bytes, tight);
    assert_eq!(q.quantized_bytes, q.original_bytes);
}

#[test]
fn sixteen_bit_input_is_reduced() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    let img: ImageBuffer<Rgba<u16>, Vec<u16>> = ImageBuffer::from_fn(32, 32, |x, y| {
        Rgba([(x * 2000) as u16, (y * 2000) as u16, 777, 65535])
    });
    img.save(&path).unwrap();
    let q = quantized(quantize(&path, &with_colors(16)).unwrap());
    assert!(q.colors_used <= 16);
    assert_eq!(rgba(&q.bytes).len(), 32 * 32);
}

#[test]
fn score_endpoints_and_monotony() {
    let a = vec![[10, 20, 30, 255]; 16];
    assert_eq!(score(&a, &a), 100);
    // Invisible pixels: the colour does not count.
    let clear = vec![[10, 20, 30, 0]; 16];
    let other = vec![[200, 0, 0, 0]; 16];
    assert_eq!(score(&clear, &other), 100);
    // Every channel off by 100: psnr ~ 8 dB.
    let far = vec![[110, 120, 130, 255]; 16];
    assert_eq!(score(&a, &far), 0);
    // Off by 1 in one channel: psnr ~ 54 dB.
    let near = vec![[11, 20, 30, 255]; 16];
    assert_eq!(score(&a, &near), 100);
    // Off by 5 in every channel: psnr ~ 34 dB, strictly between.
    let mid = vec![[15, 25, 35, 250]; 16];
    let s = score(&a, &mid);
    assert!(s > 0 && s < 100, "{s}");
    let worse = vec![[25, 35, 45, 245]; 16];
    assert!(score(&a, &worse) < s);
}
