//! Deck sheets: single card images assembled into the grid sheets of a custom
//! deck. The layout rules and what they rest on are in `docs/deck-sheets.md`.
//! The output is deterministic: the same cards and options give the same bytes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{DynamicImage, Rgba, RgbaImage};

use crate::{DEFAULT_JPEG_QUALITY, DEFAULT_MAX_SIDE, ImageError, OutputFormat, Problem, Severity};

mod cell;
mod description;
mod grid;
mod input;
mod split;

pub use cell::Fit;
pub use description::{CardDescription, DESCRIPTION_VERSION, Description, SheetDescription};
pub use grid::{MAX_COLUMNS, MAX_ROWS, SheetPlan, grid_for, plan_sheets, slot_origin};
pub use input::{expand_card_inputs, natural_cmp, read_card_list};
pub use split::{SlotPlan, Split, SplitOptions, SplitPlan, SplitSlot, plan_split, split_sheet};

/// Largest side of a sheet this crate writes (the JPEG limit).
pub const MAX_SHEET_SIDE: u32 = 65_535;

#[derive(Debug, Clone)]
pub struct DeckOptions {
    /// Fixed number of columns, 1 to 10. Without it the grid is chosen.
    pub columns: Option<u32>,
    /// Fixed number of rows, 1 to 7. Without it the grid is chosen.
    pub rows: Option<u32>,
    /// Size of a card; by default the size of the first card.
    pub card_size: Option<(u32, u32)>,
    /// How cards of another size are fitted; without it they are an error.
    pub fit: Option<Fit>,
    /// Image for the hidden-card slot; without it the slot has the fill colour.
    pub hidden: Option<PathBuf>,
    /// Keep the last slot of every sheet free for the hidden-card image.
    pub hidden_slot: bool,
    /// Colour of unused slots, of the bands of `Fit::Contain` and, for JPEG
    /// output, the colour that transparency is flattened onto.
    pub fill: [u8; 3],
    /// Scale the cards down so that no sheet side exceeds this many pixels.
    pub max_sheet_side: Option<u32>,
    pub format: OutputFormat,
    /// JPEG quality, 1 to 100. Ignored for PNG.
    pub quality: u8,
}

impl Default for DeckOptions {
    fn default() -> Self {
        Self {
            columns: None,
            rows: None,
            card_size: None,
            fit: None,
            hidden: None,
            hidden_slot: true,
            fill: [0x80, 0x80, 0x80],
            max_sheet_side: None,
            format: OutputFormat::Png,
            quality: DEFAULT_JPEG_QUALITY,
        }
    }
}

/// Where a card lies on its sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedCard {
    /// Position in the whole deck, from 0.
    pub index: u32,
    pub source: PathBuf,
    pub slot: u32,
    pub column: u32,
    pub row: u32,
}

