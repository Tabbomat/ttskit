use super::*;
use image::{ImageBuffer, Rgb, Rgba};

const CARD: (u32, u32) = (3, 4);

/// A card whose every pixel differs, so that a shifted cut is noticed.
fn card_image(k: u32) -> image::RgbImage {
    image::RgbImage::from_fn(CARD.0, CARD.1, |x, y| {
        Rgb([
            (k * 3 + x) as u8,
            (250u32.wrapping_sub(k * 2) + y) as u8,
            (k * 7 + x * 5 + y) as u8,
        ])
    })
}

fn cards(dir: &Path, n: u32) -> Vec<PathBuf> {
    (0..n)
        .map(|k| {
            let path = dir.join(format!("card{k}.png"));
            card_image(k).save(&path).unwrap();
            path
        })
        .collect()
}

fn options(columns: u32, rows: u32) -> SplitOptions {
    SplitOptions {
        columns,
        rows,
        count: None,
        hidden_slot: true,
        skip_hidden: false,
        format: OutputFormat::Png,
        quality: DEFAULT_JPEG_QUALITY,
        fill: [0x80; 3],
    }
}

fn decoded(slot: &SplitSlot) -> image::RgbImage {
    image::load_from_memory(&slot.bytes).unwrap().to_rgb8()
}

fn error_text(result: Result<Split, ImageError>) -> String {
    result.expect_err("expected an error").to_string()
}

/// Builds `n` cards into one sheet in `dir` and returns its path and grid.
fn built(dir: &Path, n: u32, deck_options: &DeckOptions) -> (PathBuf, u32, u32) {
    let deck = build_deck(&cards(dir, n), deck_options).unwrap();
    assert_eq!(deck.sheets.len(), 1);
    let sheet = &deck.sheets[0];
    let path = dir.join("sheet.png");
    std::fs::write(&path, &sheet.bytes).unwrap();
    (path, sheet.columns, sheet.rows)
}

#[test]
fn a_built_sheet_splits_back_into_its_cards() {
    for (n, hidden_slot) in [(6, true), (69, true), (6, false), (70, false)] {
        let dir = tempfile::tempdir().unwrap();
        let back = dir.path().join("back.png");
        let back_image = card_image(200);
        back_image.save(&back).unwrap();
        let deck_options = DeckOptions {
            hidden: hidden_slot.then_some(back),
            hidden_slot,
            ..DeckOptions::default()
        };
        let (sheet, columns, rows) = built(dir.path(), n, &deck_options);
        let mut split_options = options(columns, rows);
        split_options.hidden_slot = hidden_slot;
        split_options.count = Some(n);
        let split = split_sheet(&sheet, &split_options).unwrap();
        assert_eq!((split.cell_width, split.cell_height), CARD);
        assert_eq!(split.slots.len() as u32, n + u32::from(hidden_slot));
        for (k, slot) in split.slots.iter().take(n as usize).enumerate() {
            let k = k as u32;
            assert_eq!(
                (slot.slot, slot.column, slot.row, slot.is_hidden),
                (k, k % columns, k / columns, false)
            );
            assert_eq!(decoded(slot), card_image(k), "{n} cards, card {k}");
        }
        if hidden_slot {
            let last = split.slots.last().unwrap();
            assert!(last.is_hidden);
            assert_eq!(last.slot, columns * rows - 1);
            assert_eq!(decoded(last), back_image);
        }
    }
}

#[test]
fn count_limits_the_cards() {
    let dir = tempfile::tempdir().unwrap();
    let (sheet, columns, rows) = built(dir.path(), 6, &DeckOptions::default());
    let mut o = options(columns, rows);
    o.count = Some(2);
    let split = split_sheet(&sheet, &o).unwrap();
    let slots: Vec<(u32, bool)> = split.slots.iter().map(|s| (s.slot, s.is_hidden)).collect();
    assert_eq!(slots, [(0, false), (1, false), (columns * rows - 1, true)]);

    o.count = Some(0);
    assert!(error_text(split_sheet(&sheet, &o)).contains("--count"));
    // 3x3 with a hidden slot has 8 card slots.
    o.count = Some(9);
    let text = error_text(split_sheet(&sheet, &o));
    assert!(text.contains("8 card slots"), "{text}");
    o.count = Some(8);
    assert!(split_sheet(&sheet, &o).is_ok());
    o.hidden_slot = false;
    o.count = Some(9);
    assert!(split_sheet(&sheet, &o).is_ok());
    o.count = Some(10);
    assert!(error_text(split_sheet(&sheet, &o)).contains("9 card slots"));
}

