use super::*;
use crate::compatibility_tests::ANIMATED_WEBP;
use image::{GenericImageView, ImageBuffer, Luma, Rgb, Rgba};

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn opts(to: OutputFormat) -> ConvertOptions {
    ConvertOptions::new(to)
}

/// A gradient with distinct values per pixel, fully opaque.
fn rgba_opaque(w: u32, h: u32) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    ImageBuffer::from_fn(w, h, |x, y| {
        Rgba([(x * 17) as u8, (y * 29) as u8, (x + y) as u8, 255])
    })
}

fn rgba_soft(w: u32, h: u32) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    ImageBuffer::from_fn(w, h, |x, y| {
        Rgba([(x * 17) as u8, (y * 29) as u8, 99, (x * 40 + y * 3) as u8])
    })
}

fn decode(bytes: &[u8]) -> DynamicImage {
    image::load_from_memory(bytes).unwrap()
}

fn codes(c: &Converted) -> Vec<&'static str> {
    c.notes.iter().map(|n| n.code).collect()
}

// Lossless pairs: the pixels must survive exactly.

#[test]
fn lossless_sources_to_png_keep_pixels_exactly() {
    let dir = tmp();
    let src = rgba_soft(6, 5);
    for ext in ["png", "bmp", "tiff", "webp"] {
        let path = dir.path().join(format!("a.{ext}"));
        src.save(&path).unwrap();
        let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
        assert_eq!(decode(&out.bytes).to_rgba8(), src, "from {ext}");
        assert_eq!(out.from, ext);
        assert_eq!((out.width, out.height), (6, 5));
    }
}

#[test]
fn lossless_sources_to_png_keep_rgb_exactly() {
    let dir = tmp();
    let rgb = ImageBuffer::from_fn(4, 4, |x, y| Rgb([x as u8 * 3, y as u8 * 5, 200]));
    for ext in ["bmp", "tiff", "png"] {
        let path = dir.path().join(format!("rgb.{ext}"));
        rgb.save(&path).unwrap();
        let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
        let back = decode(&out.bytes);
        assert_eq!(back.color(), ColorType::Rgb8, "{ext}");
        assert_eq!(back.to_rgb8(), rgb, "{ext}");
    }
}

#[test]
fn gray_keeps_pixels_exactly() {
    let dir = tmp();
    let gray = ImageBuffer::from_fn(4, 4, |x, y| Luma([(x * 40 + y) as u8]));
    let path = dir.path().join("gray.png");
    gray.save(&path).unwrap();
    let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
    let back = decode(&out.bytes);
    assert_eq!(back.color(), ColorType::L8);
    assert_eq!(back.to_luma8(), gray);
}

#[test]
fn gif_to_png_keeps_pixels_exactly() {
    let dir = tmp();
    let path = dir.path().join("a.gif");
    let src = ImageBuffer::from_fn(4, 3, |x, _| {
        if x < 2 {
            Rgba([255u8, 0, 0, 255])
        } else {
            Rgba([0, 0, 255, 255])
        }
    });
    src.save(&path).unwrap();
    let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
    assert_eq!(out.from, "gif");
    assert_eq!(decode(&out.bytes).to_rgba8(), src);
    assert!(out.notes.is_empty());
}

#[test]
fn sixteen_bit_png_keeps_its_depth() {
    let dir = tmp();
    let src = ImageBuffer::<Rgba<u16>, Vec<u16>>::from_fn(3, 3, |x, y| {
        Rgba([x as u16 * 9000 + 1, y as u16 * 7000 + 3, 12345, 40000])
    });
    let path = dir.path().join("a.png");
    src.save(&path).unwrap();
    let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
    let back = decode(&out.bytes);
    assert_eq!(back.color(), ColorType::Rgba16);
    assert_eq!(back.to_rgba16(), src);
    assert!(out.notes.is_empty());
}

#[test]
fn sixteen_bit_to_jpeg_is_reduced_with_a_note() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    ImageBuffer::<Rgb<u16>, Vec<u16>>::from_pixel(8, 8, Rgb([1000, 2000, 3000]))
        .save(&path)
        .unwrap();
    let out = convert(&path, &opts(OutputFormat::Jpeg)).unwrap();
    assert_eq!(decode(&out.bytes).color(), ColorType::Rgb8);
    assert_eq!(codes(&out), ["bit-depth-reduced"]);
}

// JPEG output.

#[test]
fn opaque_image_to_jpeg_is_close_to_the_source() {
    let dir = tmp();
    let src = ImageBuffer::from_pixel(16, 16, Rgb([200u8, 100, 50]));
    for ext in ["png", "bmp", "tiff", "webp"] {
        let path = dir.path().join(format!("a.{ext}"));
        src.save(&path).unwrap();
        let out = convert(&path, &opts(OutputFormat::Jpeg)).unwrap();
        let back = decode(&out.bytes);
        assert_eq!(back.dimensions(), (16, 16));
        let p = back.to_rgb8().get_pixel(8, 8).0;
        for (a, b) in p.iter().zip([200u8, 100, 50]) {
            assert!(a.abs_diff(b) <= 3, "{ext}: {p:?}");
        }
    }
}

