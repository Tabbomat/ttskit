use super::*;
use image::{ImageBuffer, Rgb, Rgba};

fn rules(report: &CheckReport) -> Vec<&'static str> {
    report.findings.iter().map(|f| f.rule).collect()
}

fn save_rgb(path: &Path, w: u32, h: u32) {
    ImageBuffer::from_pixel(w, h, Rgb([1u8, 2, 3]))
        .save(path)
        .unwrap();
}

fn save_rgb_as(path: &Path, format: image::ImageFormat) {
    ImageBuffer::from_pixel(4, 4, Rgb([1u8, 2, 3]))
        .save_with_format(path, format)
        .unwrap();
}

fn save_rgba(path: &Path, alpha: impl Fn(u32, u32) -> u8) {
    ImageBuffer::from_fn(4, 4, |x, y| Rgba([10, 20, 30, alpha(x, y)]))
        .save(path)
        .unwrap();
}

#[test]
fn clean_image_has_no_findings_and_reports_facts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    save_rgb(&path, 30, 20);
    let report = check(&path, &CheckOptions::default()).unwrap();
    assert_eq!(
        (report.format.as_str(), report.width, report.height),
        ("png", 30, 20)
    );
    assert_eq!(report.file_bytes, std::fs::metadata(&path).unwrap().len());
    assert!(!report.has_alpha);
    assert!(report.findings.is_empty());
}

#[test]
fn side_limit_is_inclusive_and_applies_to_either_side() {
    let dir = tempfile::tempdir().unwrap();
    let options = CheckOptions {
        max_side: 10,
        max_bytes: None,
    };
    let cases = [((10, 4), false), ((11, 4), true), ((4, 11), true)];
    for (i, ((w, h), flagged)) in cases.into_iter().enumerate() {
        let path = dir.path().join(format!("{i}.png"));
        save_rgb(&path, w, h);
        let report = check(&path, &options).unwrap();
        let expected: &[&str] = if flagged { &["side-too-long"] } else { &[] };
        assert_eq!(rules(&report), expected, "{w}x{h}");
    }
}

#[test]
fn default_side_limit_is_4096() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    save_rgb(&path, 4097, 1);
    assert_eq!(
        rules(&check(&path, &CheckOptions::default()).unwrap()),
        ["side-too-long"]
    );
    save_rgb(&path, 4096, 1);
    assert!(
        check(&path, &CheckOptions::default())
            .unwrap()
            .findings
            .is_empty()
    );
}

#[test]
fn byte_limit_only_applies_when_given() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    save_rgb(&path, 8, 8);
    let size = std::fs::metadata(&path).unwrap().len();
    let at = CheckOptions {
        max_bytes: Some(size),
        ..CheckOptions::default()
    };
    assert!(check(&path, &at).unwrap().findings.is_empty());
    let below = CheckOptions {
        max_bytes: Some(size - 1),
        ..CheckOptions::default()
    };
    assert_eq!(rules(&check(&path, &below).unwrap()), ["file-too-large"]);
    assert!(
        check(&path, &CheckOptions::default())
            .unwrap()
            .findings
            .is_empty()
    );
}

#[test]
fn extension_must_match_the_content() {
    let dir = tempfile::tempdir().unwrap();
    for (name, flagged) in [
        ("a.PNG", false),
        ("a.png", false),
        ("a.jpg", true),
        ("a.webp", true),
        ("a.dat", true),
        ("noext", true),
    ] {
        let path = dir.path().join(name);
        save_rgb_as(&path, image::ImageFormat::Png);
        let report = check(&path, &CheckOptions::default()).unwrap();
        let expected: &[&str] = if flagged {
            &["extension-mismatch"]
        } else {
            &[]
        };
        assert_eq!(rules(&report), expected, "{name}");
        assert_eq!(report.format, "png", "{name}");
    }
}

#[test]
fn jpeg_extensions_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.jpg", "a.jpeg", "a.JPG"] {
        let path = dir.path().join(name);
        save_rgb_as(&path, image::ImageFormat::Jpeg);
        let report = check(&path, &CheckOptions::default()).unwrap();
        assert_eq!(report.format, "jpeg");
        assert!(report.findings.is_empty(), "{name}");
    }
    let wrong = dir.path().join("b.png");
    save_rgb_as(&wrong, image::ImageFormat::Jpeg);
    assert_eq!(
        rules(&check(&wrong, &CheckOptions::default()).unwrap()),
        ["extension-mismatch"]
    );
}

#[test]
fn fully_opaque_alpha_channel_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    save_rgba(&path, |_, _| 255);
    let report = check(&path, &CheckOptions::default()).unwrap();
    assert!(report.has_alpha);
    assert_eq!(rules(&report), ["alpha-opaque"]);
}

#[test]
fn real_transparency_is_not_flagged() {
    let dir = tempfile::tempdir().unwrap();
    type AlphaFn = fn(u32, u32) -> u8;
    let cases: [(&str, AlphaFn); 3] = [
        ("hole.png", |x, _| if x == 0 { 0 } else { 255 }),
        (
            "one_pixel.png",
            |x, y| {
                if (x, y) == (3, 3) { 254 } else { 255 }
            },
        ),
        ("soft.png", |_, _| 128),
    ];
    for (name, alpha) in cases {
        let path = dir.path().join(name);
        save_rgba(&path, alpha);
        let report = check(&path, &CheckOptions::default()).unwrap();
        assert!(report.has_alpha && report.findings.is_empty(), "{name}");
    }
}

#[test]
fn opaque_16_bit_and_gray_alpha_are_checked_too() {
    let dir = tempfile::tempdir().unwrap();
    let wide = dir.path().join("wide.png");
    ImageBuffer::<Rgba<u16>, Vec<u16>>::from_pixel(2, 2, Rgba([1, 2, 3, u16::MAX]))
        .save(&wide)
        .unwrap();
    assert_eq!(
        rules(&check(&wide, &CheckOptions::default()).unwrap()),
        ["alpha-opaque"]
    );
    let almost = dir.path().join("almost.png");
    ImageBuffer::<Rgba<u16>, Vec<u16>>::from_pixel(2, 2, Rgba([1, 2, 3, u16::MAX - 1]))
        .save(&almost)
        .unwrap();
    assert!(
        check(&almost, &CheckOptions::default())
            .unwrap()
            .findings
            .is_empty()
    );
    let gray = dir.path().join("gray.png");
    ImageBuffer::from_pixel(2, 2, image::LumaA([9u8, 255]))
        .save(&gray)
        .unwrap();
    assert_eq!(
        rules(&check(&gray, &CheckOptions::default()).unwrap()),
        ["alpha-opaque"]
    );
}

#[test]
fn image_without_alpha_channel_is_not_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    save_rgb(&path, 4, 4);
    assert!(!check(&path, &CheckOptions::default()).unwrap().has_alpha);
}

#[test]
fn findings_come_together_and_unreadable_files_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.jpg");
    save_rgb_as(&path, image::ImageFormat::Png);
    let options = CheckOptions {
        max_side: 2,
        max_bytes: Some(1),
    };
    assert_eq!(
        rules(&check(&path, &options).unwrap()),
        ["side-too-long", "file-too-large", "extension-mismatch"]
    );
    let text = dir.path().join("t.png");
    std::fs::write(&text, b"nope").unwrap();
    assert!(matches!(
        check(&text, &options),
        Err(ImageError::UnknownFormat { .. })
    ));
    assert!(matches!(
        check(&dir.path().join("missing.png"), &options),
        Err(ImageError::Io { .. })
    ));
}
