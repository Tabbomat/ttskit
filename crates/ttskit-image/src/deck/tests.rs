use super::*;
use image::{ImageBuffer, Rgb};

const CARD: (u32, u32) = (3, 4);

fn colour(k: u32) -> [u8; 3] {
    [(k * 3 + 1) as u8, (250 - k * 2) as u8, (k * 7 % 251) as u8]
}

fn solid(path: &Path, (w, h): (u32, u32), rgb: [u8; 3]) {
    ImageBuffer::from_pixel(w, h, Rgb(rgb)).save(path).unwrap();
}

/// `n` cards, each a different solid colour, in `dir` as `card0.png`...
fn cards(dir: &Path, n: u32) -> Vec<PathBuf> {
    (0..n)
        .map(|k| {
            let path = dir.join(format!("card{k}.png"));
            solid(&path, CARD, colour(k));
            path
        })
        .collect()
}

fn decode(sheet: &Sheet) -> image::RgbImage {
    image::load_from_memory(&sheet.bytes).unwrap().to_rgb8()
}

/// Asserts that the cell of `slot` is entirely `rgb`.
fn assert_cell(img: &image::RgbImage, columns: u32, cell: (u32, u32), slot: u32, rgb: [u8; 3]) {
    let (c, r) = slot_origin(slot, columns);
    for y in 0..cell.1 {
        for x in 0..cell.0 {
            let p = img.get_pixel(c * cell.0 + x, r * cell.1 + y).0;
            assert_eq!(p, rgb, "slot {slot} pixel {x},{y}");
        }
    }
}

fn build(dir: &Path, n: u32, options: &DeckOptions) -> Deck {
    build_deck(&cards(dir, n), options).unwrap()
}

fn error_text(result: Result<Deck, ImageError>) -> String {
    result.expect_err("expected an error").to_string()
}

#[test]
fn automatic_grid_always_holds_the_slots() {
    for hidden in [true, false] {
        for cards in 1..=(70 - u32::from(hidden)) {
            let slots = cards + u32::from(hidden);
            let (c, r) = grid_for(cards, hidden, None, None);
            assert!((2..=MAX_COLUMNS).contains(&c), "{cards} cards: {c}x{r}");
            assert!((2..=MAX_ROWS).contains(&r), "{cards} cards: {c}x{r}");
            assert!(
                c * r >= slots,
                "{cards} cards need {slots} slots, got {c}x{r}"
            );
        }
    }
}

#[test]
fn automatic_grid_table() {
    // (cards, hidden slot, columns, rows)
    let table = [
        (1, true, 2, 2),
        (3, true, 2, 2),
        (4, true, 3, 2),
        (8, true, 3, 3),
        (1, false, 2, 2),
        (4, false, 2, 2),
        (5, false, 3, 2),
        (69, true, 10, 7),
        (70, false, 10, 7),
    ];
    for (cards, hidden, columns, rows) in table {
        assert_eq!(
            grid_for(cards, hidden, None, None),
            (columns, rows),
            "{cards} cards, hidden slot {hidden}"
        );
    }
}

#[test]
fn given_dimensions_keep_the_other_one_minimal() {
    assert_eq!(grid_for(5, true, Some(1), None), (1, 6));
    assert_eq!(grid_for(5, false, None, Some(1)), (5, 1));
    assert_eq!(grid_for(5, true, Some(4), None), (4, 2));
    assert_eq!(grid_for(5, true, None, Some(3)), (2, 3));
    assert_eq!(grid_for(5, true, Some(3), Some(5)), (3, 5));
    assert_eq!(slot_origin(0, 4), (0, 0));
    assert_eq!(slot_origin(5, 4), (1, 1));
    assert_eq!(slot_origin(11, 4), (3, 2));
}

