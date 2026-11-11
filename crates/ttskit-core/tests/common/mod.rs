//! Helpers shared by the integration tests.

use serde_json::Value;

/// Structural equality that, unlike `Value`'s `PartialEq`, also compares the
/// order of object keys.
pub fn same_structure(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && same_structure(v1, v2))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_structure(p, q))
        }
        _ => a == b,
    }
}