#[derive(Debug, Clone)]
pub struct Sheet {
    pub columns: u32,
    pub rows: u32,
    pub width: u32,
    pub height: u32,
    /// The encoded PNG or JPEG file.
    pub bytes: Vec<u8>,
    pub cards: Vec<PlacedCard>,
    /// The slot reserved for the hidden-card image, the last one.
    pub hidden_slot: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct Deck {
    pub card_width: u32,
    pub card_height: u32,
    /// True when the last slot holds a card (no hidden-card slot).
    pub back_is_hidden: bool,
    pub sheets: Vec<Sheet>,
    /// What was changed or lost on the way. Codes are stable.
    pub notes: Vec<Problem>,
}

fn bad(message: impl Into<String>) -> ImageError {
    ImageError::Options(message.into())
}

/// The cell size after `--max-sheet-side`: the card size, scaled down evenly
/// (in integers, never enlarged) until the largest sheet fits.
fn final_cell(
    base: (u32, u32),
    max_columns: u32,
    max_rows: u32,
    limit: Option<u32>,
    notes: &mut Vec<Problem>,
) -> Result<(u32, u32), ImageError> {
    let (cw, ch) = base;
    let (width, height) = (
        u64::from(max_columns) * u64::from(cw),
        u64::from(max_rows) * u64::from(ch),
    );
    let longest = width.max(height);
    let Some(limit) = limit.filter(|&n| longest > u64::from(n)) else {
        return Ok(base);
    };
    if max_columns > limit || max_rows > limit {
        return Err(bad(format!(
            "a sheet of {max_columns}x{max_rows} cards is wider than {limit} px even with cells of 1 pixel; raise --max-sheet-side or use fewer columns and rows"
        )));
    }
    // min(limit / width, limit / height) is limit / max(width, height).
    let scale = |side: u32| {
        u32::try_from((u64::from(side) * u64::from(limit) / longest).max(1)).unwrap_or(u32::MAX)
    };
    let cell = (scale(cw), scale(ch));
    notes.push(Problem::new(
        Severity::Note,
        "scaled-down",
        format!(
            "cards scaled down from {cw}x{ch} to {}x{} px so that no sheet side exceeds {limit} px",
            cell.0, cell.1
        ),
    ));
    Ok(cell)
}

/// Flattens `canvas` onto the `fill` colour; the second value is the number of
/// pixels that were not opaque.
fn flatten(canvas: &RgbaImage, fill: [u8; 3]) -> (image::RgbImage, u64) {
    let fill = fill.map(u32::from);
    let mut flattened = 0;
    let rgb = image::RgbImage::from_fn(canvas.width(), canvas.height(), |x, y| {
        let Rgba([r, g, b, a]) = *canvas.get_pixel(x, y);
        if a == 255 {
            return image::Rgb([r, g, b]);
        }
        flattened += 1;
        let a = u32::from(a);
        let mix = |c: u8, bg: u32| ((u32::from(c) * a + bg * (255 - a) + 127) / 255) as u8;
        image::Rgb([mix(r, fill[0]), mix(g, fill[1]), mix(b, fill[2])])
    });
    (rgb, flattened)
}

/// Encodes a sheet; the second value is the number of pixels flattened
/// onto the fill colour (JPEG only).
fn encode_sheet(
    canvas: RgbaImage,
    options: &DeckOptions,
    number: usize,
) -> Result<(Vec<u8>, u64), ImageError> {
    let fail = |source| ImageError::Encode {
        path: PathBuf::from(format!("sheet {number}")),
        source,
    };
    let mut bytes = Vec::new();
    let mut flattened = 0;
    match options.format {
        OutputFormat::Png => {
            let opaque = canvas.pixels().all(|p| p.0[3] == 255);
            let img = DynamicImage::ImageRgba8(canvas);
            let img = if opaque {
                DynamicImage::ImageRgb8(img.into_rgb8())
            } else {
                img
            };
            img.write_with_encoder(PngEncoder::new(&mut bytes))
                .map_err(fail)?;
        }
        OutputFormat::Jpeg => {
            let (rgb, count) = flatten(&canvas, options.fill);
            flattened = count;
            DynamicImage::ImageRgb8(rgb)
                .write_with_encoder(JpegEncoder::new_with_quality(&mut bytes, options.quality))
                .map_err(fail)?;
        }
    }
    Ok((bytes, flattened))
}

/// Builds the sheets of a deck from `cards` (files, in deck order; a file may
/// repeat). Nothing is written. Cards of another size are an error unless
/// `options.fit` says how to fit them; the error lists all of them.
pub fn build_deck(cards: &[PathBuf], options: &DeckOptions) -> Result<Deck, ImageError> {
    if !(1..=100).contains(&options.quality) {
        return Err(bad(format!(
            "quality must be between 1 and 100, not {}",
            options.quality
        )));
    }
    if options.card_size.is_some_and(|(w, h)| w == 0 || h == 0) {
        return Err(bad("the card size must be at least 1x1 pixels"));
    }
    if options.max_sheet_side == Some(0) {
        return Err(bad("the maximum sheet side must be at least 1 pixel"));
    }
    if options.hidden.is_some() && !options.hidden_slot {
        return Err(bad(
            "a hidden-card image needs the hidden-card slot; do not combine it with --no-hidden-slot",
        ));
    }
    let plans = plan_sheets(
        cards.len(),
        options.columns,
        options.rows,
        options.hidden_slot,
    )?;
    let max_columns = plans.iter().map(|p| p.columns).max().unwrap_or(1);
    let max_rows = plans.iter().map(|p| p.rows).max().unwrap_or(1);

    let mut notes = Vec::new();
    let (base, base_origin) = match options.card_size {
        Some(size) => (size, "given by --card-size"),
        None => {
            let first = cell::decode(&cards[0], &mut Vec::new())?;
            (first.dimensions(), "the size of the first card")
        }
    };
    let cell_size = final_cell(
        base,
        max_columns,
        max_rows,
        options.max_sheet_side,
        &mut notes,
    )?;
    let (widest, tallest) = (
        u64::from(max_columns) * u64::from(cell_size.0),
        u64::from(max_rows) * u64::from(cell_size.1),
    );
    if widest.max(tallest) > u64::from(MAX_SHEET_SIDE) {
        return Err(bad(format!(
            "a sheet would be {widest}x{tallest} px; the longest side may be at most {MAX_SHEET_SIDE} px (see --max-sheet-side)"
        )));
    }

    // Every distinct file is decoded and fitted once, straight to its final size.
    let mut cells: Vec<RgbaImage> = Vec::new();
    let mut cell_of: HashMap<&Path, usize> = HashMap::new();
    let mut errors: Vec<ImageError> = Vec::new();
    let mut mismatches: Vec<String> = Vec::new();
    let hidden = options.hidden.as_deref();
    let labelled = cards
        .iter()
        .map(|c| (c.as_path(), false))
        .chain(hidden.map(|h| (h, true)));
    for (path, is_hidden) in labelled {
        if cell_of.contains_key(path) {
            continue;
        }
        let img = match cell::decode(path, &mut notes) {
            Ok(img) => img,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        if options.fit.is_none() && img.dimensions() != base {
            let what = if is_hidden {
                " (hidden-card image)"
            } else {
                ""
            };
            mismatches.push(format!(
                "  {}{what}: {}x{}",
                path.display(),
                img.width(),
                img.height()
            ));
            continue;
        }
        cell_of.insert(path, cells.len());
        cells.push(cell::fit_cell(img, cell_size, options.fit, options.fill));
    }
    if !mismatches.is_empty() {
        errors.push(bad(format!(
            "{} image(s) are not {}x{} px, the card size ({base_origin}); resize them or pass --fit contain, cover or stretch:\n{}",
            mismatches.len(),
            base.0,
            base.1,
            mismatches.join("\n")
        )));
    }
    if errors.len() == 1 {
        return Err(errors.remove(0));
    }
    if !errors.is_empty() {
        let lines: Vec<String> = errors.iter().map(ToString::to_string).collect();
        return Err(bad(lines.join("\n")));
    }

    let fill = Rgba([options.fill[0], options.fill[1], options.fill[2], 255]);
    let mut sheets = Vec::new();
    let mut flattened = 0;
    let mut next = 0;
    for (number, plan) in plans.iter().enumerate() {
        let mut canvas =
            RgbaImage::from_pixel(plan.columns * cell_size.0, plan.rows * cell_size.1, fill);
        let place = |canvas: &mut RgbaImage, cell: &RgbaImage, slot: u32| {
            let (column, row) = slot_origin(slot, plan.columns);
            image::imageops::replace(
                canvas,
                cell,
                i64::from(column * cell_size.0),
                i64::from(row * cell_size.1),
            );
            (column, row)
        };
        let mut placed = Vec::new();
        for slot in 0..plan.cards {
            let source = &cards[next];
            let (column, row) = place(&mut canvas, &cells[cell_of[source.as_path()]], slot);
            placed.push(PlacedCard {
                index: next as u32,
                source: source.clone(),
                slot,
                column,
                row,
            });
            next += 1;
        }
        let hidden_slot = options.hidden_slot.then(|| plan.columns * plan.rows - 1);
        if let (Some(slot), Some(path)) = (hidden_slot, hidden) {
            place(&mut canvas, &cells[cell_of[path]], slot);
        }
        let (width, height) = canvas.dimensions();
        let (bytes, count) = encode_sheet(canvas, options, number + 1)?;
        flattened += count;
        sheets.push(Sheet {
            columns: plan.columns,
            rows: plan.rows,
            width,
            height,
            bytes,
            cards: placed,
            hidden_slot,
        });
    }
    if flattened > 0 {
        notes.push(Problem::new(
            Severity::Note,
            "alpha-flattened",
            format!(
                "{flattened} transparent or translucent pixels flattened onto #{:02x}{:02x}{:02x} (JPEG has no alpha channel)",
                options.fill[0], options.fill[1], options.fill[2]
            ),
        ));
    }
    if options.max_sheet_side.is_none() && widest.max(tallest) > u64::from(DEFAULT_MAX_SIDE) {
        notes.push(Problem::new(
            Severity::Warning,
            "sheet-too-large",
            format!(
                "the largest sheet is {widest}x{tallest} px; {DEFAULT_MAX_SIDE} px is the largest side seen to load fine in the game (see --max-sheet-side)"
            ),
        ));
    }
    Ok(Deck {
        card_width: cell_size.0,
        card_height: cell_size.1,
        back_is_hidden: !options.hidden_slot,
        sheets,
        notes,
    })
}

#[cfg(test)]
mod split_tests;
#[cfg(test)]
mod tests;
