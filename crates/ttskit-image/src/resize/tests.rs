use super::*;
use image::{ImageBuffer, Luma, Rgb, Rgba};

fn resized(outcome: Outcome) -> Resized {
    match outcome {
        Outcome::Resized(r) => r,
        other => panic!("expected a resized image, got {other:?}"),
    }
}

fn unchanged(outcome: Outcome) -> Unchanged {
    match outcome {
        Outcome::Unchanged(u) => u,
        other => panic!("expected an unchanged image, got {other:?}"),
    }
}

fn gradient(path: &Path, w: u32, h: u32) {
    ImageBuffer::from_fn(w, h, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 90]))
        .save(path)
        .unwrap();
}

fn dims(bytes: &[u8]) -> (u32, u32) {
    let img = image::load_from_memory(bytes).unwrap();
    (img.width(), img.height())
}

#[test]
fn target_sizes_keep_the_aspect_ratio() {
    let size = |w, h, t| target_size(w, h, t).unwrap();
    assert_eq!(size(400, 200, Target::MaxSide(100)), (100, 50));
    assert_eq!(size(200, 400, Target::MaxSide(100)), (50, 100));
    assert_eq!(size(400, 200, Target::Width(100)), (100, 50));
    assert_eq!(size(400, 200, Target::Height(50)), (100, 50));
    assert_eq!(size(400, 200, Target::Scale(0.25)), (100, 50));
    // Rounded to the nearest pixel, and never below 1.
    assert_eq!(size(100, 3, Target::MaxSide(10)), (10, 1));
    assert_eq!(size(7, 5, Target::Width(3)), (3, 2));
    assert_eq!(size(1000, 1, Target::Width(10)), (10, 1));
}

#[test]
fn bad_targets_are_errors() {
    for target in [
        Target::MaxSide(0),
        Target::Width(0),
        Target::Height(0),
        Target::Scale(0.0),
        Target::Scale(-1.0),
        Target::Scale(f64::NAN),
        Target::Scale(f64::INFINITY),
        Target::MaxSide(MAX_RESULT_SIDE + 1),
    ] {
        assert!(
            matches!(target_size(10, 10, target), Err(ImageError::Options(_))),
            "{target:?}"
        );
    }
}

#[test]
fn shrinks_png_keeping_the_aspect_ratio() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    gradient(&path, 120, 60);
    let r = resized(resize(&path, &ResizeOptions::new(Target::MaxSide(30))).unwrap());
    assert_eq!((r.original_width, r.original_height), (120, 60));
    assert_eq!((r.width, r.height), (30, 15));
    assert_eq!(r.format, "png");
    assert_eq!(dims(&r.bytes), (30, 15));
    assert_eq!(r.original_bytes, std::fs::metadata(&path).unwrap().len());
}

#[test]
fn every_target_kind_shrinks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    gradient(&path, 80, 40);
    for (target, expected) in [
        (Target::MaxSide(40), (40, 20)),
        (Target::Width(20), (20, 10)),
        (Target::Height(10), (20, 10)),
        (Target::Scale(0.5), (40, 20)),
    ] {
        let r = resized(resize(&path, &ResizeOptions::new(target)).unwrap());
        assert_eq!((r.width, r.height), expected, "{target:?}");
    }
}

#[test]
fn does_not_enlarge_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    gradient(&path, 40, 20);
    for target in [
        Target::MaxSide(100),
        Target::Width(80),
        Target::Height(40),
        Target::Scale(2.0),
    ] {
        let u = unchanged(resize(&path, &ResizeOptions::new(target)).unwrap());
        assert_eq!(u.code, "would-enlarge", "{target:?}");
        assert_eq!((u.width, u.height), (40, 20));
    }
}

#[test]
fn enlarges_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    gradient(&path, 40, 20);
    let options = ResizeOptions {
        allow_enlarge: true,
        ..ResizeOptions::new(Target::MaxSide(80))
    };
    let r = resized(resize(&path, &options).unwrap());
    assert_eq!((r.width, r.height), (80, 40));
}

#[test]
fn same_size_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    gradient(&path, 40, 20);
    for target in [Target::MaxSide(40), Target::Scale(1.0)] {
        let u = unchanged(resize(&path, &ResizeOptions::new(target)).unwrap());
        assert_eq!(u.code, "already-that-size");
    }
}