#[test]
fn sheets_are_split_in_order() {
    let plan = |n, c, r, hidden| plan_sheets(n, c, r, hidden).unwrap();
    let sheet = |columns, rows, cards| SheetPlan {
        columns,
        rows,
        cards,
    };
    assert_eq!(plan(1, None, None, true), [sheet(2, 2, 1)]);
    assert_eq!(plan(69, None, None, true), [sheet(10, 7, 69)]);
    assert_eq!(
        plan(70, None, None, true),
        [sheet(10, 7, 69), sheet(2, 2, 1)]
    );
    assert_eq!(plan(70, None, None, false), [sheet(10, 7, 70)]);
    assert_eq!(
        plan(139, None, None, true),
        [sheet(10, 7, 69), sheet(10, 7, 69), sheet(2, 2, 1)]
    );
    // Capacity 3 * 7 - 1: the last sheet keeps the columns.
    assert_eq!(
        plan(21, Some(3), None, true),
        [sheet(3, 7, 20), sheet(3, 1, 1)]
    );
    assert_eq!(
        plan(3, Some(1), Some(1), false),
        [sheet(1, 1, 1), sheet(1, 1, 1), sheet(1, 1, 1)]
    );
    // Both given: every sheet has that grid, the last may be partly empty.
    assert_eq!(
        plan(7, Some(2), Some(3), true),
        [sheet(2, 3, 5), sheet(2, 3, 2)]
    );
}

#[test]
fn bad_grids_are_errors() {
    let text = |c, r, hidden| plan_sheets(5, c, r, hidden).unwrap_err().to_string();
    assert!(text(Some(11), None, true).contains("10"));
    assert!(text(Some(0), None, true).contains("between 1 and 10"));
    assert!(text(None, Some(8), true).contains("7"));
    assert!(text(Some(1), Some(1), true).contains("hidden-card slot"));
    assert!(plan_sheets(0, None, None, true).is_err());
    assert!(plan_sheets(1, Some(1), Some(1), false).is_ok());
}

#[test]
fn one_card_makes_a_2x2_sheet_with_the_hidden_slot() {
    let dir = tempfile::tempdir().unwrap();
    let deck = build(dir.path(), 1, &DeckOptions::default());
    assert_eq!((deck.card_width, deck.card_height), CARD);
    assert!(!deck.back_is_hidden);
    assert_eq!(deck.sheets.len(), 1);
    let sheet = &deck.sheets[0];
    assert_eq!((sheet.columns, sheet.rows), (2, 2));
    assert_eq!((sheet.width, sheet.height), (6, 8));
    assert_eq!(sheet.hidden_slot, Some(3));
    let img = decode(sheet);
    assert_cell(&img, 2, CARD, 0, colour(0));
    assert_cell(&img, 2, CARD, 1, [0x80; 3]);
    assert_cell(&img, 2, CARD, 2, [0x80; 3]);
    assert_cell(&img, 2, CARD, 3, [0x80; 3]);
}

#[test]
fn every_card_lies_at_its_slot() {
    let dir = tempfile::tempdir().unwrap();
    let options = DeckOptions {
        fill: [1, 2, 3],
        ..DeckOptions::default()
    };
    let deck = build(dir.path(), 69, &options);
    assert_eq!(deck.sheets.len(), 1);
    let sheet = &deck.sheets[0];
    assert_eq!((sheet.columns, sheet.rows), (10, 7));
    assert_eq!((sheet.width, sheet.height), (30, 28));
    assert_eq!(sheet.hidden_slot, Some(69));
    assert_eq!(sheet.cards.len(), 69);
    let img = decode(sheet);
    for (k, placed) in sheet.cards.iter().enumerate() {
        let k = k as u32;
        assert_eq!((placed.index, placed.slot), (k, k));
        assert_eq!((placed.column, placed.row), (k % 10, k / 10));
        assert_cell(&img, 10, CARD, k, colour(k));
    }
    assert_cell(&img, 10, CARD, 69, [1, 2, 3]);
}

