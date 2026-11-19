//! Classification of `LuaScriptState` strings.

use serde_json::Value;

/// How a script state string is written to disk.
#[derive(Debug, Clone, PartialEq)]
pub enum StateKind {
    /// Valid JSON that equals its own compact form: written prettified as
    /// `state.json` and compacted again on pack.
    Pretty(Value),
    /// Valid JSON in another form: written verbatim as `state.json`.
    Json,
    /// Anything else: written verbatim as `state.txt`.
    Text,
}

/// The compact form of a script state value. Unpack uses this to decide
/// whether a state may be prettified and pack uses it to compact again, so
/// both must go through this function.
pub fn compact_state(value: &Value) -> String {
    serde_json::to_string(value).expect("a JSON value always serializes")
}

/// Classifies a non-empty script state string.
pub fn classify_state(state: &str) -> StateKind {
    match serde_json::from_str::<Value>(state) {
        Ok(value) if compact_state(&value) == state => StateKind::Pretty(value),
        Ok(_) => StateKind::Json,
        Err(_) => StateKind::Text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_json_is_pretty() {
        let state = r#"{"a":1,"b":[1.0,-0.0,"x"],"c":{"d":null}}"#;
        let StateKind::Pretty(value) = classify_state(state) else {
            panic!("expected Pretty");
        };
        assert_eq!(compact_state(&value), state);
        assert!(matches!(classify_state("[]"), StateKind::Pretty(_)));
        assert!(matches!(classify_state("5"), StateKind::Pretty(_)));
        assert!(matches!(classify_state("\"s\""), StateKind::Pretty(_)));
    }

    #[test]
    fn non_compact_json_stays_verbatim_json() {
        for state in [
            r#"{"a": 1}"#,
            "{\"a\":1}\n",
            " [1]",
            "{\n  \"a\": 1\n}",
            r#"{"a":1,"a":2}"#,
            "{\"a\":\"\\u00f6\"}",
        ] {
            assert_eq!(classify_state(state), StateKind::Json, "{state}");
        }
    }

    #[test]
    fn everything_else_is_text() {
        for state in ["hello", "{\"a\":", "a\r\nb", "{} {}", "nil"] {
            assert_eq!(classify_state(state), StateKind::Text, "{state}");
        }
    }
}
