//! The context handed to every rule: configuration and the findings sink.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::config::{LintConfig, Severity};

/// One problem found in a save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Id of the rule that produced it.
    pub rule: &'static str,
    pub severity: Severity,
    pub message: String,
    /// JSON path: dot-joined object keys with `[i]` for array indexes, for
    /// example `ObjectStates[3].ContainedObjects[0]`.
    pub path: String,
    /// `Nickname` of the owning object, if it is a non-empty string.
    pub nickname: Option<String>,
    /// `GUID` of the owning object: the nearest enclosing JSON object with a
    /// string `GUID` key.
    pub guid: Option<String>,
}

/// Owner of the object being walked: `(guid, nickname)`.
pub(crate) type Owner<'a> = Option<(&'a str, Option<&'a str>)>;

/// A scene object handed to the rules.
pub(crate) struct SceneObject<'a, 'p> {
    pub map: &'a Map<String, Value>,
    /// JSON path of the object.
    pub path: &'p str,
    /// Whether the object is an entry of the top-level `ObjectStates`
    /// (otherwise it is nested in `ContainedObjects` or `States`).
    pub top_level: bool,
    /// The object itself if it has a string GUID, else the nearest enclosing
    /// object that has one.
    pub owner: Owner<'a>,
}

/// Shared state of one lint run.
pub(crate) struct Ctx<'a> {
    config: &'a LintConfig,
    pub root: &'a Value,
    findings: Vec<Finding>,
    /// First scene object seen per non-empty GUID, at any level.
    pub seen_guids: HashMap<&'a str, &'a Map<String, Value>>,
    /// First top-level object seen per non-empty GUID.
    pub seen_top_guids: HashMap<&'a str, &'a Map<String, Value>>,
    /// `duplicate-guid` findings whose message still needs the path of the
    /// first occurrence: `(index into findings, first occurrence)`. Resolved
    /// in one extra traversal, only when there are any.
    pub pending_first_paths: Vec<(usize, &'a Map<String, Value>)>,
}

impl<'a> Ctx<'a> {
    pub fn new(config: &'a LintConfig, root: &'a Value) -> Ctx<'a> {
        Ctx {
            config,
            root,
            findings: Vec::new(),
            seen_guids: HashMap::new(),
            seen_top_guids: HashMap::new(),
            pending_first_paths: Vec::new(),
        }
    }

    /// Whether rule `id` is switched on.
    pub fn enabled(&self, id: &str) -> bool {
        self.config.severity(id).is_some()
    }

    /// Records a finding unless rule `id` is off. `message` is only built when
    /// the finding is recorded.
    pub fn report(
        &mut self,
        id: &'static str,
        path: &str,
        owner: Owner<'_>,
        message: impl FnOnce() -> String,
    ) {
        let Some(severity) = self.config.severity(id) else {
            return;
        };
        self.findings.push(Finding {
            rule: id,
            severity,
            message: message(),
            path: path.to_owned(),
            nickname: owner.and_then(|(_, n)| n).map(str::to_owned),
            guid: owner.map(|(g, _)| g.to_owned()),
        });
    }

    /// Number of findings recorded so far.
    pub fn finding_count(&self) -> usize {
        self.findings.len()
    }

    pub fn findings_mut(&mut self) -> &mut [Finding] {
        &mut self.findings
    }

    pub fn into_findings(self) -> Vec<Finding> {
        self.findings
    }
}