#[test]
fn the_hidden_image_fills_the_last_slot_and_gaps_use_the_fill() {
    let dir = tempfile::tempdir().unwrap();
    let hidden = dir.path().join("back.png");
    solid(&hidden, CARD, [200, 10, 10]);
    let options = DeckOptions {
        hidden: Some(hidden),
        ..DeckOptions::default()
    };
    let deck = build(dir.path(), 5, &options);
    let sheet = &deck.sheets[0];
    assert_eq!((sheet.columns, sheet.rows), (3, 2));
    let img = decode(sheet);
    assert_cell(&img, 3, CARD, 4, colour(4));
    assert_cell(&img, 3, CARD, 5, [200, 10, 10]);
    // 4 cards and the hidden slot leave a gap before the hidden slot.
    let deck = build(dir.path(), 4, &options);
    let img = decode(&deck.sheets[0]);
    assert_eq!(deck.sheets[0].hidden_slot, Some(5));
    assert_cell(&img, 3, CARD, 4, [0x80; 3]);
    assert_cell(&img, 3, CARD, 5, [200, 10, 10]);
}

#[test]
fn seventy_cards_need_two_sheets_unless_the_hidden_slot_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(dir.path(), 70);
    let deck = build_deck(&all, &DeckOptions::default()).unwrap();
    assert_eq!(deck.sheets.len(), 2);
    assert_eq!(deck.sheets[0].cards.len(), 69);
    assert_eq!(deck.sheets[1].cards.len(), 1);
    assert_eq!(
        (
            deck.sheets[1].columns,
            deck.sheets[1].rows,
            deck.sheets[1].hidden_slot
        ),
        (2, 2, Some(3))
    );
    // The index counts over the whole deck, the slot per sheet.
    let last = &deck.sheets[1].cards[0];
    assert_eq!((last.index, last.slot), (69, 0));
    assert_cell(&decode(&deck.sheets[1]), 2, CARD, 0, colour(69));

    let options = DeckOptions {
        hidden_slot: false,
        ..DeckOptions::default()
    };
    let deck = build_deck(&all, &options).unwrap();
    assert!(deck.back_is_hidden);
    assert_eq!(deck.sheets.len(), 1);
    let sheet = &deck.sheets[0];
    assert_eq!(
        (sheet.columns, sheet.rows, sheet.hidden_slot),
        (10, 7, None)
    );
    let img = decode(sheet);
    assert_cell(&img, 10, CARD, 69, colour(69));
}

#[test]
fn hidden_image_without_hidden_slot_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let hidden = dir.path().join("back.png");
    solid(&hidden, CARD, [1, 1, 1]);
    let options = DeckOptions {
        hidden: Some(hidden),
        hidden_slot: false,
        ..DeckOptions::default()
    };
    let text = error_text(build_deck(&cards(dir.path(), 2), &options));
    assert!(text.contains("--no-hidden-slot"), "{text}");
}

#[test]
fn explicit_grids() {
    let dir = tempfile::tempdir().unwrap();
    let one_column = DeckOptions {
        columns: Some(1),
        ..DeckOptions::default()
    };
    let deck = build(dir.path(), 4, &one_column);
    let sheet = &deck.sheets[0];
    assert_eq!(
        (sheet.columns, sheet.rows, sheet.hidden_slot),
        (1, 5, Some(4))
    );
    let img = decode(sheet);
    for k in 0..4 {
        assert_cell(&img, 1, CARD, k, colour(k));
    }

    let one_row = DeckOptions {
        rows: Some(1),
        hidden_slot: false,
        ..DeckOptions::default()
    };
    let deck = build(dir.path(), 4, &one_row);
    let sheet = &deck.sheets[0];
    assert_eq!((sheet.columns, sheet.rows, sheet.hidden_slot), (4, 1, None));
    assert_cell(&decode(sheet), 4, CARD, 3, colour(3));

    let both = DeckOptions {
        columns: Some(4),
        rows: Some(3),
        ..DeckOptions::default()
    };
    let deck = build(dir.path(), 3, &both);
    assert_eq!((deck.sheets[0].columns, deck.sheets[0].rows), (4, 3));
    assert_eq!(deck.sheets[0].hidden_slot, Some(11));

    let tiny = DeckOptions {
        columns: Some(1),
        rows: Some(1),
        ..DeckOptions::default()
    };
    let text = error_text(build_deck(&cards(dir.path(), 1), &tiny));
    assert!(text.contains("1x1"), "{text}");
    let wide = DeckOptions {
        columns: Some(11),
        ..DeckOptions::default()
    };
    let text = error_text(build_deck(&cards(dir.path(), 1), &wide));
    assert!(text.contains("10"), "{text}");
}

