use super::*;
use image::{ImageBuffer, Rgba};

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Wasteful on purpose: 8-bit RGBA with two colours, so palette and filter
/// reductions have something to win.
fn write_fat_png(path: &Path) {
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(64, 64, |x, y| {
        if (x / 8 + y / 8) % 2 == 0 {
            Rgba([200, 30, 30, 255])
        } else {
            Rgba([10, 10, 200, 255])
        }
    });
    img.save(path).unwrap();
}

fn optimized(outcome: Outcome) -> Optimized {
    match outcome {
        Outcome::Optimized(o) => o,
        Outcome::Skipped(s) => panic!("skipped: {}", s.message),
    }
}

fn pixels(bytes: &[u8]) -> Vec<u16> {
    decoded(bytes).unwrap().to_rgba16().into_raw()
}

fn opts(level: u8, strip: StripMode) -> OptimizeOptions {
    OptimizeOptions { level, strip }
}

#[test]
fn pixels_are_identical_and_the_file_shrinks() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write_fat_png(&path);
    let original = std::fs::read(&path).unwrap();
    for level in [0, DEFAULT_LEVEL, MAX_LEVEL] {
        let o = optimized(optimize(&path, &opts(level, StripMode::Safe)).unwrap());
        assert!(o.smaller, "level {level}");
        assert!(o.optimized_bytes < o.original_bytes);
        assert_eq!(o.optimized_bytes, o.bytes.len() as u64);
        assert_eq!(pixels(&o.bytes), pixels(&original), "level {level}");
        assert_eq!((o.width, o.height), (64, 64));
    }
}

#[test]
fn sixteen_bit_and_translucent_pixels_survive() {
    let dir = tmp();
    let path = dir.path().join("deep.png");
    let img: ImageBuffer<Rgba<u16>, Vec<u16>> = ImageBuffer::from_fn(16, 16, |x, y| {
        Rgba([(x * 4001) as u16, (y * 3001) as u16, 777, (x * 4000) as u16])
    });
    img.save(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    let o = optimized(optimize(&path, &OptimizeOptions::default()).unwrap());
    assert_eq!(pixels(&o.bytes), pixels(&original));
    // Transparent pixels keep their colour.
    let clear: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_fn(8, 8, |x, _| Rgba([x as u8 * 30, 5, 6, 0]));
    let path = dir.path().join("clear.png");
    clear.save(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    let o = optimized(optimize(&path, &OptimizeOptions::default()).unwrap());
    assert_eq!(pixels(&o.bytes), pixels(&original));
}

#[test]
fn optimizing_twice_changes_nothing() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write_fat_png(&path);
    let options = OptimizeOptions::default();
    let first = optimized(optimize(&path, &options).unwrap());
    assert!(first.smaller);
    std::fs::write(&path, &first.bytes).unwrap();
    let second = optimized(optimize(&path, &options).unwrap());
    assert!(!second.smaller);
    assert_eq!(second.bytes, first.bytes);
    assert_eq!(second.optimized_bytes, second.original_bytes);
}

#[test]
fn output_is_deterministic() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write_fat_png(&path);
    let options = opts(MAX_LEVEL, StripMode::Safe);
    let a = optimized(optimize(&path, &options).unwrap());
    let b = optimized(optimize(&path, &options).unwrap());
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn a_result_that_is_not_smaller_returns_the_original_bytes() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write_fat_png(&path);
    let best = optimized(optimize(&path, &opts(MAX_LEVEL, StripMode::All)).unwrap());
    std::fs::write(&path, &best.bytes).unwrap();
    // A weaker setting cannot beat the file that came from the strongest one.
    let o = optimized(optimize(&path, &opts(0, StripMode::All)).unwrap());
    assert!(!o.smaller);
    assert_eq!(o.bytes, best.bytes);
    assert_eq!(o.original_bytes, o.optimized_bytes);
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Inserts a chunk right after IHDR.
fn with_chunk(png: &[u8], name: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut crc_input = name.to_vec();
    crc_input.extend_from_slice(data);
    let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(&crc_input);
    chunk.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    let at = 8 + 25; // signature + IHDR (4 + 4 + 13 + 4)
    let mut out = png[..at].to_vec();
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[at..]);
    out
}

fn chunk_names(png: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut pos = 8;
    while pos + 8 <= png.len() {
        let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
        names.push(String::from_utf8_lossy(&png[pos + 4..pos + 8]).into_owned());
        pos += len + 12;
    }
    names
}