#[test]
fn jpeg_to_jpeg_reencodes() {
    let dir = tmp();
    let path = dir.path().join("a.jpg");
    ImageBuffer::from_pixel(16, 16, Rgb([10u8, 200, 90]))
        .save(&path)
        .unwrap();
    let out = convert(&path, &opts(OutputFormat::Jpeg)).unwrap();
    assert_eq!(out.from, "jpeg");
    let p = decode(&out.bytes).to_rgb8().get_pixel(3, 3).0;
    for (a, b) in p.iter().zip([10u8, 200, 90]) {
        assert!(a.abs_diff(b) <= 6, "{p:?}");
    }
}

#[test]
fn jpeg_to_png_has_the_decoded_pixels() {
    let dir = tmp();
    let path = dir.path().join("a.jpg");
    ImageBuffer::from_pixel(8, 8, Rgb([10u8, 200, 90]))
        .save(&path)
        .unwrap();
    let direct = image::open(&path).unwrap().to_rgb8();
    let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
    assert_eq!(decode(&out.bytes).to_rgb8(), direct);
}

#[test]
fn gray_stays_gray_in_jpeg() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    ImageBuffer::from_pixel(8, 8, Luma([90u8]))
        .save(&path)
        .unwrap();
    let out = convert(&path, &opts(OutputFormat::Jpeg)).unwrap();
    assert_eq!(decode(&out.bytes).color(), ColorType::L8);
}

#[test]
fn higher_quality_gives_bigger_jpeg() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    ImageBuffer::from_fn(64, 64, |x, y| {
        Rgb([(x * x) as u8, (y * 7) as u8, (x ^ y) as u8])
    })
    .save(&path)
    .unwrap();
    let mut low = opts(OutputFormat::Jpeg);
    low.quality = 20;
    let mut high = opts(OutputFormat::Jpeg);
    high.quality = 95;
    let low = convert(&path, &low).unwrap();
    let high = convert(&path, &high).unwrap();
    assert!(low.bytes.len() < high.bytes.len());
}

#[test]
fn default_quality_is_85() {
    assert_eq!(DEFAULT_JPEG_QUALITY, 85);
    assert_eq!(opts(OutputFormat::Jpeg).quality, 85);
}

#[test]
fn quality_out_of_range_is_refused() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    rgba_opaque(2, 2).save(&path).unwrap();
    for q in [0u8, 101] {
        let mut o = opts(OutputFormat::Jpeg);
        o.quality = q;
        assert!(matches!(convert(&path, &o), Err(ImageError::Options(_))));
    }
}

#[test]
fn transparency_to_jpeg_is_refused() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    rgba_soft(4, 4).save(&path).unwrap();
    let err = convert(&path, &opts(OutputFormat::Jpeg)).unwrap_err();
    assert!(matches!(err, ImageError::AlphaLost { .. }), "{err}");
    assert!(err.to_string().contains("--background"));
}

#[test]
fn one_transparent_pixel_is_enough_to_refuse() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    let mut img = rgba_opaque(4, 4);
    img.put_pixel(1, 1, Rgba([1, 2, 3, 254]));
    img.save(&path).unwrap();
    let err = convert(&path, &opts(OutputFormat::Jpeg)).unwrap_err();
    assert!(matches!(
        err,
        ImageError::AlphaLost {
            pixels: 1,
            total: 16,
            ..
        }
    ));
}

#[test]
fn fully_opaque_alpha_channel_to_jpeg_needs_no_background() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    rgba_opaque(4, 4).save(&path).unwrap();
    let out = convert(&path, &opts(OutputFormat::Jpeg)).unwrap();
    assert_eq!(decode(&out.bytes).color(), ColorType::Rgb8);
    assert!(out.notes.is_empty());
}

#[test]
fn background_flattens_transparency() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    let mut src = ImageBuffer::from_pixel(16, 16, Rgba([255u8, 0, 0, 255]));
    for x in 0..8 {
        for y in 0..16 {
            src.put_pixel(x, y, Rgba([0, 0, 255, 0])); // fully transparent
        }
    }
    src.save(&path).unwrap();
    let mut o = opts(OutputFormat::Jpeg);
    o.background = Some([0, 255, 0]);
    let out = convert(&path, &o).unwrap();
    assert_eq!(codes(&out), ["alpha-flattened"]);
    let back = decode(&out.bytes).to_rgb8();
    let green = back.get_pixel(2, 8).0;
    let red = back.get_pixel(13, 8).0;
    assert!(
        green[1] > 240 && green[0] < 15 && green[2] < 15,
        "{green:?}"
    );
    assert!(red[0] > 240 && red[1] < 15 && red[2] < 15, "{red:?}");
}

