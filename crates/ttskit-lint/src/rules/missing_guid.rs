//! `missing-guid` and `missing-guid-nested`: a scene object without a usable
//! GUID.

use serde_json::Value;

use super::RuleInfo;
use crate::config::Severity;
use crate::ctx::{Ctx, SceneObject};

pub(super) const INFO: RuleInfo = RuleInfo {
    id: "missing-guid",
    default: Severity::Warning,
    summary: "an object has no GUID, or it is not a non-empty string",
};

pub(super) const NESTED_INFO: RuleInfo = RuleInfo {
    id: "missing-guid-nested",
    default: Severity::Info,
    summary: "an object inside a container or a state has no GUID, or it is not a non-empty string",
};

/// Top-level objects are `missing-guid`, nested ones `missing-guid-nested`.
pub(super) fn check(ctx: &mut Ctx<'_>, object: &SceneObject<'_, '_>) {
    let ok = matches!(object.map.get("GUID"), Some(Value::String(g)) if !g.is_empty());
    if !ok {
        let rule = if object.top_level {
            INFO.id
        } else {
            NESTED_INFO.id
        };
        ctx.report(rule, object.path, object.owner, || {
            "object has no GUID (missing, not a string or empty)".to_owned()
        });
    }
}
