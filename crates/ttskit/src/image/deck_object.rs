//! `image deck object`: the saved-object JSON of a custom deck.
//!
//! Pure functions (no files, no clock, no randomness), so that the output can
//! be tested exactly. The field set and defaults were taken from a 60 card
//! deck made in the game (see docs/deck-sheets.md).

#![allow(dead_code)] // wired into the command by the next commit

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use ttskit_image::Description;

/// Name, description and GM notes of one object. `None` keeps the default
/// (empty text).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    pub nickname: Option<String>,
    pub description: Option<String>,
    pub gm_notes: Option<String>,
}

/// The text of the deck object and of its cards, in deck order. A missing
/// entry (a shorter list) means no text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub deck: Text,
    pub cards: Vec<Text>,
}

pub struct ObjectInput<'a> {
    pub description: &'a Description,
    /// One URL per sheet, in sheet order.
    pub face_urls: &'a [String],
    /// One URL per sheet, or a single one shared by all sheets.
    pub back_urls: &'a [String],
    pub unique_backs: bool,
    pub meta: &'a Meta,
    /// Cards without a nickname in `meta` get the file stem of their source.
    pub nicknames_from_files: bool,
}

/// The file stem of a card source such as `cards/ace of spades.png`.
pub fn source_stem(source: &str) -> &str {
    let name = source.rsplit('/').next().unwrap_or(source);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// Builds `{"ObjectStates": [object]}`: a `DeckCustom` for two or more cards,
/// a `CardCustom` for one.
pub fn build_object(input: &ObjectInput) -> Result<Value, String> {
    let d = input.description;
    let sheets = d.sheets.len();
    if sheets == 0 {
        return Err("the description has no sheets".to_owned());
    }
    if input.face_urls.len() != sheets {
        return Err(format!(
            "{} face URLs for {sheets} sheets",
            input.face_urls.len()
        ));
    }
    let backs_ok = if input.unique_backs {
        input.back_urls.len() == sheets
    } else {
        input.back_urls.len() == 1
    };
    if !backs_ok {
        return Err(format!(
            "{} back URLs given; {}",
            input.back_urls.len(),
            if input.unique_backs {
                format!("--unique-backs needs one per sheet ({sheets})")
            } else {
                "all sheets share one back (use --unique-backs for one per sheet)".to_owned()
            }
        ));
    }

    let mut custom_deck = Map::new();
    for (n, sheet) in d.sheets.iter().enumerate() {
        let back = &input.back_urls[if input.unique_backs { n } else { 0 }];
        custom_deck.insert(
            (n + 1).to_string(),
            json!({
                "FaceURL": input.face_urls[n],
                "BackURL": back,
                "NumWidth": sheet.columns,
                "NumHeight": sheet.rows,
                "BackIsHidden": d.back_is_hidden,
                "UniqueBack": input.unique_backs,
                "Type": 0,
            }),
        );
    }

    // Cards in deck order, with the id the game gives them: the key of the
    // sheet times 100, plus the slot.
    let mut cards: Vec<(u32, &str)> = Vec::new();
    for (n, sheet) in d.sheets.iter().enumerate() {
        for card in &sheet.cards {
            if card.slot >= sheet.columns * sheet.rows {
                return Err(format!(
                    "card {} is in slot {}, outside the {}x{} grid of {}",
                    card.index, card.slot, sheet.columns, sheet.rows, sheet.file
                ));
            }
            cards.push((((n + 1) as u32) * 100 + card.slot, card.source.as_str()));
        }
    }
    if cards.is_empty() {
        return Err("the description has no cards".to_owned());
    }
    if input.meta.cards.len() > cards.len() {
        return Err(format!(
            "the metadata has text for {} cards, but the deck has {}",
            input.meta.cards.len(),
            cards.len()
        ));
    }

    let mut guids = Guids::default();
    let empty = Text::default();
    let mut objects = Vec::new();
    for (i, (id, source)) in cards.iter().enumerate() {
        let mut text = input.meta.cards.get(i).unwrap_or(&empty).clone();
        if text.nickname.is_none() && input.nicknames_from_files {
            text.nickname = Some(source_stem(source).to_owned());
        }
        let key = format!(
            "card|{i}|{id}|{source}|{}",
            text.nickname.as_deref().unwrap_or("")
        );
        let name = if cards.len() == 1 {
            "CardCustom"
        } else {
            "Card"
        };
        let mut card = object(name, &guids.next(&key), &text);
        card.insert("Hands".to_owned(), json!(true));
        card.insert("CardID".to_owned(), json!(id));
        card.insert("SidewaysCard".to_owned(), json!(false));
        if cards.len() == 1 {
            card.insert("CustomDeck".to_owned(), Value::Object(custom_deck.clone()));
        }
        card.insert("XmlUI".to_owned(), json!(""));
        card.insert("ContainedObjects".to_owned(), json!([]));
        objects.push(Value::Object(card));
    }

    let root = if cards.len() == 1 {
        objects.remove(0)
    } else {
        let all: String = cards.iter().map(|(id, s)| format!("{id}={s};")).collect();
        let key = format!("deck|{}|{all}", d.name);
        let mut deck = object("DeckCustom", &guids.next(&key), &input.meta.deck);
        deck.insert("HideWhenFaceDown".to_owned(), json!(true));
        deck.insert("Hands".to_owned(), json!(false));
        deck.insert("SidewaysCard".to_owned(), json!(false));
        deck.insert(
            "DeckIDs".to_owned(),
            Value::Array(cards.iter().map(|(id, _)| json!(id)).collect()),
        );
        deck.insert("CustomDeck".to_owned(), Value::Object(custom_deck));
        deck.insert("LuaScript".to_owned(), json!(""));
        deck.insert("LuaScriptState".to_owned(), json!(""));
        deck.insert("XmlUI".to_owned(), json!(""));
        deck.insert("ContainedObjects".to_owned(), Value::Array(objects));
        Value::Object(deck)
    };
    Ok(json!({ "ObjectStates": [root] }))
}

/// The fields every object has, in the order the game writes them, up to and
/// including `Tooltip` and `GridProjection`; the caller adds the rest.
fn object(name: &str, guid: &str, text: &Text) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "GUID": guid,
        "Name": name,
        "Transform": {
            "posX": 0.0, "posY": 1.0, "posZ": 0.0,
            "rotX": 0.0, "rotY": 180.0, "rotZ": 180.0,
            "scaleX": 1.0, "scaleY": 1.0, "scaleZ": 1.0,
        },
        "Nickname": text.nickname.as_deref().unwrap_or(""),
        "Description": text.description.as_deref().unwrap_or(""),
        "GMNotes": text.gm_notes.as_deref().unwrap_or(""),
        "AltLookAngle": { "x": 0.0, "y": 0.0, "z": 0.0 },
        "ColorDiffuse": { "r": 0.713235259, "g": 0.713235259, "b": 0.713235259 },
        "LayoutGroupSortIndex": 0,
        "Value": 0,
        "Locked": false,
        "Grid": true,
        "Snap": true,
        "IgnoreFoW": false,
        "MeasureMovement": false,
        "DragSelectable": true,
        "Autoraise": true,
        "Sticky": true,
        "Tooltip": true,
        "GridProjection": false,
    }) else {
        unreachable!()
    };
    map
}

/// Deterministic GUIDs: six lowercase hex digits from a hash of the content,
/// made longer-probed until they are unique within the object.
#[derive(Default)]
struct Guids {
    used: BTreeSet<String>,
}

impl Guids {
    fn next(&mut self, content: &str) -> String {
        let mut attempt = 0u32;
        loop {
            let digest = Sha256::digest(format!("{attempt}|{content}"));
            let guid: String = digest[..3].iter().map(|b| format!("{b:02x}")).collect();
            if self.used.insert(guid.clone()) {
                return guid;
            }
            attempt += 1;
        }
    }
}

#[cfg(test)]
mod tests;