#[test]
fn another_size_is_an_error_listing_every_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut all = cards(dir.path(), 2);
    for (name, size) in [("wide.png", (6, 4)), ("tall.png", (3, 8))] {
        let path = dir.path().join(name);
        solid(&path, size, [9, 9, 9]);
        all.push(path);
    }
    let text = error_text(build_deck(&all, &DeckOptions::default()));
    assert!(text.contains("wide.png") && text.contains("6x4"), "{text}");
    assert!(text.contains("tall.png") && text.contains("3x8"), "{text}");
    assert!(text.contains("3x4"), "{text}");
    assert!(!text.contains("card1.png"), "{text}");

    // A wrong hidden image is listed too.
    let hidden = dir.path().join("back.png");
    solid(&hidden, (5, 5), [9, 9, 9]);
    let options = DeckOptions {
        hidden: Some(hidden),
        ..DeckOptions::default()
    };
    let text = error_text(build_deck(&all[..2], &options));
    assert!(text.contains("back.png") && text.contains("5x5"), "{text}");

    // --card-size without --fit does not accept cards of the first size either.
    let options = DeckOptions {
        card_size: Some((4, 4)),
        ..DeckOptions::default()
    };
    let text = error_text(build_deck(&all[..2], &options));
    assert!(
        text.contains("--card-size") && text.contains("card0.png"),
        "{text}"
    );
}

/// A card of 8x4: the left half red, the right half blue.
fn two_halves(path: &Path) {
    ImageBuffer::from_fn(8, 4, |x, _| {
        if x < 4 {
            Rgb([255u8, 0, 0])
        } else {
            Rgb([0u8, 0, 255])
        }
    })
    .save(path)
    .unwrap();
}

#[test]
fn fit_modes() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.png");
    solid(&first, (4, 4), [10, 20, 30]);
    let wide = dir.path().join("wide.png");
    two_halves(&wide);
    let all = [first, wide];
    let fill = [50, 60, 70];
    let build = |fit| {
        let options = DeckOptions {
            fit: Some(fit),
            fill,
            ..DeckOptions::default()
        };
        let deck = build_deck(&all, &options).unwrap();
        assert_eq!((deck.card_width, deck.card_height), (4, 4));
        decode(&deck.sheets[0])
    };
    let pixel = |img: &image::RgbImage, slot: u32, x: u32, y: u32| {
        let (c, r) = slot_origin(slot, 2);
        img.get_pixel(c * 4 + x, r * 4 + y).0
    };

    // The card of the cell size is copied untouched.
    let contain = build(Fit::Contain);
    assert_cell(&contain, 2, (4, 4), 0, [10, 20, 30]);
    // 8x4 scaled to 4x2, centred: one band of fill above and below.
    for x in 0..4 {
        assert_eq!(pixel(&contain, 1, x, 0), fill);
        assert_eq!(pixel(&contain, 1, x, 3), fill);
    }
    assert_eq!(pixel(&contain, 1, 0, 1), [255, 0, 0]);
    assert_eq!(pixel(&contain, 1, 3, 2), [0, 0, 255]);

    // Cover keeps the scale (the height already matches) and crops the sides:
    // two red and two blue columns remain.
    let cover = build(Fit::Cover);
    for y in 0..4 {
        assert_eq!(pixel(&cover, 1, 0, y), [255, 0, 0]);
        assert_eq!(pixel(&cover, 1, 1, y), [255, 0, 0]);
        assert_eq!(pixel(&cover, 1, 2, y), [0, 0, 255]);
        assert_eq!(pixel(&cover, 1, 3, y), [0, 0, 255]);
    }

    // Stretch fills the cell; the left and right ends keep their colour.
    let stretch = build(Fit::Stretch);
    assert_eq!(pixel(&stretch, 1, 0, 0), [255, 0, 0]);
    assert_eq!(pixel(&stretch, 1, 3, 3), [0, 0, 255]);
}