#[test]
fn the_hidden_slot_is_written_skipped_or_a_card() {
    let dir = tempfile::tempdir().unwrap();
    let (sheet, columns, rows) = built(dir.path(), 3, &DeckOptions::default());
    assert_eq!((columns, rows), (2, 2));
    let mut o = options(2, 2);
    let split = split_sheet(&sheet, &o).unwrap();
    let flags: Vec<bool> = split.slots.iter().map(|s| s.is_hidden).collect();
    assert_eq!(flags, [false, false, false, true]);
    // The unused hidden slot has the fill colour.
    assert!(decoded(&split.slots[3]).pixels().all(|p| p.0 == [0x80; 3]));

    o.skip_hidden = true;
    let split = split_sheet(&sheet, &o).unwrap();
    assert_eq!(split.slots.len(), 3);
    assert!(split.slots.iter().all(|s| !s.is_hidden));

    o.skip_hidden = false;
    o.hidden_slot = false;
    let split = split_sheet(&sheet, &o).unwrap();
    assert_eq!(split.slots.len(), 4);
    assert!(split.slots.iter().all(|s| !s.is_hidden));
    assert_eq!(split.slots[3].slot, 3);

    // A single slot with a hidden slot has no card.
    let one = dir.path().join("one.png");
    card_image(0).save(&one).unwrap();
    let mut o = options(1, 1);
    let text = error_text(split_sheet(&one, &o));
    assert!(text.contains("no card slot"), "{text}");
    o.hidden_slot = false;
    assert_eq!(split_sheet(&one, &o).unwrap().slots.len(), 1);
}

#[test]
fn file_names_are_padded_by_the_grid() {
    let names = |columns, rows, count, hidden| {
        let plan = plan_split(columns, rows, count, hidden, false).unwrap();
        plan.slots
            .iter()
            .map(|s| plan.file_name("deck", s, "png"))
            .collect::<Vec<_>>()
    };
    let full = names(10, 7, None, true);
    assert_eq!(full.len(), 70);
    assert_eq!(full[0], "deck_00.png");
    assert_eq!(full[68], "deck_68.png");
    assert_eq!(full[69], "deck_hidden.png");
    let no_hidden = names(10, 7, None, false);
    assert_eq!(no_hidden[69], "deck_69.png");
    assert_eq!(
        names(2, 2, None, true),
        [
            "deck_00.png",
            "deck_01.png",
            "deck_02.png",
            "deck_hidden.png"
        ]
    );
    // --count does not change the padding of the names.
    assert_eq!(names(10, 7, Some(2), true)[..2], full[..2]);
    let plan = plan_split(1, 1, None, false, false).unwrap();
    assert_eq!(plan.file_name("a", &plan.slots[0], "jpg"), "a_00.jpg");
}

#[test]
fn bad_grids_are_errors() {
    let text = |c, r| plan_split(c, r, None, true, false).unwrap_err().to_string();
    assert!(text(0, 2).contains("between 1 and 10"));
    assert!(text(11, 2).contains("between 1 and 10"));
    assert!(text(2, 8).contains("between 1 and 7"));
    let o = options(2, 2);
    let dir = tempfile::tempdir().unwrap();
    let (sheet, ..) = built(dir.path(), 3, &DeckOptions::default());
    for quality in [0, 101] {
        let bad = SplitOptions {
            quality,
            ..o.clone()
        };
        assert!(error_text(split_sheet(&sheet, &bad)).contains("quality"));
    }
}

#[test]
fn a_sheet_that_does_not_divide_is_an_error_naming_the_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("odd.png");
    ImageBuffer::from_pixel(7, 5, Rgb([1u8, 2, 3]))
        .save(&path)
        .unwrap();
    let text = error_text(split_sheet(&path, &options(2, 2)));
    assert!(text.contains("7x5"), "{text}");
    assert!(text.contains("2 columns x 2 rows"), "{text}");
    assert!(text.contains("1 px over"), "{text}");
    // Only the height does not divide.
    let text = error_text(split_sheet(&path, &options(7, 2)));
    assert!(
        text.contains("width leaves 0 px over, height 1 px"),
        "{text}"
    );
    // Fewer pixels than columns.
    let tiny = dir.path().join("tiny.png");
    ImageBuffer::from_pixel(3, 3, Rgb([1u8, 2, 3]))
        .save(&tiny)
        .unwrap();
    assert!(split_sheet(&tiny, &options(10, 1)).is_err());
}