#[test]
fn two_runs_give_identical_bytes_for_every_filter() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    let jpg = dir.path().join("a.jpg");
    gradient(&png, 90, 50);
    gradient(&jpg, 90, 50);
    for path in [&png, &jpg] {
        for filter in [
            Filter::Lanczos3,
            Filter::CatmullRom,
            Filter::Triangle,
            Filter::Nearest,
        ] {
            let options = ResizeOptions {
                filter,
                ..ResizeOptions::new(Target::MaxSide(30))
            };
            let a = resized(resize(path, &options).unwrap()).bytes;
            let b = resized(resize(path, &options).unwrap()).bytes;
            assert_eq!(a, b, "{path:?} {filter:?}");
        }
    }
}

#[test]
fn jpeg_stays_jpeg_and_quality_matters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.jpg");
    gradient(&path, 100, 100);
    let low = ResizeOptions {
        jpeg_quality: 10,
        ..ResizeOptions::new(Target::MaxSide(50))
    };
    let high = ResizeOptions {
        jpeg_quality: 95,
        ..ResizeOptions::new(Target::MaxSide(50))
    };
    let low = resized(resize(&path, &low).unwrap());
    let high = resized(resize(&path, &high).unwrap());
    assert_eq!(low.format, "jpeg");
    assert_eq!(image::guess_format(&low.bytes).unwrap(), ImageFormat::Jpeg);
    assert_eq!(dims(&low.bytes), (50, 50));
    assert!(low.bytes.len() < high.bytes.len());
    let bad = ResizeOptions {
        jpeg_quality: 0,
        ..ResizeOptions::new(Target::MaxSide(50))
    };
    assert!(matches!(resize(&path, &bad), Err(ImageError::Options(_))));
}

#[test]
fn other_formats_are_skipped_by_content() {
    let dir = tempfile::tempdir().unwrap();
    let webp = dir.path().join("a.png"); // the extension lies
    ImageBuffer::from_pixel(4, 4, Rgba([1u8, 2, 3, 255]))
        .save_with_format(&webp, ImageFormat::WebP)
        .unwrap();
    match resize(&webp, &ResizeOptions::new(Target::MaxSide(2))).unwrap() {
        Outcome::Skipped(s) => {
            assert_eq!(s.code, "unsupported-format");
            assert!(s.message.contains("webp") && s.message.contains("image convert"));
        }
        other => panic!("{other:?}"),
    }
    let text = dir.path().join("t.png");
    std::fs::write(&text, b"not an image").unwrap();
    assert!(matches!(
        resize(&text, &ResizeOptions::new(Target::MaxSide(2))),
        Err(ImageError::UnknownFormat { .. })
    ));
}

#[test]
fn alpha_is_resized_and_transparent_pixels_do_not_bleed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    // Left half opaque red, right half fully transparent black.
    ImageBuffer::from_fn(40, 40, |x, _| {
        if x < 20 {
            Rgba([255u8, 0, 0, 255])
        } else {
            Rgba([0, 0, 0, 0])
        }
    })
    .save(&path)
    .unwrap();
    let r = resized(resize(&path, &ResizeOptions::new(Target::MaxSide(10))).unwrap());
    let img = image::load_from_memory(&r.bytes).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (10, 10));
    // The opaque core stays exactly opaque red: no dark fringe from the
    // transparent black neighbours.
    for x in 0..2 {
        assert_eq!(img.get_pixel(x, 5).0, [255, 0, 0, 255], "x={x}");
    }
    assert_eq!(img.get_pixel(9, 5).0[3], 0);
    // Any partly transparent pixel on the edge is still pure red.
    for p in img.pixels().filter(|p| p.0[3] > 0) {
        assert_eq!(&p.0[..3], &[255, 0, 0]);
    }
}

