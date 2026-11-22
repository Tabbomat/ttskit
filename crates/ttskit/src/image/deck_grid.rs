//! Where `image deck split` gets the grid of a sheet from besides
//! `--columns` and `--rows`: the description written by `image deck build`,
//! or the `CustomDeck` entry of a save file. Pure functions, so that they can
//! be tested without files.

use std::collections::BTreeSet;

use serde_json::Value;
use ttskit_image::{DESCRIPTION_VERSION, Description};

/// The grid of a sheet and what else the source knows about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridSpec {
    pub columns: u32,
    pub rows: u32,
    /// How many cards the sheet holds, if the source says.
    pub count: Option<u32>,
    /// True when the last slot is the hidden-card slot.
    pub hidden_slot: bool,
}

/// The entry of the sheet file `sheet_file` (compared as a whole, case
/// sensitive) in the text of a description file.
pub fn from_description(text: &str, sheet_file: &str) -> Result<GridSpec, String> {
    let description = parse_description(text)?;
    let Some(sheet) = description.sheets.iter().find(|s| s.file == sheet_file) else {
        let names: Vec<&str> = description.sheets.iter().map(|s| s.file.as_str()).collect();
        return Err(format!(
            "the description has no sheet named '{sheet_file}'; it lists: {}",
            if names.is_empty() {
                "(no sheets)".to_owned()
            } else {
                names.join(", ")
            }
        ));
    };
    Ok(GridSpec {
        columns: sheet.columns,
        rows: sheet.rows,
        count: Some(sheet.card_count),
        hidden_slot: sheet.hidden_slot.is_some(),
    })
}

/// Parses the text of a description file, refusing other versions.
pub fn parse_description(text: &str) -> Result<Description, String> {
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("the description is not valid JSON: {e}"))?;
    match value.get("format_version").and_then(Value::as_u64) {
        Some(v) if v == u64::from(DESCRIPTION_VERSION) => {}
        Some(v) => {
            return Err(format!(
                "the description has format_version {v}; this ttskit reads version {DESCRIPTION_VERSION} (is it from a newer ttskit?)"
            ));
        }
        None => {
            return Err(
                "the description has no format_version; it is not a file written by `ttskit image deck build`"
                    .to_owned(),
            );
        }
    }
    serde_json::from_value(value).map_err(|e| format!("the description is malformed: {e}"))
}

/// Every `CustomDeck` map found anywhere in `value` (object states at any
/// depth, contained objects, ...).
fn custom_decks<'a>(value: &'a Value, found: &mut Vec<&'a serde_json::Map<String, Value>>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "CustomDeck"
                    && let Value::Object(decks) = child
                {
                    found.push(decks);
                }
                custom_decks(child, found);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| custom_decks(i, found)),
        _ => {}
    }
}

