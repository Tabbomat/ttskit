//! The single traversal that feeds every rule.

use std::fmt::Write;

use serde_json::{Map, Value};
use ttskit_core::Save;

use crate::config::LintConfig;
use crate::ctx::{Ctx, Field, Finding, Owner, SceneObject};
use crate::rules;

/// What the walker knows about the value it is visiting.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Plain,
    /// An array whose items are scene objects.
    SceneList,
    /// An object whose values are scene objects.
    SceneMap,
    Scene,
    /// The `ObjectStates` array of the save.
    TopList,
    /// An entry of the top-level `ObjectStates`.
    TopScene,
    /// A `CustomUIAssets` array.
    UiList,
    /// An item of a `CustomUIAssets` array.
    UiItem,
}

pub(crate) fn run(save: &Save, config: &LintConfig) -> Vec<Finding> {
    let mut ctx = Ctx::new(config, save.value());
    let mut path = String::new();
    walk_map(save.map(), true, Role::Plain, &mut path, None, &mut ctx);
    rules::finish(&mut ctx);
    ctx.into_findings()
}

fn walk<'a>(value: &'a Value, role: Role, path: &mut String, owner: Owner<'a>, ctx: &mut Ctx<'a>) {
    match value {
        Value::Array(items) => {
            let item_role = match role {
                Role::SceneList => Role::Scene,
                Role::TopList => Role::TopScene,
                Role::UiList => Role::UiItem,
                _ => Role::Plain,
            };
            for (i, item) in items.iter().enumerate() {
                let len = path.len();
                let _ = write!(path, "[{i}]");
                walk(item, item_role, path, owner, ctx);
                path.truncate(len);
            }
        }
        Value::Object(map) => walk_map(map, false, role, path, owner, ctx),
        _ => {}
    }
}

fn walk_map<'a>(
    map: &'a Map<String, Value>,
    is_root: bool,
    role: Role,
    path: &mut String,
    owner: Owner<'a>,
    ctx: &mut Ctx<'a>,
) {
    let owner = match map.get("GUID") {
        Some(Value::String(guid)) => Some((
            guid.as_str(),
            map.get("Nickname")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty()),
        )),
        _ => owner,
    };
    if matches!(role, Role::Scene | Role::TopScene) {
        let top_level = role == Role::TopScene;
        let object = SceneObject {
            map,
            path,
            owner,
            top_level,
        };
        rules::check_scene_object(ctx, &object);
    }
    for (key, child) in map {
        let child_role = match key.as_str() {
            _ if role == Role::SceneMap => Role::Scene,
            "ObjectStates" if is_root => Role::TopList,
            "ContainedObjects" => Role::SceneList,
            "States" => Role::SceneMap,
            "CustomUIAssets" => Role::UiList,
            _ => Role::Plain,
        };
        let len = path.len();
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(key);
        if ctx.field_rules_active
            && let Value::String(value) = child
        {
            let field = Field {
                key,
                value,
                path,
                owner,
                ui_asset_item: role == Role::UiItem,
            };
            rules::check_field(ctx, &field);
        }
        walk(child, child_role, path, owner, ctx);
        path.truncate(len);
    }
}