#[test]
fn flattening_blends_half_transparency() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    ImageBuffer::from_pixel(8, 8, Rgba([200u8, 200, 200, 128]))
        .save(&path)
        .unwrap();
    let mut o = opts(OutputFormat::Jpeg);
    o.background = Some([0, 0, 0]);
    let out = convert(&path, &o).unwrap();
    let p = decode(&out.bytes).to_rgb8().get_pixel(4, 4).0;
    // 200 * 128/255 = 100.4
    for c in p {
        assert!(c.abs_diff(100) <= 3, "{p:?}");
    }
}

#[test]
fn gray_alpha_is_flattened_too() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    ImageBuffer::from_pixel(8, 8, image::LumaA([100u8, 0]))
        .save(&path)
        .unwrap();
    let mut o = opts(OutputFormat::Jpeg);
    o.background = Some([255, 255, 255]);
    let out = convert(&path, &o).unwrap();
    let p = decode(&out.bytes).to_rgb8().get_pixel(4, 4).0;
    assert!(p.iter().all(|&c| c > 250), "{p:?}");
}

#[test]
fn background_with_png_output_changes_nothing() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    let src = rgba_soft(4, 4);
    src.save(&path).unwrap();
    let mut o = opts(OutputFormat::Png);
    o.background = Some([1, 2, 3]);
    let out = convert(&path, &o).unwrap();
    assert_eq!(decode(&out.bytes).to_rgba8(), src);
}

#[test]
fn parse_color_forms() {
    assert_eq!(parse_color("#ff8000").unwrap(), [255, 128, 0]);
    assert_eq!(parse_color("FF8000").unwrap(), [255, 128, 0]);
    assert_eq!(parse_color("#f80").unwrap(), [255, 136, 0]);
    assert_eq!(parse_color("White").unwrap(), [255, 255, 255]);
    assert_eq!(parse_color("black").unwrap(), [0, 0, 0]);
    for bad in ["", "#", "#ff80", "#gg0000", "red", "#ff80000"] {
        assert!(parse_color(bad).is_err(), "{bad}");
    }
}

// Policy: animated input, metadata, CMYK, formats.

#[test]
fn animated_gif_uses_first_frame_with_warning() {
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame};
    let dir = tmp();
    let path = dir.path().join("a.gif");
    let mut encoder = GifEncoder::new(File::create(&path).unwrap());
    for c in [[250u8, 0, 0], [0, 0, 250]] {
        let buf = ImageBuffer::from_pixel(3, 3, Rgba([c[0], c[1], c[2], 255]));
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
    let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
    assert_eq!(codes(&out), ["animated-first-frame"]);
    assert_eq!(out.notes[0].severity, Severity::Warning);
    let p = decode(&out.bytes).to_rgb8().get_pixel(1, 1).0;
    assert!(p[0] > 200 && p[2] < 50, "{p:?}");
}

#[test]
fn animated_webp_uses_first_frame_with_warning() {
    let dir = tmp();
    let path = dir.path().join("a.webp");
    std::fs::write(&path, ANIMATED_WEBP).unwrap();
    let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
    assert_eq!(codes(&out), ["animated-first-frame"]);
    assert_eq!((out.width, out.height), (2, 2));
}

#[test]
fn animated_png_uses_first_frame_with_warning() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    let mut encoder = png::Encoder::new(File::create(&path).unwrap(), 2, 2);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(2, 0).unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[10; 12]).unwrap();
    writer.write_image_data(&[250; 12]).unwrap();
    drop(writer);
    let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
    assert_eq!(codes(&out), ["animated-first-frame"]);
    assert_eq!(decode(&out.bytes).to_rgb8().get_pixel(0, 0).0, [10, 10, 10]);
}

fn png_with_icc(path: &Path) {
    let mut encoder = PngEncoder::new(File::create(path).unwrap());
    encoder.set_icc_profile(vec![7; 64]).unwrap();
    encoder
        .write_image(&[1, 2, 3], 1, 1, image::ExtendedColorType::Rgb8)
        .unwrap();
}

fn decoder_of(bytes: &[u8]) -> Box<dyn ImageDecoder + '_> {
    Box::new(
        image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .unwrap()
            .into_decoder()
            .unwrap(),
    )
}

