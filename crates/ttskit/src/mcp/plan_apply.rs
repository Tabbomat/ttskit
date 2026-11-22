//! The two-step pattern of the tools that change something: a `*_plan` tool
//! shows what would happen and returns a fingerprint of the plan, and the
//! matching `*_apply` tool only acts if the plan is still the same.
//!
//! An apply call (1) needs `confirm: true` and a fingerprint, (2) computes
//! the plan again from the current state, (3) refuses if its fingerprint
//! differs from the given one and returns the fresh plan, and only then
//! (4) does the work. A plan call never changes anything.
//!
//! The fingerprint is taken over a separate document (`hashed`) from the one
//! returned to the caller (`body`), so that volatile parts of a plan (such as
//! free quota) can be left out of the hash, and inputs that the plan text does
//! not show (file contents) can be put in.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Hex of the SHA-256 of `bytes`, for hashing input files into a plan.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The first 16 hex characters of the SHA-256 of the compact serialization
/// of `hashed`.
pub(super) fn fingerprint(hashed: &Value) -> String {
    let text = serde_json::to_string(hashed).expect("a JSON value serializes");
    sha256_hex(text.as_bytes())[..16].to_owned()
}

/// What the tools of a pair are called, for the hints in their results.
#[derive(Clone, Copy)]
pub(super) struct Pair {
    pub plan: &'static str,
    pub apply: &'static str,
}

/// A plan: what is shown to the caller, and what is hashed.
pub(super) struct Plan {
    pub body: Value,
    pub hashed: Value,
}

impl Plan {
    /// The plan as returned by the plan tool: `body` plus `fingerprint` and a
    /// hint for the next call.
    pub fn result(&self, pair: Pair) -> Value {
        let print = fingerprint(&self.hashed);
        let mut doc = self.body.clone();
        doc["fingerprint"] = json!(print);
        // A plan can carry a warning (such as "this deletes files"), which is
        // repeated in the hint.
        let warning = self
            .body
            .get("warning")
            .and_then(Value::as_str)
            .map(|w| format!("{w} "))
            .unwrap_or_default();
        doc["next"] = json!(format!(
            "Nothing was changed. {warning}To apply this plan, call {} with the same arguments plus fingerprint \"{print}\" and confirm: true.",
            pair.apply
        ));
        doc
    }
}

/// An error result body.
pub(super) fn error(message: impl Into<String>) -> Value {
    json!({ "error": message.into() })
}

/// Step 1: the fingerprint and `confirm: true` must be given. Returns the
/// fingerprint.
pub(super) fn require_confirmation(
    fingerprint: Option<&str>,
    confirm: Option<bool>,
    pair: Pair,
) -> Result<String, Value> {
    let print = fingerprint.map(str::trim).unwrap_or_default();
    if print.is_empty() {
        return Err(error(format!(
            "fingerprint is missing; nothing was done. Call {} first and pass the fingerprint it returns",
            pair.plan
        )));
    }
    if confirm != Some(true) {
        return Err(error(
            "confirm must be true to apply; nothing was done. Review the plan, then repeat the call with confirm: true",
        ));
    }
    Ok(print.to_owned())
}

/// Steps 2 and 3: the plan computed now must have the given fingerprint;
/// otherwise the refusal carries the fresh plan.
pub(super) fn verify(given: &str, current: &Plan, pair: Pair) -> Result<(), Value> {
    if fingerprint(&current.hashed) == given.to_ascii_lowercase() {
        return Ok(());
    }
    let mut doc = error(format!(
        "the state changed since the plan (or the fingerprint is wrong); nothing was done. Review the new plan below and call {} again with its fingerprint",
        pair.apply
    ));
    doc["applied"] = json!(false);
    doc["plan"] = current.result(pair);
    Err(doc)
}

/// Marks the result of an applied plan.
pub(super) fn applied(mut result: Value) -> Value {
    result["applied"] = json!(true);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAIR: Pair = Pair {
        plan: "x_plan",
        apply: "x_apply",
    };

    #[test]
    fn the_fingerprint_depends_on_the_hashed_document_only() {
        let a = Plan {
            body: json!({"shown": 1}),
            hashed: json!({"k": [1, 2]}),
        };
        let b = Plan {
            body: json!({"shown": 2}),
            hashed: json!({"k": [1, 2]}),
        };
        let c = Plan {
            body: json!({"shown": 1}),
            hashed: json!({"k": [1, 3]}),
        };
        assert_eq!(fingerprint(&a.hashed), fingerprint(&b.hashed));
        assert_ne!(fingerprint(&a.hashed), fingerprint(&c.hashed));
        assert_eq!(fingerprint(&a.hashed).len(), 16);
        let result = a.result(PAIR);
        assert_eq!(result["shown"], 1);
        assert_eq!(result["fingerprint"], fingerprint(&a.hashed));
        assert!(result["next"].as_str().unwrap().contains("x_apply"));
    }

    #[test]
    fn confirmation_needs_fingerprint_and_true() {
        assert!(require_confirmation(None, Some(true), PAIR).is_err());
        assert!(require_confirmation(Some(" "), Some(true), PAIR).is_err());
        assert!(require_confirmation(Some("ab"), None, PAIR).is_err());
        assert!(require_confirmation(Some("ab"), Some(false), PAIR).is_err());
        assert_eq!(
            require_confirmation(Some(" ab "), Some(true), PAIR).unwrap(),
            "ab"
        );
    }

    #[test]
    fn a_changed_plan_is_refused_with_the_fresh_plan() {
        let plan = Plan {
            body: json!({"shown": 1}),
            hashed: json!(1),
        };
        assert!(verify(&fingerprint(&plan.hashed), &plan, PAIR).is_ok());
        let refusal = verify("0000000000000000", &plan, PAIR).unwrap_err();
        assert_eq!(refusal["applied"], false);
        assert_eq!(refusal["plan"]["fingerprint"], fingerprint(&plan.hashed));
    }
}
