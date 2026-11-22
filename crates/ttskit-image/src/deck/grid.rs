//! Sheet layout: how many columns and rows a sheet gets, and where a slot lies.
//! The limits are the game's (`docs/deck-sheets.md`).

use crate::ImageError;

/// Most columns a sheet of a custom deck has.
pub const MAX_COLUMNS: u32 = 10;
/// Most rows a sheet of a custom deck has.
pub const MAX_ROWS: u32 = 7;

/// The `(column, row)` of `slot` on a sheet with `columns` columns. Slots are
/// numbered from 0, left to right and then top to bottom.
pub fn slot_origin(slot: u32, columns: u32) -> (u32, u32) {
    (slot % columns, slot / columns)
}

fn ceil_sqrt(n: u32) -> u32 {
    let mut root = 0;
    while root * root < n {
        root += 1;
    }
    root
}

/// The grid `(columns, rows)` of a sheet that holds `cards` cards (plus the
/// hidden-card slot if `hidden_slot`). A given dimension is kept and the other
/// one is the smallest that fits (at least 1). With neither given the grid is
/// the smallest roughly square one, but at least 2x2 (the in-game dialog does
/// not take less) and at most 10x7. The caller makes sure the cards fit: for a
/// given dimension the result is not limited to the game's maximum.
pub fn grid_for(
    cards: u32,
    hidden_slot: bool,
    columns: Option<u32>,
    rows: Option<u32>,
) -> (u32, u32) {
    let slots = cards + u32::from(hidden_slot);
    match (columns, rows) {
        (Some(c), Some(r)) => (c, r),
        (Some(c), None) => (c, slots.div_ceil(c).max(1)),
        (None, Some(r)) => (slots.div_ceil(r).max(1), r),
        (None, None) => {
            let c = ceil_sqrt(slots).clamp(2, MAX_COLUMNS);
            let r = slots.div_ceil(c).max(2);
            if r > MAX_ROWS {
                (MAX_COLUMNS, MAX_ROWS)
            } else {
                (c, r)
            }
        }
    }
}

/// One sheet of a deck: its grid and how many cards it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SheetPlan {
    pub columns: u32,
    pub rows: u32,
    pub cards: u32,
}

/// Splits `cards` cards over sheets. Every sheet but the last is full (a
/// missing dimension counts as the game's maximum); the last keeps a given
/// dimension and takes the smallest value of the other that fits.
pub fn plan_sheets(
    cards: usize,
    columns: Option<u32>,
    rows: Option<u32>,
    hidden_slot: bool,
) -> Result<Vec<SheetPlan>, ImageError> {
    let bad = |m: String| ImageError::Options(m);
    if let Some(c) = columns
        && !(1..=MAX_COLUMNS).contains(&c)
    {
        return Err(bad(format!(
            "columns must be between 1 and {MAX_COLUMNS} (the limit of the game), not {c}"
        )));
    }
    if let Some(r) = rows
        && !(1..=MAX_ROWS).contains(&r)
    {
        return Err(bad(format!(
            "rows must be between 1 and {MAX_ROWS} (the limit of the game), not {r}"
        )));
    }
    if cards == 0 {
        return Err(bad("there are no cards to put on a sheet".to_owned()));
    }
    let full_columns = columns.unwrap_or(MAX_COLUMNS);
    let full_rows = rows.unwrap_or(MAX_ROWS);
    let capacity = full_columns * full_rows - u32::from(hidden_slot);
    if capacity == 0 {
        return Err(bad(format!(
            "a {full_columns}x{full_rows} grid has no room for a card next to the hidden-card slot; use a larger grid or --no-hidden-slot"
        )));
    }
    let mut plans = Vec::new();
    let mut left = cards;
    while left > 0 {
        if left > capacity as usize {
            plans.push(SheetPlan {
                columns: full_columns,
                rows: full_rows,
                cards: capacity,
            });
            left -= capacity as usize;
        } else {
            let n = left as u32;
            let (c, r) = grid_for(n, hidden_slot, columns, rows);
            plans.push(SheetPlan {
                columns: c,
                rows: r,
                cards: n,
            });
            left = 0;
        }
    }
    Ok(plans)
}
