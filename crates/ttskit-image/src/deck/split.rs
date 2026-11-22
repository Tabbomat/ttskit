//! Cutting a deck sheet into single card images: the reverse of `build_deck`.
//! The slot rules are those of `docs/deck-sheets.md`. The output is
//! deterministic: the same sheet and options give the same bytes.

use std::path::{Path, PathBuf};

use image::DynamicImage;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;

use super::cell::{decode_keeping_depth, is_deep};
use super::grid::{MAX_COLUMNS, MAX_ROWS, slot_origin};
use super::{bad, flatten};
use crate::{ImageError, OutputFormat, Problem, Severity};

#[derive(Debug, Clone)]
pub struct SplitOptions {
    /// Columns of the sheet, 1 to 10.
    pub columns: u32,
    /// Rows of the sheet, 1 to 7.
    pub rows: u32,
    /// Write only the first N card slots; without it all of them.
    pub count: Option<u32>,
    /// The last slot is the hidden-card slot (not a card).
    pub hidden_slot: bool,
    /// Do not write the hidden-card slot.
    pub skip_hidden: bool,
    pub format: OutputFormat,
    /// JPEG quality, 1 to 100. Ignored for PNG.
    pub quality: u8,
    /// Colour that transparency is flattened onto in JPEG output.
    pub fill: [u8; 3],
}

/// One slot to be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotPlan {
    pub slot: u32,
    pub column: u32,
    pub row: u32,
    /// The hidden-card slot rather than a card.
    pub is_hidden: bool,
}

/// The slots a split writes, in the order they are written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPlan {
    pub slots: Vec<SlotPlan>,
    /// Digits of a slot number in a file name.
    pad: usize,
}

impl SplitPlan {
    /// The file name of `slot`: `NAME_NN.EXT`, or `NAME_hidden.EXT` for the
    /// hidden-card slot. The padding depends on the grid only, not on `count`.
    pub fn file_name(&self, name: &str, slot: &SlotPlan, extension: &str) -> String {
        if slot.is_hidden {
            format!("{name}_hidden.{extension}")
        } else {
            format!("{name}_{:0pad$}.{extension}", slot.slot, pad = self.pad)
        }
    }
}

/// Decides which slots of a `columns` x `rows` sheet are written. Card slots
/// are numbered from 0; with `hidden_slot` the last slot is not a card but is
/// written as the hidden-card image unless `skip_hidden`.
pub fn plan_split(
    columns: u32,
    rows: u32,
    count: Option<u32>,
    hidden_slot: bool,
    skip_hidden: bool,
) -> Result<SplitPlan, ImageError> {
    if !(1..=MAX_COLUMNS).contains(&columns) {
        return Err(bad(format!(
            "columns must be between 1 and {MAX_COLUMNS} (the limit of the game), not {columns}"
        )));
    }
    if !(1..=MAX_ROWS).contains(&rows) {
        return Err(bad(format!(
            "rows must be between 1 and {MAX_ROWS} (the limit of the game), not {rows}"
        )));
    }
    let slots = columns * rows;
    let card_slots = slots - u32::from(hidden_slot);
    if card_slots == 0 {
        return Err(bad(format!(
            "a {columns}x{rows} grid has no card slot next to the hidden-card slot; use --no-hidden-slot if its only slot is a card"
        )));
    }
    let wanted = match count {
        Some(0) => return Err(bad("--count must be at least 1")),
        Some(n) if n > card_slots => {
            return Err(bad(format!(
                "--count {n} is more than the {card_slots} card slots of a {columns}x{rows} grid{}",
                if hidden_slot {
                    " (the last slot is the hidden-card slot)"
                } else {
                    ""
                }
            )));
        }
        Some(n) => n,
        None => card_slots,
    };
    let pad = (card_slots - 1).to_string().len().max(2);
    let place = |slot, is_hidden| {
        let (column, row) = slot_origin(slot, columns);
        SlotPlan {
            slot,
            column,
            row,
            is_hidden,
        }
    };
    let mut plan: Vec<SlotPlan> = (0..wanted).map(|slot| place(slot, false)).collect();
    if hidden_slot && !skip_hidden {
        plan.push(place(slots - 1, true));
    }
    Ok(SplitPlan { slots: plan, pad })
}

