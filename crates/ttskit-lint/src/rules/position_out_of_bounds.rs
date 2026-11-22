//! `position-out-of-bounds`: an object placed absurdly far from the origin.

use serde_json::Value;

use super::RuleInfo;
use crate::config::Severity;
use crate::ctx::{Ctx, SceneObject};

pub(super) const INFO: RuleInfo = RuleInfo {
    id: "position-out-of-bounds",
    default: Severity::Warning,
    summary: "an object's position is far outside the playable area",
};

/// Largest absolute coordinate that is not reported. Real saves have a median
/// around 34 and a 99th percentile around 138, with one mod reaching 2000.
const LIMIT: f64 = 1000.0;

/// Non-finite values are left to `non-finite-number`.
pub(super) fn check(ctx: &mut Ctx<'_>, object: &SceneObject<'_, '_>) {
    if !ctx.enabled(INFO.id) {
        return;
    }
    let Some(Value::Object(transform)) = object.map.get("Transform") else {
        return;
    };
    let mut worst: Option<(&str, f64, String)> = None;
    for axis in ["posX", "posY", "posZ"] {
        let Some(Value::Number(n)) = transform.get(axis) else {
            continue;
        };
        let Some(v) = n.as_f64().filter(|v| v.is_finite() && v.abs() > LIMIT) else {
            continue;
        };
        if worst.as_ref().is_none_or(|(_, w, _)| v.abs() > w.abs()) {
            worst = Some((axis, v, n.to_string()));
        }
    }
    if let Some((axis, _, text)) = worst {
        ctx.report(INFO.id, object.path, object.owner, || {
            format!("{axis} = {text} is outside the +-{LIMIT} limit")
        });
    }
}