#[test]
fn bit_depth_and_layout_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let wide = dir.path().join("wide.png");
    ImageBuffer::<Rgba<u16>, Vec<u16>>::from_pixel(8, 8, Rgba([1000, 2000, 3000, u16::MAX]))
        .save(&wide)
        .unwrap();
    let r = resized(resize(&wide, &ResizeOptions::new(Target::MaxSide(4))).unwrap());
    assert_eq!(
        image::load_from_memory(&r.bytes).unwrap().color(),
        ColorType::Rgba16
    );
    let gray = dir.path().join("gray.png");
    ImageBuffer::from_pixel(8, 8, Luma([77u8]))
        .save(&gray)
        .unwrap();
    let r = resized(resize(&gray, &ResizeOptions::new(Target::MaxSide(4))).unwrap());
    let out = image::load_from_memory(&r.bytes).unwrap();
    assert_eq!(out.color(), ColorType::L8);
    assert_eq!(out.to_luma8().get_pixel(1, 1).0, [77]);
    let gray_alpha = dir.path().join("ga.png");
    ImageBuffer::from_pixel(8, 8, image::LumaA([77u8, 255]))
        .save(&gray_alpha)
        .unwrap();
    let r = resized(resize(&gray_alpha, &ResizeOptions::new(Target::MaxSide(4))).unwrap());
    assert_eq!(
        image::load_from_memory(&r.bytes).unwrap().color(),
        ColorType::La8
    );
}

#[test]
fn fully_opaque_image_stays_fully_opaque() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    ImageBuffer::from_fn(64, 64, |x, y| Rgba([(x * 4) as u8, (y * 4) as u8, 9, 255]))
        .save(&path)
        .unwrap();
    for filter in [Filter::Lanczos3, Filter::CatmullRom, Filter::Triangle] {
        let options = ResizeOptions {
            filter,
            ..ResizeOptions::new(Target::MaxSide(16))
        };
        let r = resized(resize(&path, &options).unwrap());
        let img = image::load_from_memory(&r.bytes).unwrap().to_rgba8();
        assert!(img.pixels().all(|p| p.0[3] == 255), "{filter:?}");
    }
}

#[test]
fn metadata_is_dropped_with_notes() {
    use image::ImageEncoder;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    let mut bytes = Vec::new();
    let mut encoder = PngEncoder::new(&mut bytes);
    encoder.set_icc_profile(vec![7; 32]).unwrap();
    encoder
        .write_image(&[9; 4 * 4 * 3], 4, 4, image::ExtendedColorType::Rgb8)
        .unwrap();
    std::fs::write(&path, bytes).unwrap();
    let r = resized(resize(&path, &ResizeOptions::new(Target::MaxSide(2))).unwrap());
    assert_eq!(
        r.notes.iter().map(|n| n.code).collect::<Vec<_>>(),
        ["icc-dropped"]
    );
    assert!(!r.bytes.windows(4).any(|w| w == b"iCCP"));
}

#[test]
fn animated_png_uses_the_first_frame_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    let mut encoder = png::Encoder::new(std::fs::File::create(&path).unwrap(), 8, 8);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_animated(2, 0).unwrap();
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[0; 8 * 8 * 3]).unwrap();
    writer.write_image_data(&[255; 8 * 8 * 3]).unwrap();
    drop(writer);
    let r = resized(resize(&path, &ResizeOptions::new(Target::MaxSide(4))).unwrap());
    assert_eq!(
        r.notes.iter().map(|n| n.code).collect::<Vec<_>>(),
        ["animated-first-frame"]
    );
    assert_eq!(dims(&r.bytes), (4, 4));
}

#[test]
fn bytes_variant_matches_the_path_variant() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    gradient(&path, 40, 20);
    let data = std::fs::read(&path).unwrap();
    let options = ResizeOptions::new(Target::MaxSide(10));
    let from_path = resized(resize(&path, &options).unwrap());
    let label = dir.path().join("label.png");
    let from_bytes = resized(resize_bytes(&label, &data, &options).unwrap());
    assert_eq!(from_bytes.bytes, from_path.bytes);
    assert_eq!(from_bytes.original_bytes, from_path.original_bytes);
    assert_eq!(
        (from_bytes.width, from_bytes.height),
        (from_path.width, from_path.height)
    );
}

#[test]
fn bytes_variant_errors_name_the_label() {
    let label = Path::new("some/label.png");
    let options = ResizeOptions::new(Target::MaxSide(10));
    let error = resize_bytes(label, b"not an image", &options).unwrap_err();
    assert!(error.to_string().contains("label.png"), "{error}");
}