#[test]
fn a_card_size_with_fit_sets_the_cell() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(dir.path(), 2);
    let options = DeckOptions {
        card_size: Some((6, 8)),
        fit: Some(Fit::Stretch),
        ..DeckOptions::default()
    };
    let deck = build_deck(&all, &options).unwrap();
    assert_eq!((deck.card_width, deck.card_height), (6, 8));
    assert_eq!((deck.sheets[0].width, deck.sheets[0].height), (12, 16));
    let img = decode(&deck.sheets[0]);
    assert_eq!(img.get_pixel(7, 3).0, colour(1));
}

#[test]
fn max_sheet_side_scales_the_cells_down_once() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(dir.path(), 69);
    // 10x7 cells of 3x4: 30x28. A limit of 15 halves the factor: 1x2 cells.
    let options = DeckOptions {
        max_sheet_side: Some(15),
        ..DeckOptions::default()
    };
    let deck = build_deck(&all, &options).unwrap();
    assert_eq!((deck.card_width, deck.card_height), (1, 2));
    assert_eq!((deck.sheets[0].width, deck.sheets[0].height), (10, 14));
    let note = deck.notes.iter().find(|n| n.code == "scaled-down").unwrap();
    assert_eq!(note.severity, Severity::Note);
    assert!(note.message.contains("3x4") && note.message.contains("1x2"));
    assert!(deck.notes.iter().all(|n| n.code != "sheet-too-large"));
    // Solid cards stay solid.
    assert_cell(&decode(&deck.sheets[0]), 10, (1, 2), 33, colour(33));

    // Large enough: untouched, no note.
    let options = DeckOptions {
        max_sheet_side: Some(30),
        ..DeckOptions::default()
    };
    let deck = build_deck(&all, &options).unwrap();
    assert_eq!((deck.card_width, deck.card_height), CARD);
    assert!(deck.notes.is_empty());

    // Never enlarged, even by a huge limit.
    let options = DeckOptions {
        max_sheet_side: Some(100_000),
        ..DeckOptions::default()
    };
    assert_eq!(build_deck(&all, &options).unwrap().card_width, 3);

    // Not even cells of one pixel fit 10 columns into 9 pixels.
    let options = DeckOptions {
        max_sheet_side: Some(9),
        ..DeckOptions::default()
    };
    let text = error_text(build_deck(&all, &options));
    assert!(
        text.contains("--max-sheet-side") && text.contains("10x7"),
        "{text}"
    );
}

#[test]
fn max_sheet_side_applies_the_fit_against_the_final_cell() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.png");
    solid(&first, (4, 4), [10, 20, 30]);
    let wide = dir.path().join("wide.png");
    two_halves(&wide);
    let options = DeckOptions {
        fit: Some(Fit::Cover),
        max_sheet_side: Some(2),
        ..DeckOptions::default()
    };
    let deck = build_deck(&[first, wide], &options).unwrap();
    assert_eq!((deck.card_width, deck.card_height), (1, 1));
    assert_eq!((deck.sheets[0].width, deck.sheets[0].height), (2, 2));
}

#[test]
fn big_sheets_get_a_warning_unless_a_limit_is_given() {
    let dir = tempfile::tempdir().unwrap();
    let card = dir.path().join("a.png");
    solid(&card, (500, 4), [1, 2, 3]);
    let deck = build_deck(std::slice::from_ref(&card), &DeckOptions::default()).unwrap();
    // 2 columns of 500 px: fine.
    assert!(deck.notes.is_empty());
    let options = DeckOptions {
        columns: Some(10),
        ..DeckOptions::default()
    };
    let deck = build_deck(&[card], &options).unwrap();
    let warning = deck
        .notes
        .iter()
        .find(|n| n.code == "sheet-too-large")
        .unwrap();
    assert_eq!(warning.severity, Severity::Warning);
    assert!(warning.message.contains("5000x"), "{}", warning.message);
}

