//! Script state rules: `large-script-state` and `script-state-not-json`.

use serde::de::IgnoredAny;
use serde_json::Value;

use super::RuleInfo;
use crate::config::Severity;
use crate::ctx::{Ctx, ObjectMap};

pub(super) const LARGE: RuleInfo = RuleInfo {
    id: "large-script-state",
    default: Severity::Warning,
    summary: "a LuaScriptState is longer than the configured state_limit",
};

pub(super) const NOT_JSON: RuleInfo = RuleInfo {
    id: "script-state-not-json",
    default: Severity::Warning,
    summary: "a LuaScriptState is not JSON although its script calls JSON.decode",
};

pub(super) const ALL: [RuleInfo; 2] = [LARGE, NOT_JSON];

pub(super) fn check(ctx: &mut Ctx<'_>, object: &ObjectMap<'_, '_>) {
    let Some(Value::String(state)) = object.map.get("LuaScriptState") else {
        return;
    };
    let path = || {
        if object.path.is_empty() {
            "LuaScriptState".to_owned()
        } else {
            format!("{}.LuaScriptState", object.path)
        }
    };
    let limit = ctx.state_limit();
    if state.len() > limit {
        let size = state.len();
        ctx.report(LARGE.id, &path(), object.owner, || {
            format!("LuaScriptState is {size} bytes, over the limit of {limit} bytes")
        });
    }
    if state.trim().is_empty() || !ctx.enabled(NOT_JSON.id) {
        return;
    }
    let expects_json = matches!(
        object.map.get("LuaScript"),
        Some(Value::String(script)) if script.contains("JSON.decode")
    );
    if expects_json && let Err(e) = serde_json::from_str::<IgnoredAny>(state) {
        ctx.report(NOT_JSON.id, &path(), object.owner, || {
            format!("LuaScriptState is not valid JSON ({e}) but the script calls JSON.decode")
        });
    }
}
