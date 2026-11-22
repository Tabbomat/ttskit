//! Result types of a save diff.

use serde_json::Value;

/// Options controlling [`crate::diff`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiffOptions {
    /// Two transform numbers whose absolute difference is at most this value
    /// count as equal. `0.0` (the default) compares the number text exactly.
    /// Only the numbers inside `Transform` use the tolerance.
    pub tolerance: f64,
    /// Include the full line diff of Lua scripts and XML UI, not just counts.
    pub scripts: bool,
}

/// What kind of difference a [`Change`] is. The order is the report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    /// A `Transform` component.
    Transform,
    /// `LuaScript`.
    Script,
    /// `XmlUI`.
    XmlUi,
    /// `LuaScriptState`.
    ScriptState,
    /// A URL-valued field.
    Assets,
    /// Any other field.
    Other,
}

impl ChangeKind {
    /// Stable lowercase name used in text and JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Transform => "transform",
            ChangeKind::Script => "script",
            ChangeKind::XmlUi => "xml_ui",
            ChangeKind::ScriptState => "script_state",
            ChangeKind::Assets => "assets",
            ChangeKind::Other => "other",
        }
    }
}

/// Line statistics of a changed text value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineChange {
    /// Number of added lines.
    pub added: usize,
    /// Number of removed lines.
    pub removed: usize,
    /// Unified-style line diff, present only with [`DiffOptions::scripts`].
    pub diff: Option<String>,
}

/// One difference inside an object or a global part.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// The kind of difference.
    pub kind: ChangeKind,
    /// JSON path (relative to the object, or starting at the top-level key
    /// for global parts), for example `Transform.posX` or `CustomDeck.1.FaceURL`.
    pub path: String,
    /// Old value; `None` when absent or when `lines` describes the change.
    pub old: Option<Value>,
    /// New value; `None` when absent or when `lines` describes the change.
    pub new: Option<Value>,
    /// Line statistics for text values (scripts, XML UI, script state).
    pub lines: Option<LineChange>,
}

/// Where an object lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentRef {
    /// The top-level `ObjectStates` list.
    TopLevel,
    /// Inside the `ContainedObjects` or `States` of another object.
    Object {
        /// `GUID` of the container (empty when it has none).
        guid: String,
        /// `Nickname` of the container (empty when it has none).
        nickname: String,
    },
}

/// A change of parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    /// Parent in the old save.
    pub from: ParentRef,
    /// Parent in the new save.
    pub to: ParentRef,
}

/// The primary status of an object, in report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ObjectStatus {
    /// Only in the new save.
    Added,
    /// Only in the old save.
    Removed,
    /// Parent changed (it may also have changes).
    Moved,
    /// Same parent, content changed.
    Changed,
}

impl ObjectStatus {
    /// Stable lowercase name used in JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            ObjectStatus::Added => "added",
            ObjectStatus::Removed => "removed",
            ObjectStatus::Moved => "moved",
            ObjectStatus::Changed => "changed",
        }
    }
}

/// Why matching an object was uncertain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ambiguity {
    /// Its GUID occurs more than once in the old or the new save.
    DuplicateGuid,
    /// It has no (or an empty) GUID.
    NoGuid,
}

impl Ambiguity {
    /// Stable name used in JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            Ambiguity::DuplicateGuid => "duplicate_guid",
            Ambiguity::NoGuid => "no_guid",
        }
    }
}

/// The differences of one scene object.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectDiff {
    /// Added, removed, moved or changed.
    pub status: ObjectStatus,
    /// `GUID` (empty if missing).
    pub guid: String,
    /// `Nickname` (the new one for matched objects; empty if missing).
    pub nickname: String,
    /// Set when the object was matched by position instead of by unique GUID.
    pub ambiguity: Option<Ambiguity>,
    /// The parent change, for moved objects.
    pub moved: Option<Move>,
    /// Content changes, sorted by kind then path. Empty for added/removed.
    pub changes: Vec<Change>,
}

/// A GUID that occurs more than once in the old or the new save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGuid {
    /// The GUID.
    pub guid: String,
    /// Occurrences in the old save.
    pub old_count: usize,
    /// Occurrences in the new save.
    pub new_count: usize,
}

/// Which group a global part belongs to. The order is the report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlobalGroup {
    /// Global `LuaScript`.
    Script,
    /// Global `XmlUI`.
    XmlUi,
    /// Global `LuaScriptState`.
    ScriptState,
    /// `CustomUIAssets`.
    UiAssets,
    /// `Table*` keys.
    Table,
    /// `Sky*` keys.
    Sky,
    /// `Light*` keys.
    Lighting,
    /// `Hands`.
    Hands,
    /// `TabStates`.
    TabStates,
    /// Every other top-level key.
    Other,
}

impl GlobalGroup {
    /// Stable lowercase name used in text and JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            GlobalGroup::Script => "script",
            GlobalGroup::XmlUi => "xml_ui",
            GlobalGroup::ScriptState => "script_state",
            GlobalGroup::UiAssets => "ui_assets",
            GlobalGroup::Table => "table",
            GlobalGroup::Sky => "sky",
            GlobalGroup::Lighting => "lighting",
            GlobalGroup::Hands => "hands",
            GlobalGroup::TabStates => "tab_states",
            GlobalGroup::Other => "other",
        }
    }
}

/// A difference in one top-level key other than `ObjectStates`.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalDiff {
    /// Friendly group of the key.
    pub group: GlobalGroup,
    /// The top-level key.
    pub key: String,
    /// The differences inside it, sorted by kind then path.
    pub changes: Vec<Change>,
}

/// The complete difference between two saves.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SaveDiff {
    /// Object differences sorted by (status, GUID, nickname).
    pub objects: Vec<ObjectDiff>,
    /// Global differences sorted by (group, key).
    pub globals: Vec<GlobalDiff>,
    /// GUIDs occurring more than once, sorted by GUID. This is a warning and
    /// does not count towards [`SaveDiff::is_empty`].
    pub duplicate_guids: Vec<DuplicateGuid>,
}

impl SaveDiff {
    /// True when the saves have no object or global differences.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty() && self.globals.is_empty()
    }
}
