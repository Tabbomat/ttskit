//! `non-finite-number`: NaN or infinity in an object's `Transform`.

use serde_json::Value;

use super::RuleInfo;
use crate::config::Severity;
use crate::ctx::{Ctx, SceneObject};

pub(super) const INFO: RuleInfo = RuleInfo {
    id: "non-finite-number",
    default: Severity::Error,
    summary: "a Transform field is NaN or infinite",
};

pub(super) fn check(ctx: &mut Ctx<'_>, object: &SceneObject<'_, '_>) {
    if !ctx.enabled(INFO.id) {
        return;
    }
    let Some(Value::Object(transform)) = object.map.get("Transform") else {
        return;
    };
    for (key, value) in transform {
        let shown = match value {
            Value::Number(n) if n.as_f64().is_none_or(|v| !v.is_finite()) => n.to_string(),
            Value::String(s)
                if ["NaN", "Infinity", "-Infinity"]
                    .iter()
                    .any(|w| s.eq_ignore_ascii_case(w)) =>
            {
                s.clone()
            }
            _ => continue,
        };
        let path = format!("{}.Transform.{key}", object.path);
        ctx.report(INFO.id, &path, object.owner, || {
            format!("Transform.{key} is not a finite number ({shown})")
        });
    }
}
