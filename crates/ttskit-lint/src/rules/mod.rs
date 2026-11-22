//! The rule registry. A new rule is a file in this directory plus one line in
//! [`RULES`] and one in [`check_scene_object`].

mod deck;
mod duplicate_guid;
mod missing_guid;
mod non_finite_number;
mod position_out_of_bounds;
mod script;
mod state;
mod url;

use crate::config::Severity;
use crate::ctx::{Ctx, Field, ObjectMap, SceneObject};

/// Static description of a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleInfo {
    pub id: &'static str,
    /// Severity used when the configuration does not override it.
    pub default: Severity,
    pub summary: &'static str,
}

/// Every rule, sorted by id.
pub const RULES: &[RuleInfo] = &[
    deck::ID_OUT_OF_RANGE,
    deck::SIZE,
    duplicate_guid::INFO,
    duplicate_guid::NESTED_INFO,
    url::EMPTY,
    url::INSECURE,
    url::INVALID,
    state::LARGE,
    url::LEGACY_STEAM,
    url::LOCAL,
    script::LUA,
    missing_guid::INFO,
    missing_guid::NESTED_INFO,
    non_finite_number::INFO,
    position_out_of_bounds::INFO,
    state::NOT_JSON,
    script::XML,
];

/// Ids of the rules that inspect string fields.
pub(crate) fn field_rules() -> impl Iterator<Item = &'static str> {
    url::ALL.iter().chain(&script::ALL).map(|r| r.id)
}

/// Ids of the rules that inspect whole object maps.
pub(crate) fn map_rules() -> impl Iterator<Item = &'static str> {
    state::ALL.iter().chain(&deck::ALL).map(|r| r.id)
}

/// Runs every object-map rule on `object`.
pub(crate) fn check_map(ctx: &mut Ctx<'_>, object: &ObjectMap<'_, '_>) {
    state::check(ctx, object);
    deck::check(ctx, object);
}

/// Runs every field rule on `field`.
pub(crate) fn check_field<'a>(ctx: &mut Ctx<'a>, field: &Field<'a, '_>) {
    url::check(ctx, field);
    script::check(ctx, field);
}

/// Runs after the traversal, for rules that complete findings afterwards.
pub(crate) fn finish(ctx: &mut Ctx<'_>) {
    duplicate_guid::finish(ctx);
}

/// Runs every scene-object rule on `object`.
pub(crate) fn check_scene_object<'a>(ctx: &mut Ctx<'a>, object: &SceneObject<'a, '_>) {
    duplicate_guid::check(ctx, object);
    missing_guid::check(ctx, object);
    non_finite_number::check(ctx, object);
    position_out_of_bounds::check(ctx, object);
}