#[test]
fn output_is_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(dir.path(), 12);
    for format in [OutputFormat::Png, OutputFormat::Jpeg] {
        let options = DeckOptions {
            format,
            ..DeckOptions::default()
        };
        let a = build_deck(&all, &options).unwrap();
        let b = build_deck(&all, &options).unwrap();
        assert_eq!(a.sheets[0].bytes, b.sheets[0].bytes);
    }
    let png = build_deck(&all, &DeckOptions::default()).unwrap();
    assert!(png.sheets[0].bytes.starts_with(&[0x89, b'P', b'N', b'G']));
    let jpeg = build_deck(
        &all,
        &DeckOptions {
            format: OutputFormat::Jpeg,
            ..DeckOptions::default()
        },
    )
    .unwrap();
    assert!(jpeg.sheets[0].bytes.starts_with(&[0xFF, 0xD8]));
}

#[test]
fn png_is_rgb_unless_a_pixel_is_transparent() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(dir.path(), 2);
    let deck = build_deck(&all, &DeckOptions::default()).unwrap();
    let color = |deck: &Deck| {
        image::load_from_memory(&deck.sheets[0].bytes)
            .unwrap()
            .color()
    };
    assert_eq!(color(&deck), image::ColorType::Rgb8);

    let ghost = dir.path().join("ghost.png");
    ImageBuffer::from_pixel(CARD.0, CARD.1, image::Rgba([5u8, 6, 7, 0]))
        .save(&ghost)
        .unwrap();
    let all = [all[0].clone(), ghost];
    let deck = build_deck(&all, &DeckOptions::default()).unwrap();
    assert_eq!(color(&deck), image::ColorType::Rgba8);
    let img = image::load_from_memory(&deck.sheets[0].bytes)
        .unwrap()
        .to_rgba8();
    assert_eq!(img.get_pixel(3, 0).0[3], 0);
    assert_eq!(
        img.get_pixel(0, 0).0,
        [colour(0)[0], colour(0)[1], colour(0)[2], 255]
    );
    // The fill is opaque.
    assert_eq!(img.get_pixel(0, 4).0[3], 255);

    // JPEG flattens the transparency onto the fill colour, with a note.
    let options = DeckOptions {
        format: OutputFormat::Jpeg,
        quality: 100,
        fill: [255, 255, 255],
        ..DeckOptions::default()
    };
    let deck = build_deck(&all, &options).unwrap();
    assert!(deck.notes.iter().any(|n| n.code == "alpha-flattened"));
    let img = decode(&deck.sheets[0]);
    assert!(img.get_pixel(4, 1).0.iter().all(|&c| c > 250));
}

#[test]
fn rejected_and_unknown_formats_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let bmp = dir.path().join("a.bmp");
    solid(&bmp, CARD, [1, 2, 3]);
    let error = build_deck(&[bmp], &DeckOptions::default()).unwrap_err();
    assert!(
        matches!(&error, ImageError::Rejected { format, .. } if format == "bmp"),
        "{error}"
    );
    let text = dir.path().join("b.png");
    std::fs::write(&text, b"not an image").unwrap();
    assert!(matches!(
        build_deck(&[text], &DeckOptions::default()),
        Err(ImageError::UnknownFormat { .. })
    ));
    let missing = dir.path().join("missing.png");
    assert!(matches!(
        build_deck(&[missing], &DeckOptions::default()),
        Err(ImageError::Io { .. })
    ));
}

#[test]
fn a_file_may_repeat_and_gets_a_slot_each_time() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(dir.path(), 2);
    let sequence = [
        all[0].clone(),
        all[1].clone(),
        all[0].clone(),
        all[0].clone(),
    ];
    let deck = build_deck(&sequence, &DeckOptions::default()).unwrap();
    let sheet = &deck.sheets[0];
    assert_eq!(sheet.cards.len(), 4);
    let img = decode(sheet);
    let expected = [colour(0), colour(1), colour(0), colour(0)];
    for (k, rgb) in expected.into_iter().enumerate() {
        assert_cell(&img, sheet.columns, CARD, k as u32, rgb);
        assert_eq!(sheet.cards[k].source, sequence[k]);
    }
}