/// The grid of custom deck `id` in a save. Every object that uses the deck
/// repeats its entry; they must agree. `BackIsHidden: true` means the back is
/// a separate image and all slots are cards; otherwise (also when the key is
/// missing, the game's default) the last slot is the hidden-card slot.
pub fn from_save(save: &Value, id: &str) -> Result<GridSpec, String> {
    let mut decks = Vec::new();
    custom_decks(save, &mut decks);
    let mut ids = BTreeSet::new();
    let mut seen: Vec<(u32, u32, bool)> = Vec::new();
    for map in decks {
        ids.extend(map.keys().cloned());
        let Some(entry) = map.get(id) else { continue };
        let number = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| format!("CustomDeck '{id}' has no usable {key}"))
        };
        let found = (
            number("NumWidth")?,
            number("NumHeight")?,
            entry
                .get("BackIsHidden")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        );
        if !seen.contains(&found) {
            seen.push(found);
        }
    }
    match seen.as_slice() {
        [] => {
            let mut ids: Vec<String> = ids.into_iter().collect();
            ids.sort_by_key(|i| (i.parse::<u64>().unwrap_or(u64::MAX), i.clone()));
            Err(if ids.is_empty() {
                format!("the save has no CustomDeck; deck id '{id}' not found")
            } else {
                format!(
                    "deck id '{id}' not found in the save; its CustomDeck ids are: {}",
                    ids.join(", ")
                )
            })
        }
        [(columns, rows, back_is_hidden)] => Ok(GridSpec {
            columns: *columns,
            rows: *rows,
            count: None,
            hidden_slot: !back_is_hidden,
        }),
        several => {
            let list: Vec<String> = several
                .iter()
                .map(|(c, r, b)| format!("NumWidth {c}, NumHeight {r}, BackIsHidden {b}"))
                .collect();
            Err(format!(
                "the objects of the save disagree about deck id '{id}': {}",
                list.join(" / ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn description(sheets: Value) -> String {
        json!({
            "format_version": 1, "name": "d", "card_width": 3, "card_height": 4,
            "back_is_hidden": false, "sheets": sheets
        })
        .to_string()
    }

    fn sheet(file: &str, hidden: Value) -> Value {
        json!({
            "file": file, "columns": 3, "rows": 2, "width": 9, "height": 8,
            "card_count": 4, "hidden_slot": hidden, "cards": []
        })
    }

    #[test]
    fn description_sheet_is_found_by_file_name() {
        let text = description(json!([
            sheet("d_1.png", json!(5)),
            sheet("d_2.png", json!(null))
        ]));
        let spec = |name| from_description(&text, name);
        assert_eq!(
            spec("d_1.png"),
            Ok(GridSpec {
                columns: 3,
                rows: 2,
                count: Some(4),
                hidden_slot: true
            })
        );
        assert!(!spec("d_2.png").unwrap().hidden_slot);
        let err = spec("D_1.png").unwrap_err();
        assert!(
            err.contains("'D_1.png'") && err.contains("d_1.png, d_2.png"),
            "{err}"
        );
        let err = from_description(&description(json!([])), "x.png").unwrap_err();
        assert!(err.contains("(no sheets)"), "{err}");
    }

    #[test]
    fn bad_descriptions_are_errors() {
        let err = |text: &str| from_description(text, "a.png").unwrap_err();
        assert!(err("not json").contains("not valid JSON"));
        assert!(err("{}").contains("no format_version"));
        assert!(err("[]").contains("no format_version"));
        let newer = description(json!([])).replace("\"format_version\":1", "\"format_version\":2");
        assert!(err(&newer).contains("format_version 2"), "{}", err(&newer));
        let broken = json!({"format_version": 1, "name": "d"}).to_string();
        assert!(err(&broken).contains("malformed"), "{}", err(&broken));
    }

    fn deck_entry(w: u32, h: u32, hidden: bool) -> Value {
        json!({"NumWidth": w, "NumHeight": h, "BackIsHidden": hidden, "FaceURL": "x"})
    }

    #[test]
    fn save_deck_is_found_at_any_depth() {
        let save = json!({"ObjectStates": [
            {"CustomDeck": {"2": deck_entry(4, 3, true)}, "ContainedObjects": [
                {"CustomDeck": {"2": deck_entry(4, 3, true), "7": deck_entry(2, 2, false)},
                 "ContainedObjects": [{"CustomDeck": {"9": deck_entry(5, 1, false)}}]}
            ]}
        ]});
        let spec = from_save(&save, "2").unwrap();
        assert_eq!((spec.columns, spec.rows), (4, 3));
        assert!(!spec.hidden_slot, "BackIsHidden true: no hidden slot");
        assert_eq!(spec.count, None);
        let spec = from_save(&save, "7").unwrap();
        assert!(spec.hidden_slot, "BackIsHidden false: hidden slot");
        assert_eq!((from_save(&save, "9").unwrap().columns, 5), (5, 5));
    }

    #[test]
    fn missing_deck_lists_the_ids() {
        let save = json!({"ObjectStates": [
            {"CustomDeck": {"10": deck_entry(4, 3, true), "2": deck_entry(4, 3, true)}}
        ]});
        let err = from_save(&save, "3").unwrap_err();
        assert!(err.contains("ids are: 2, 10"), "{err}");
        let err = from_save(&json!({"ObjectStates": []}), "3").unwrap_err();
        assert!(err.contains("no CustomDeck"), "{err}");
    }

    #[test]
    fn disagreeing_occurrences_are_an_error() {
        for other in [
            deck_entry(5, 3, true),
            deck_entry(4, 4, true),
            deck_entry(4, 3, false),
        ] {
            let save = json!({"ObjectStates": [
                {"CustomDeck": {"1": deck_entry(4, 3, true)}},
                {"CustomDeck": {"1": other}}
            ]});
            let err = from_save(&save, "1").unwrap_err();
            assert!(err.contains("disagree about deck id '1'"), "{err}");
        }
        let save = json!({"ObjectStates": [{"CustomDeck": {"1": {"NumWidth": 2}}}]});
        assert!(from_save(&save, "1").unwrap_err().contains("NumHeight"));
        let save = json!({"CustomDeck": {"1": {"NumWidth": 2, "NumHeight": 2}}});
        assert!(from_save(&save, "1").unwrap().hidden_slot);
    }
}