#[test]
fn one_row_and_one_column_sheets() {
    let dir = tempfile::tempdir().unwrap();
    for (columns, rows) in [(1, 5), (5, 1)] {
        let deck_options = DeckOptions {
            columns: Some(columns),
            rows: Some(rows),
            ..DeckOptions::default()
        };
        let (sheet, c, r) = built(dir.path(), 4, &deck_options);
        assert_eq!((c, r), (columns, rows));
        let split = split_sheet(&sheet, &options(columns, rows)).unwrap();
        assert_eq!(split.slots.len(), 5);
        for k in 0..4 {
            assert_eq!(decoded(&split.slots[k as usize]), card_image(k));
        }
        assert!(split.slots[4].is_hidden);
        let last = &split.slots[3];
        assert_eq!(
            (last.column, last.row),
            if columns == 1 { (0, 3) } else { (3, 0) }
        );
    }
}

#[test]
fn output_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let (sheet, columns, rows) = built(dir.path(), 5, &DeckOptions::default());
    for format in [OutputFormat::Png, OutputFormat::Jpeg] {
        let o = SplitOptions {
            format,
            ..options(columns, rows)
        };
        let a = split_sheet(&sheet, &o).unwrap();
        let b = split_sheet(&sheet, &o).unwrap();
        assert_eq!(a.slots.len(), b.slots.len());
        for (x, y) in a.slots.iter().zip(&b.slots) {
            assert_eq!(x.bytes, y.bytes);
        }
    }
}

#[test]
fn jpeg_output_flattens_transparency_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alpha.png");
    ImageBuffer::from_fn(4, 2, |x, _| {
        if x < 2 {
            Rgba([255u8, 0, 0, 255])
        } else {
            Rgba([0u8, 0, 0, 0])
        }
    })
    .save(&path)
    .unwrap();
    let o = SplitOptions {
        format: OutputFormat::Jpeg,
        hidden_slot: false,
        fill: [255, 255, 255],
        ..options(2, 1)
    };
    let split = split_sheet(&path, &o).unwrap();
    let note = split
        .notes
        .iter()
        .find(|n| n.code == "alpha-flattened")
        .expect("note");
    assert!(note.message.contains("4 transparent"), "{}", note.message);
    assert!(note.message.contains("#ffffff"), "{}", note.message);
    let img = image::load_from_memory(&split.slots[1].bytes)
        .unwrap()
        .to_rgb8();
    assert!(img.pixels().all(|p| p.0.iter().all(|&c| c > 250)));
    assert_eq!(&split.slots[1].bytes[..2], [0xff, 0xd8]);

    // No transparency, no note; PNG keeps the alpha channel.
    let png = SplitOptions {
        hidden_slot: false,
        ..options(2, 1)
    };
    let split = split_sheet(&path, &png).unwrap();
    assert!(split.notes.is_empty());
    let cell = image::load_from_memory(&split.slots[1].bytes).unwrap();
    assert!(cell.color().has_alpha());
    assert_eq!(cell.to_rgba8().get_pixel(0, 0).0, [0, 0, 0, 0]);
}

#[test]
fn png_keeps_16_bits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep.png");
    ImageBuffer::from_fn(4, 2, |x, y| {
        Rgb([x as u16 * 1000 + 257, y as u16 * 300 + 3, 65535])
    })
    .save(&path)
    .unwrap();
    let o = SplitOptions {
        hidden_slot: false,
        ..options(2, 1)
    };
    let split = split_sheet(&path, &o).unwrap();
    assert!(split.notes.is_empty(), "{:?}", split.notes);
    let cell = image::load_from_memory(&split.slots[1].bytes).unwrap();
    assert_eq!(cell.color(), image::ColorType::Rgb16);
    assert_eq!(cell.to_rgb16().get_pixel(1, 1).0, [3257, 303, 65535]);

    let jpeg = SplitOptions {
        format: OutputFormat::Jpeg,
        ..o
    };
    let split = split_sheet(&path, &jpeg).unwrap();
    assert!(split.notes.iter().any(|n| n.code == "reduced-to-8-bit"));
}

#[test]
fn other_formats_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.bmp");
    ImageBuffer::from_pixel(4, 4, Rgb([1u8, 2, 3]))
        .save(&path)
        .unwrap();
    let text = error_text(split_sheet(&path, &options(2, 2)));
    assert!(text.contains("bmp"), "{text}");
    assert!(split_sheet(&dir.path().join("missing.png"), &options(2, 2)).is_err());
}