#[test]
fn strip_modes_decide_which_chunks_stay() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write_fat_png(&path);
    let base = std::fs::read(&path).unwrap();
    let mut with = with_chunk(&base, b"tEXt", b"Comment\0hello hello hello");
    with = with_chunk(&with, b"pHYs", &[0, 0, 0x0B, 0x13, 0, 0, 0x0B, 0x13, 1]);
    std::fs::write(&path, &with).unwrap();
    let run = |strip| chunk_names(&optimized(optimize(&path, &opts(2, strip)).unwrap()).bytes);
    assert!(run(StripMode::None).contains(&"tEXt".to_owned()));
    let safe = run(StripMode::Safe);
    assert!(!safe.contains(&"tEXt".to_owned()));
    assert!(safe.contains(&"pHYs".to_owned()));
    let all = run(StripMode::All);
    assert!(!all.contains(&"tEXt".to_owned()));
    assert!(!all.contains(&"pHYs".to_owned()));
}

#[test]
fn non_png_is_skipped_by_content() {
    let dir = tmp();
    let jpg = dir.path().join("a.png"); // the extension lies
    image::RgbImage::new(4, 4)
        .save_with_format(&jpg, image::ImageFormat::Jpeg)
        .unwrap();
    match optimize(&jpg, &OptimizeOptions::default()).unwrap() {
        Outcome::Skipped(s) => assert_eq!(s.code, "not-png"),
        Outcome::Optimized(_) => panic!("a JPEG was optimized"),
    }
    // A PNG with another extension is optimized.
    let png = dir.path().join("a.dat");
    let source = dir.path().join("src.png");
    write_fat_png(&source);
    std::fs::rename(&source, &png).unwrap();
    assert!(matches!(
        optimize(&png, &OptimizeOptions::default()).unwrap(),
        Outcome::Optimized(_)
    ));
}

#[test]
fn corrupt_png_is_an_error_and_a_bad_level_is_refused() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write_fat_png(&path);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.truncate(bytes.len() / 2);
    std::fs::write(&path, &bytes).unwrap();
    assert!(optimize(&path, &OptimizeOptions::default()).is_err());
    let err = optimize(&path, &opts(7, StripMode::Safe)).unwrap_err();
    assert!(err.to_string().contains("between 0 and 6"));
    assert!(optimize(&dir.path().join("missing.png"), &OptimizeOptions::default()).is_err());
}

#[test]
fn animated_png_is_skipped_for_strip_all_only() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write_fat_png(&path);
    let base = std::fs::read(&path).unwrap();
    // acTL: 1 frame, 0 plays. The frame data is not needed to test the skip.
    let mut actl = 1u32.to_be_bytes().to_vec();
    actl.extend_from_slice(&0u32.to_be_bytes());
    std::fs::write(&path, with_chunk(&base, b"acTL", &actl)).unwrap();
    match optimize(&path, &opts(2, StripMode::All)).unwrap() {
        Outcome::Skipped(s) => assert_eq!(s.code, "animated-strip-all"),
        Outcome::Optimized(_) => panic!("strip all on an animation"),
    }
    assert!(!matches!(
        optimize(&path, &opts(2, StripMode::Safe)),
        Ok(Outcome::Skipped(_))
    ));
}

#[test]
fn bytes_variant_matches_the_path_variant() {
    let dir = tmp();
    let path = dir.path().join("a.png");
    write_fat_png(&path);
    let data = std::fs::read(&path).unwrap();
    let options = opts(DEFAULT_LEVEL, StripMode::Safe);
    let from_path = optimized(optimize(&path, &options).unwrap());
    // The label does not exist on disk: it is never opened.
    let label = dir.path().join("label.png");
    let from_bytes = optimized(optimize_bytes(&label, &data, &options).unwrap());
    assert_eq!(from_bytes.bytes, from_path.bytes);
    assert_eq!(from_bytes.original_bytes, from_path.original_bytes);
    assert_eq!(from_bytes.optimized_bytes, from_path.optimized_bytes);
    assert_eq!(from_bytes.smaller, from_path.smaller);
}

#[test]
fn bytes_variant_errors_name_the_label() {
    let label = Path::new("some/label.png");
    let mut data = PNG_SIGNATURE.to_vec();
    data.extend_from_slice(b"garbage");
    let error = optimize_bytes(label, &data, &opts(DEFAULT_LEVEL, StripMode::Safe)).unwrap_err();
    assert!(error.to_string().contains("label.png"), "{error}");
}
