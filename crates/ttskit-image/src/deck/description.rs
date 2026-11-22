//! The description file written next to the sheets (`NAME.json`).

use serde::{Deserialize, Serialize};

use super::Deck;

/// Version of the description format.
pub const DESCRIPTION_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Description {
    pub format_version: u32,
    pub name: String,
    pub card_width: u32,
    pub card_height: u32,
    /// True when the last slot of a sheet holds a card (`--no-hidden-slot`).
    pub back_is_hidden: bool,
    pub sheets: Vec<SheetDescription>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetDescription {
    /// File name, relative to the directory of the description.
    pub file: String,
    pub columns: u32,
    pub rows: u32,
    pub width: u32,
    pub height: u32,
    pub card_count: u32,
    /// The slot of the hidden-card image; none with `--no-hidden-slot`.
    pub hidden_slot: Option<u32>,
    pub cards: Vec<CardDescription>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardDescription {
    /// Position in the whole deck, from 0.
    pub index: u32,
    pub slot: u32,
    pub column: u32,
    pub row: u32,
    /// The card file as given, with `/` separators.
    pub source: String,
}

impl Deck {
    /// The description of this deck. `files` holds the file name of each
    /// sheet, in order.
    pub fn description(&self, name: &str, files: &[String]) -> Description {
        Description {
            format_version: DESCRIPTION_VERSION,
            name: name.to_owned(),
            card_width: self.card_width,
            card_height: self.card_height,
            back_is_hidden: self.back_is_hidden,
            sheets: self
                .sheets
                .iter()
                .zip(files)
                .map(|(sheet, file)| SheetDescription {
                    file: file.clone(),
                    columns: sheet.columns,
                    rows: sheet.rows,
                    width: sheet.width,
                    height: sheet.height,
                    card_count: sheet.cards.len() as u32,
                    hidden_slot: sheet.hidden_slot,
                    cards: sheet
                        .cards
                        .iter()
                        .map(|c| CardDescription {
                            index: c.index,
                            slot: c.slot,
                            column: c.column,
                            row: c.row,
                            source: c.source.to_string_lossy().replace('\\', "/"),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}
