//! Deck rules: `deck-size` and `deck-id-out-of-range`.
//!
//! A card id is `sheet * 100 + index`: `sheet` is the key into the object's
//! `CustomDeck` map and `index` counts cells of that sheet's grid. This is
//! checked for `DeckIDs`; a card's own `CardID` is only checked to be a
//! non-negative integer.

use serde_json::{Map, Value};

use super::RuleInfo;
use crate::config::Severity;
use crate::ctx::{Ctx, ObjectMap};

pub(super) const SIZE: RuleInfo = RuleInfo {
    id: "deck-size",
    default: Severity::Warning,
    summary: "a CustomDeck sheet grid is outside the editor limits (1-10 by 1-7)",
};

pub(super) const ID_OUT_OF_RANGE: RuleInfo = RuleInfo {
    id: "deck-id-out-of-range",
    default: Severity::Error,
    summary: "a DeckIDs entry points outside the object's CustomDeck sheets, or a card id is negative or not an integer",
};

pub(super) const ALL: [RuleInfo; 2] = [SIZE, ID_OUT_OF_RANGE];

const MAX_WIDTH: i64 = 10;
const MAX_HEIGHT: i64 = 7;
/// Findings per object for `deck-id-out-of-range` before they are summarized.
const MAX_ID_FINDINGS: usize = 20;

/// An integer JSON number.
fn integer(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// One grid dimension: the valid value, or what is wrong with it.
fn dimension(sheet: &Map<String, Value>, key: &str, max: i64) -> Result<i64, String> {
    match sheet.get(key) {
        None => Err(format!("{key} is missing")),
        Some(v) => match integer(v) {
            Some(n) if (1..=max).contains(&n) => Ok(n),
            Some(n) => Err(format!("{key} is {n}, expected 1..={max}")),
            None => Err(format!("{key} is not an integer (expected 1..={max})")),
        },
    }
}

fn join(base: &str, rest: &str) -> String {
    if base.is_empty() {
        rest.to_owned()
    } else {
        format!("{base}.{rest}")
    }
}

pub(super) fn check(ctx: &mut Ctx<'_>, object: &ObjectMap<'_, '_>) {
    let Some(Value::Object(deck)) = object.map.get("CustomDeck") else {
        return;
    };
    if ctx.enabled(SIZE.id) {
        for (key, sheet) in deck {
            let problems: Vec<String> = match sheet {
                Value::Object(sheet) => [
                    dimension(sheet, "NumWidth", MAX_WIDTH),
                    dimension(sheet, "NumHeight", MAX_HEIGHT),
                ]
                .into_iter()
                .filter_map(Result::err)
                .collect(),
                _ => vec!["sheet is not an object".to_owned()],
            };
            if !problems.is_empty() {
                let path = join(&join(object.path, "CustomDeck"), key);
                ctx.report(SIZE.id, &path, object.owner, || {
                    format!("CustomDeck sheet {key}: {}", problems.join(", "))
                });
            }
        }
    }
    if ctx.enabled(ID_OUT_OF_RANGE.id) {
        check_ids(ctx, object, deck);
    }
}

/// Why `id` does not point into `deck`, if it does not.
fn id_problem(deck: &Map<String, Value>, id: Option<i64>, in_deck_ids: bool) -> Option<String> {
    let Some(id) = id else {
        return Some("is not an integer".to_owned());
    };
    if id < 0 {
        return Some(format!("{id} is negative"));
    }
    // A card's own `CardID` follows the numbering of the deck it came from,
    // while its `CustomDeck` key does not (seen in real saves), so only
    // `DeckIDs` entries are matched against the sheets.
    if !in_deck_ids {
        return None;
    }
    let (sheet, index) = (id / 100, id % 100);
    let Some(entry) = deck.get(&sheet.to_string()) else {
        return Some(format!(
            "{id} needs sheet {sheet}, which CustomDeck does not have"
        ));
    };
    // An unusable grid is reported by `deck-size`.
    let Value::Object(entry) = entry else {
        return None;
    };
    let (Ok(w), Ok(h)) = (
        dimension(entry, "NumWidth", MAX_WIDTH),
        dimension(entry, "NumHeight", MAX_HEIGHT),
    ) else {
        return None;
    };
    (index >= w * h).then(|| {
        format!(
            "{id} is card {index} of sheet {sheet}, which has only {} cards",
            w * h
        )
    })
}

fn check_ids(ctx: &mut Ctx<'_>, object: &ObjectMap<'_, '_>, deck: &Map<String, Value>) {
    let mut reported = 0usize;
    let mut skipped = 0usize;
    let mut check_one = |ctx: &mut Ctx<'_>, label: &str, path: String, value: &Value| {
        let in_deck_ids = label != "CardID";
        let Some(problem) = id_problem(deck, integer(value), in_deck_ids) else {
            return;
        };
        if reported < MAX_ID_FINDINGS {
            reported += 1;
            ctx.report(ID_OUT_OF_RANGE.id, &path, object.owner, || {
                format!("{label} {problem}")
            });
        } else {
            skipped += 1;
        }
    };
    if let Some(value @ Value::Number(_)) = object.map.get("CardID") {
        check_one(ctx, "CardID", join(object.path, "CardID"), value);
    }
    if let Some(Value::Array(ids)) = object.map.get("DeckIDs") {
        for (i, value) in ids.iter().enumerate() {
            let path = format!("{}[{i}]", join(object.path, "DeckIDs"));
            check_one(ctx, "DeckIDs entry", path, value);
        }
    }
    if skipped > 0 {
        let path = join(object.path, "DeckIDs");
        ctx.report(ID_OUT_OF_RANGE.id, &path, object.owner, || {
            format!("...and {skipped} more ids outside the CustomDeck sheets")
        });
    }
}
