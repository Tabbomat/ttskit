//! The rule registry. A new rule is a file in this directory plus one line in
//! [`RULES`] and one in [`check_scene_object`].

mod duplicate_guid;
mod missing_guid;
mod non_finite_number;
mod position_out_of_bounds;
mod url;

use crate::config::Severity;
use crate::ctx::{Ctx, Field, SceneObject};

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
    duplicate_guid::INFO,
    duplicate_guid::NESTED_INFO,
    url::EMPTY,
    url::INSECURE,
    url::INVALID,
    url::LEGACY_STEAM,
    url::LOCAL,
    missing_guid::INFO,
    missing_guid::NESTED_INFO,
    non_finite_number::INFO,
    position_out_of_bounds::INFO,
];

/// Ids of the rules that inspect URL fields.
pub(crate) fn url_rules() -> impl Iterator<Item = &'static str> {
    url::ALL.iter().map(|r| r.id)
}

/// Runs every field rule on `field`.
pub(crate) fn check_field(ctx: &mut Ctx<'_>, field: &Field<'_, '_>) {
    url::check(ctx, field);
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