#[test]
fn icc_profile_is_dropped_by_default_and_kept_on_request() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    png_with_icc(&path);
    for to in [OutputFormat::Png, OutputFormat::Jpeg] {
        let out = convert(&path, &opts(to)).unwrap();
        assert_eq!(
            decoder_of(&out.bytes).icc_profile().unwrap(),
            None,
            "{to:?}"
        );
        assert_eq!(codes(&out), ["icc-dropped"]);

        let mut o = opts(to);
        o.keep_metadata = true;
        let out = convert(&path, &o).unwrap();
        assert_eq!(
            decoder_of(&out.bytes).icc_profile().unwrap(),
            Some(vec![7; 64]),
            "{to:?}"
        );
        assert!(out.notes.is_empty());
    }
}

/// A minimal EXIF block (little endian) with one Orientation entry.
fn exif_orientation(value: u16) -> Vec<u8> {
    let mut e = b"II*\0".to_vec();
    e.extend_from_slice(&8u32.to_le_bytes());
    e.extend_from_slice(&1u16.to_le_bytes());
    e.extend_from_slice(&0x0112u16.to_le_bytes());
    e.extend_from_slice(&3u16.to_le_bytes());
    e.extend_from_slice(&1u32.to_le_bytes());
    e.extend_from_slice(&value.to_le_bytes());
    e.extend_from_slice(&[0, 0]);
    e.extend_from_slice(&0u32.to_le_bytes());
    e
}

fn jpeg_with_orientation(path: &Path, orientation: u16) {
    // 4 wide, 2 high.
    let img = DynamicImage::ImageRgb8(ImageBuffer::from_fn(4, 2, |x, _| {
        if x < 2 {
            Rgb([255, 0, 0])
        } else {
            Rgb([0, 0, 255])
        }
    }));
    let mut encoder = JpegEncoder::new_with_quality(File::create(path).unwrap(), 100);
    encoder
        .set_exif_metadata(exif_orientation(orientation))
        .unwrap();
    img.write_with_encoder(encoder).unwrap();
}

#[test]
fn exif_orientation_is_applied_when_metadata_is_dropped() {
    let dir = tmp();
    let path = dir.path().join("a.jpg");
    jpeg_with_orientation(&path, 6); // rotate 90 degrees clockwise
    let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
    assert_eq!((out.width, out.height), (2, 4));
    assert!(codes(&out).contains(&"orientation-applied"));
    assert!(codes(&out).contains(&"exif-dropped"));
    assert_eq!(decoder_of(&out.bytes).exif_metadata().unwrap(), None);
}

#[test]
fn exif_is_kept_with_its_orientation_on_request() {
    let dir = tmp();
    let path = dir.path().join("a.jpg");
    jpeg_with_orientation(&path, 6);
    let mut o = opts(OutputFormat::Png);
    o.keep_metadata = true;
    let out = convert(&path, &o).unwrap();
    // Pixels untouched; the orientation travels in the EXIF data.
    assert_eq!((out.width, out.height), (4, 2));
    assert!(out.notes.is_empty());
    assert_eq!(
        decoder_of(&out.bytes).orientation().unwrap(),
        image::metadata::Orientation::Rotate90
    );
}

#[test]
fn undecodable_cmyk_header_is_an_error_not_a_panic() {
    // Header only: cannot be decoded, but must not panic.
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x14, 8, 0, 2, 0, 3, 4];
    for id in 1..=4u8 {
        jpeg.extend_from_slice(&[id, 0x11, 0]);
    }
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    let dir = tmp();
    let path = dir.path().join("a.jpg");
    std::fs::write(&path, &jpeg).unwrap();
    assert!(convert(&path, &opts(OutputFormat::Png)).is_err());
}

#[test]
fn format_is_detected_by_content() {
    let dir = tmp();
    let path = dir.path().join("really-a-png.jpg");
    rgba_opaque(3, 3)
        .save_with_format(&path, image::ImageFormat::Png)
        .unwrap();
    let out = convert(&path, &opts(OutputFormat::Png)).unwrap();
    assert_eq!(out.from, "png");
}

#[test]
fn unknown_and_unreadable_inputs_are_errors() {
    let dir = tmp();
    let text = dir.path().join("a.png");
    std::fs::write(&text, "not an image").unwrap();
    assert!(matches!(
        convert(&text, &opts(OutputFormat::Png)),
        Err(ImageError::UnknownFormat { .. })
    ));
    assert!(matches!(
        convert(&dir.path().join("missing.png"), &opts(OutputFormat::Png)),
        Err(ImageError::Io { .. })
    ));
}

#[test]
fn output_is_identical_across_runs() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    rgba_soft(9, 7).save(&path).unwrap();
    for to in [OutputFormat::Png, OutputFormat::Jpeg] {
        let mut o = opts(to);
        o.background = Some([10, 20, 30]);
        let a = convert(&path, &o).unwrap();
        let b = convert(&path, &o).unwrap();
        assert_eq!(a.bytes, b.bytes);
    }
}