/// One image cut from the sheet.
#[derive(Debug, Clone)]
pub struct SplitSlot {
    pub slot: u32,
    pub column: u32,
    pub row: u32,
    pub is_hidden: bool,
    /// The encoded PNG or JPEG file.
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Split {
    pub sheet_width: u32,
    pub sheet_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub slots: Vec<SplitSlot>,
    /// What was changed or lost on the way. Codes are stable.
    pub notes: Vec<Problem>,
}

/// Cuts the sheet `path` into its slots (see [`plan_split`]). Nothing is
/// written. A sheet that does not divide evenly into the grid is an error.
pub fn split_sheet(path: &Path, options: &SplitOptions) -> Result<Split, ImageError> {
    if !(1..=100).contains(&options.quality) {
        return Err(bad(format!(
            "quality must be between 1 and 100, not {}",
            options.quality
        )));
    }
    let plan = plan_split(
        options.columns,
        options.rows,
        options.count,
        options.hidden_slot,
        options.skip_hidden,
    )?;
    let mut notes = Vec::new();
    let sheet = decode_keeping_depth(path, &mut notes)?;
    let (width, height) = (sheet.width(), sheet.height());
    let (columns, rows) = (options.columns, options.rows);
    if width % columns != 0 || height % rows != 0 {
        return Err(bad(format!(
            "{}: the sheet is {width}x{height} px, which does not divide into {columns} columns x {rows} rows (width leaves {} px over, height {} px)",
            path.display(),
            width % columns,
            height % rows
        )));
    }
    let (cell_width, cell_height) = (width / columns, height / rows);
    if cell_width == 0 || cell_height == 0 {
        return Err(bad(format!(
            "{}: the sheet is {width}x{height} px, too small for {columns} columns x {rows} rows (a cell would be empty)",
            path.display()
        )));
    }

    let deep = is_deep(&sheet);
    if deep && options.format == OutputFormat::Jpeg {
        notes.push(Problem::new(
            Severity::Note,
            "reduced-to-8-bit",
            "channels reduced to 8 bits (JPEG has 8-bit channels)".to_owned(),
        ));
    }
    let mut flattened = 0;
    let mut slots = Vec::with_capacity(plan.slots.len());
    for planned in &plan.slots {
        let cell = sheet.crop_imm(
            planned.column * cell_width,
            planned.row * cell_height,
            cell_width,
            cell_height,
        );
        let (bytes, count) = encode_cell(&cell, options, planned.slot)?;
        flattened += count;
        slots.push(SplitSlot {
            slot: planned.slot,
            column: planned.column,
            row: planned.row,
            is_hidden: planned.is_hidden,
            bytes,
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
    Ok(Split {
        sheet_width: width,
        sheet_height: height,
        cell_width,
        cell_height,
        slots,
        notes,
    })
}

/// Encodes one cell; the second value is the number of pixels flattened
/// onto the fill colour (JPEG only). PNG keeps the colour type and bit depth.
fn encode_cell(
    cell: &DynamicImage,
    options: &SplitOptions,
    slot: u32,
) -> Result<(Vec<u8>, u64), ImageError> {
    let fail = |source| ImageError::Encode {
        path: PathBuf::from(format!("slot {slot}")),
        source,
    };
    let mut bytes = Vec::new();
    let mut flattened = 0;
    match options.format {
        OutputFormat::Png => cell
            .write_with_encoder(PngEncoder::new(&mut bytes))
            .map_err(fail)?,
        OutputFormat::Jpeg => {
            let (rgb, count) = flatten(&cell.to_rgba8(), options.fill);
            flattened = count;
            DynamicImage::ImageRgb8(rgb)
                .write_with_encoder(JpegEncoder::new_with_quality(&mut bytes, options.quality))
                .map_err(fail)?;
        }
    }
    Ok((bytes, flattened))
}