#[test]
fn natural_order() {
    let mut names = [
        "card10.png",
        "card2.png",
        "Card1.png",
        "card02.png",
        "b.png",
        "a.png",
    ];
    names.sort_by(|a, b| natural_cmp(a, b));
    assert_eq!(
        names,
        [
            "a.png",
            "b.png",
            "Card1.png",
            "card02.png",
            "card2.png",
            "card10.png"
        ]
    );
    // Total: equal in the natural sense, ordered by the plain string.
    assert_eq!(natural_cmp("a1", "a1"), std::cmp::Ordering::Equal);
    assert_ne!(natural_cmp("A", "a"), std::cmp::Ordering::Equal);
    assert_eq!(natural_cmp("A", "a"), natural_cmp("a", "A").reverse());
    assert_eq!(natural_cmp("1", "a"), std::cmp::Ordering::Less);
    assert_eq!(natural_cmp("a", "ab"), std::cmp::Ordering::Less);
}

#[test]
fn directories_give_their_images_in_natural_order_and_files_keep_theirs() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("cards");
    std::fs::create_dir_all(sub.join("nested")).unwrap();
    for name in [
        "card10.png",
        "card2.PNG",
        "card1.jpg",
        "notes.txt",
        "nested/x.png",
    ] {
        std::fs::write(sub.join(name), b"x").unwrap();
    }
    let single = dir.path().join("z.png");
    let expanded = expand_card_inputs(&[single.clone(), sub.clone(), single.clone()]).unwrap();
    assert_eq!(
        expanded,
        [
            single.clone(),
            sub.join("card1.jpg"),
            sub.join("card2.PNG"),
            sub.join("card10.png"),
            single,
        ]
    );
}

#[test]
fn card_lists() {
    let dir = tempfile::tempdir().unwrap();
    let list = dir.path().join("deck.txt");
    let mut text = String::from("\u{feff}# the deck\r\n\r\nfront/a.png\r\n  b.png  \r\n#c.png\n");
    let absolute = dir.path().join("abs.png");
    text.push_str(&format!("{}\n", absolute.display()));
    text.push_str("front\n");
    std::fs::write(&list, text).unwrap();
    let entries = read_card_list(&list).unwrap();
    assert_eq!(
        entries,
        [
            dir.path().join("front/a.png"),
            dir.path().join("b.png"),
            absolute,
            dir.path().join("front"),
        ]
    );
    std::fs::write(&list, [0xFF, 0xFE, 0x00]).unwrap();
    assert!(read_card_list(&list).is_err());
    assert!(read_card_list(&dir.path().join("none.txt")).is_err());
}

#[test]
fn description_lists_every_card() {
    let dir = tempfile::tempdir().unwrap();
    let all = cards(dir.path(), 70);
    let deck = build_deck(&all, &DeckOptions::default()).unwrap();
    let files = vec!["deck_1.png".to_owned(), "deck_2.png".to_owned()];
    let d = deck.description("deck", &files);
    assert_eq!(d.format_version, 1);
    assert_eq!(
        (d.card_width, d.card_height, d.back_is_hidden),
        (3, 4, false)
    );
    assert_eq!(d.sheets.len(), 2);
    assert_eq!(d.sheets[0].file, "deck_1.png");
    assert_eq!(d.sheets[0].card_count, 69);
    assert_eq!(d.sheets[0].hidden_slot, Some(69));
    assert_eq!((d.sheets[1].columns, d.sheets[1].rows), (2, 2));
    let card = &d.sheets[0].cards[12];
    assert_eq!(
        (card.index, card.slot, card.column, card.row),
        (12, 12, 2, 1)
    );
    assert!(!card.source.contains('\\'));
    assert_eq!(d.sheets[1].cards[0].index, 69);
}
