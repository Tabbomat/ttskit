//! The sync plan: what a one-way sync of a local directory with a cloud
//! folder would do.
//!
//! [`plan_sync`] is pure: it takes the two listings ([`LocalFile`]s from
//! [`scan_local`](crate::scan_local), and the cloud index with the cloud file
//! list) and returns a [`SyncPlan`]. Nothing is read or changed.
//!
//! Each direction runs one way. [`Direction::Push`] makes the cloud folder
//! match the local directory, [`Direction::Pull`] the other way round. Files
//! that exist only on the target side are never deleted unless
//! [`PlanOptions::prune`] is set; otherwise they are skipped.
//!
//! Files correspond by folder and name, ignoring ASCII case (as Steam Cloud
//! does); content is compared by SHA1. Paths are relative to the synced
//! folder and use `/`. A step's `path` is spelled as on the source side
//! (local for push, cloud for pull), except for deletions and skips of
//! target-only files, which use the target's spelling.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::{CloudFile, CloudIndex, IndexEntry, LocalFile, cloud_key, normalize_folder};

/// Which side is the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Local to cloud.
    Push,
    /// Cloud to local.
    Pull,
}

impl Direction {
    /// `push` or `pull`.
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Push => "push",
            Direction::Pull => "pull",
        }
    }
}

/// Options for [`plan_sync`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlanOptions {
    /// Delete files that exist only on the target side. Off by default.
    pub prune: bool,
}

/// What to do with a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Write the local file to the cloud (push).
    Upload,
    /// Write the cloud file to the local directory (pull).
    Download,
    /// Delete the file on the target side (needs prune).
    Delete,
    /// The target has the content under another path (`from`); the new path
    /// is created and `from` removed (needs prune). The cloud has no rename,
    /// so on push this is an upload followed by a delete.
    Rename,
    /// Nothing to do.
    Skip,
    /// The plan cannot be carried out for this file; see the reason.
    Conflict,
}

impl Action {
    /// All actions, in the order they are reported.
    pub const ALL: [Action; 6] = [
        Action::Conflict,
        Action::Upload,
        Action::Download,
        Action::Rename,
        Action::Delete,
        Action::Skip,
    ];

    /// The lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Upload => "upload",
            Action::Download => "download",
            Action::Delete => "delete",
            Action::Rename => "rename",
            Action::Skip => "skip",
            Action::Conflict => "conflict",
        }
    }
}

/// What kind of collision makes a file a [`Action::Conflict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collision {
    /// Several local files differ only by case.
    LocalCase,
    /// The index has several entries for one folder and name.
    DuplicateEntry,
    /// The upload would get the same cloud file name (`<SHA1>_<name>`) as
    /// another file in a different folder, so both would be one cloud file.
    SharedCloudFile,
}

/// Why a step has its action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Only the source has the file.
    New,
    /// Both have it with different content.
    Changed,
    /// Only the target has the file.
    Removed,
    /// The same content is at another path (see [`Action::Rename`]).
    Renamed,
    /// Same content; the names differ only by case.
    CaseOnly,
    /// Same content.
    Identical,
    /// The index lists the file but the cloud file is gone. On push it is
    /// uploaded again; on pull there is nothing to download.
    CloudFileGone,
    /// A target-only file that is kept because prune was not requested.
    NotPruned,
    /// See [`Collision`].
    Collision(Collision),
}

impl Reason {
    /// A short description for the text output.
    pub fn describe(self) -> &'static str {
        match self {
            Reason::New => "new",
            Reason::Changed => "changed",
            Reason::Removed => "removed from the source",
            Reason::Renamed => "renamed",
            Reason::CaseOnly => "names differ only by case",
            Reason::Identical => "identical",
            Reason::CloudFileGone => "cloud file is gone",
            Reason::NotPruned => "only on the target, kept without --prune",
            Reason::Collision(Collision::LocalCase) => "local files differ only by case",
            Reason::Collision(Collision::DuplicateEntry) => "several index entries for this name",
            Reason::Collision(Collision::SharedCloudFile) => {
                "same name and content in another folder would share one cloud file"
            }
        }
    }

    /// A stable lowercase identifier for JSON.
    pub fn id(self) -> &'static str {
        match self {
            Reason::New => "new",
            Reason::Changed => "changed",
            Reason::Removed => "removed",
            Reason::Renamed => "renamed",
            Reason::CaseOnly => "case_only",
            Reason::Identical => "identical",
            Reason::CloudFileGone => "cloud_file_gone",
            Reason::NotPruned => "not_pruned",
            Reason::Collision(Collision::LocalCase) => "collision_local_case",
            Reason::Collision(Collision::DuplicateEntry) => "collision_duplicate_entry",
            Reason::Collision(Collision::SharedCloudFile) => "collision_shared_cloud_file",
        }
    }
}

/// One file in a [`SyncPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStep {
    /// What to do.
    pub action: Action,
    /// Why.
    pub reason: Reason,
    /// Path relative to the synced folder, `/` separated.
    pub path: String,
    /// For [`Action::Rename`]: the old path on the target side.
    pub from: Option<String>,
    /// For a collision: the other paths involved.
    pub others: Vec<String>,
    /// Size of the file on the source side, if the file exists there.
    pub size: Option<u64>,
    /// SHA1 (uppercase hex) of the file's content: the source's, or the
    /// target's for target-only files.
    pub sha1: Option<String>,
    /// The cloud file name of the matching index entry, if any.
    pub cloud_key: Option<String>,
}

/// The result of [`plan_sync`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPlan {
    /// The direction planned.
    pub direction: Direction,
    /// The options used.
    pub options: PlanOptions,
    /// The cloud folder, `/` separated, spelled as the existing folder if
    /// there is one.
    pub cloud_folder: String,
    /// One step per file or rename, sorted by path ignoring ASCII case.
    pub steps: Vec<SyncStep>,
}

impl SyncPlan {
    /// The number of steps with `action`.
    pub fn count(&self, action: Action) -> usize {
        self.steps.iter().filter(|s| s.action == action).count()
    }

    /// Whether any step is a conflict. Such a plan must not be carried out.
    pub fn has_conflicts(&self) -> bool {
        self.count(Action::Conflict) > 0
    }

    /// Whether nothing would change (every step is a skip).
    pub fn is_noop(&self) -> bool {
        self.steps.iter().all(|s| s.action == Action::Skip)
    }

    /// Whether the two sides already match: a no-op in which every skip is
    /// identical or case-only.
    pub fn is_in_sync(&self) -> bool {
        self.steps
            .iter()
            .all(|s| matches!(s.reason, Reason::Identical | Reason::CaseOnly))
    }

    /// Human-readable text. Steps that are plain `identical` skips are only
    /// counted.
    pub fn render_text(&self) -> String {
        let mut out = format!(
            "{} cloud folder {}{}:\n",
            match self.direction {
                Direction::Push => "Push to",
                Direction::Pull => "Pull from",
            },
            if self.cloud_folder.is_empty() {
                "(root)"
            } else {
                &self.cloud_folder
            },
            if self.options.prune { " (prune)" } else { "" },
        );
        for step in &self.steps {
            if step.reason == Reason::Identical {
                continue;
            }
            let label = step.action.as_str();
            let what = match &step.from {
                Some(from) => format!("{} <- {from}", step.path),
                None => step.path.clone(),
            };
            let mut note = step.reason.describe().to_owned();
            if !step.others.is_empty() {
                note = format!("{note}: {}", step.others.join(", "));
            }
            out.push_str(&format!("  {label:<9}  {what}  ({note})\n"));
        }
        let mut parts: Vec<String> = Action::ALL
            .iter()
            .filter_map(|&a| {
                let n = match a {
                    Action::Skip => self
                        .steps
                        .iter()
                        .filter(|s| s.action == a && s.reason != Reason::Identical)
                        .count(),
                    _ => self.count(a),
                };
                (n > 0).then(|| format!("{n} {}", a.as_str()))
            })
            .collect();
        let identical = self
            .steps
            .iter()
            .filter(|s| s.reason == Reason::Identical)
            .count();
        if identical > 0 {
            parts.push(format!("{identical} identical"));
        }
        if parts.is_empty() {
            out.push_str("Nothing to do.\n");
        } else {
            out.push_str(&parts.join(", "));
            out.push('\n');
        }
        out
    }

    /// The plan as JSON: direction, folder, prune flag, every step and the
    /// counts per action.
    pub fn to_json(&self) -> Value {
        let steps: Vec<Value> = self
            .steps
            .iter()
            .map(|s| {
                json!({
                    "action": s.action.as_str(),
                    "reason": s.reason.id(),
                    "path": s.path,
                    "from": s.from,
                    "others": s.others,
                    "size": s.size,
                    "sha1": s.sha1,
                    "cloud_key": s.cloud_key,
                })
            })
            .collect();
        let counts: serde_json::Map<String, Value> = Action::ALL
            .iter()
            .map(|a| (a.as_str().to_owned(), json!(self.count(*a))))
            .collect();
        json!({
            "direction": self.direction.as_str(),
            "cloud_folder": self.cloud_folder,
            "prune": self.options.prune,
            "has_conflicts": self.has_conflicts(),
            "counts": counts,
            "steps": steps,
        })
    }
}

fn lower_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_ascii_lowercase()
        .cmp(&b.to_ascii_lowercase())
        .then_with(|| a.cmp(b))
}

fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

/// (lowercased relative folder, lowercased name)
type Place = (String, String);

/// Per lowercased cloud file name: the (lowercased absolute folder, lowercased
/// name) and the display path of every index entry using it.
type KeyPlaces = HashMap<String, Vec<(Place, String)>>;

fn local_place(path: &str) -> Place {
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    (
        normalize_folder(folder).to_ascii_lowercase(),
        name.to_ascii_lowercase(),
    )
}

/// The folder of an entry relative to `root_lower` (the lowercased synced
/// folder), spelled as in the index; `None` if the entry is outside.
fn relative_folder(entry_folder: &str, root_lower: &str) -> Option<String> {
    let full = normalize_folder(entry_folder);
    let full_lower = full.to_ascii_lowercase();
    if full_lower == root_lower {
        Some(String::new())
    } else if root_lower.is_empty() {
        Some(full)
    } else {
        full_lower
            .strip_prefix(root_lower)
            .filter(|r| r.starts_with('/'))
            .map(|_| full[root_lower.len() + 1..].to_owned())
    }
}

struct CloudSide<'a> {
    entry: &'a IndexEntry,
    path: String,
}

/// Works out what a one-way sync of `local` and the cloud folder `folder`
/// would do.
///
/// `folder` may use `/` or `\`; the root is the empty string. Entries outside
/// the folder and cloud files without an index entry are ignored (apart from
/// the cloud file name check for [`Collision::SharedCloudFile`]). A folder
/// that does not exist yet is fine: it is empty.
pub fn plan_sync(
    direction: Direction,
    local: &[LocalFile],
    index: &CloudIndex,
    cloud_files: &[CloudFile],
    folder: &str,
    options: PlanOptions,
) -> SyncPlan {
    let wanted = normalize_folder(folder);
    let root_lower = wanted.to_ascii_lowercase();
    let cloud_folder = index
        .all_folders()
        .into_iter()
        .find(|f| f.eq_ignore_ascii_case(&wanted))
        .unwrap_or(wanted);

    let present: HashSet<String> = cloud_files
        .iter()
        .map(|f| f.name.to_ascii_lowercase())
        .collect();

    // Index entries in scope, by place, in index order.
    let mut cloud: HashMap<Place, Vec<CloudSide>> = HashMap::new();
    for entry in &index.entries {
        if let Some(rel) = relative_folder(&entry.folder, &root_lower) {
            let place = (rel.to_ascii_lowercase(), entry.name.to_ascii_lowercase());
            cloud.entry(place).or_default().push(CloudSide {
                path: join(&rel, &entry.name),
                entry,
            });
        }
    }
    // Where each cloud file name is used in the whole index.
    let mut key_places: KeyPlaces = HashMap::new();
    for entry in &index.entries {
        let full = normalize_folder(&entry.folder);
        key_places
            .entry(entry.key.to_ascii_lowercase())
            .or_default()
            .push((
                (full.to_ascii_lowercase(), entry.name.to_ascii_lowercase()),
                join(&full, &entry.name),
            ));
    }

    let mut locals: HashMap<Place, Vec<&LocalFile>> = HashMap::new();
    for file in local {
        locals
            .entry(local_place(&file.path))
            .or_default()
            .push(file);
    }
    // The cloud file name each local file would get, to find files that
    // would share one.
    let mut local_keys: HashMap<String, Vec<&LocalFile>> = HashMap::new();
    for file in local {
        let name = file
            .path
            .rsplit_once('/')
            .map_or(file.path.as_str(), |p| p.1);
        local_keys
            .entry(cloud_key(&file.sha1, name).to_ascii_lowercase())
            .or_default()
            .push(file);
    }

    let mut places: Vec<&Place> = locals.keys().chain(cloud.keys()).collect();
    places.sort();
    places.dedup();

    let mut steps: Vec<SyncStep> = Vec::new();
    for place in places {
        let l = locals.get(place);
        let c = cloud.get(place);
        match (l, c) {
            (Some(l), _) if l.len() > 1 => {
                for f in l {
                    steps.push(conflict_step(
                        Collision::LocalCase,
                        f.path.clone(),
                        l.iter()
                            .filter(|o| o.path != f.path)
                            .map(|o| o.path.clone())
                            .collect(),
                        Some(f),
                        None,
                    ));
                }
            }
            (_, Some(c)) if c.len() > 1 => {
                let first = c[0].path.clone();
                steps.push(conflict_step(
                    Collision::DuplicateEntry,
                    first.clone(),
                    c.iter().skip(1).map(|s| s.path.clone()).collect(),
                    l.map(|l| l[0]),
                    Some(c[0].entry),
                ));
            }
            (Some(l), None) => {
                let f = l[0];
                match direction {
                    Direction::Push => {
                        steps.push(push_upload(
                            f,
                            Reason::New,
                            &root_lower,
                            &key_places,
                            &local_keys,
                        ));
                    }
                    Direction::Pull => {
                        steps.push(target_only(
                            options,
                            f.path.clone(),
                            f.size,
                            f.sha1.clone(),
                            None,
                        ));
                    }
                }
            }
            (None, Some(c)) => {
                let side = &c[0];
                let e = side.entry;
                match direction {
                    Direction::Push => steps.push(target_only(
                        options,
                        side.path.clone(),
                        e.size.unwrap_or(0),
                        e.sha1().to_owned(),
                        Some(e.key.clone()),
                    )),
                    Direction::Pull => {
                        if present.contains(&e.key.to_ascii_lowercase()) {
                            steps.push(SyncStep {
                                action: Action::Download,
                                reason: Reason::New,
                                path: side.path.clone(),
                                from: None,
                                others: Vec::new(),
                                size: e.size,
                                sha1: Some(e.sha1().to_owned()),
                                cloud_key: Some(e.key.clone()),
                            });
                        } else {
                            steps.push(gone_step(side));
                        }
                    }
                }
            }
            (Some(l), Some(c)) => {
                let f = l[0];
                let side = &c[0];
                let e = side.entry;
                let exists = present.contains(&e.key.to_ascii_lowercase());
                let same = e.sha1().eq_ignore_ascii_case(&f.sha1);
                if same && exists {
                    let spelled_alike = f.path == side.path;
                    steps.push(SyncStep {
                        action: Action::Skip,
                        reason: if spelled_alike {
                            Reason::Identical
                        } else {
                            Reason::CaseOnly
                        },
                        path: match direction {
                            Direction::Push => f.path.clone(),
                            Direction::Pull => side.path.clone(),
                        },
                        from: None,
                        others: if spelled_alike {
                            Vec::new()
                        } else {
                            vec![match direction {
                                Direction::Push => side.path.clone(),
                                Direction::Pull => f.path.clone(),
                            }]
                        },
                        size: Some(f.size),
                        sha1: Some(f.sha1.clone()),
                        cloud_key: Some(e.key.clone()),
                    });
                } else {
                    match direction {
                        Direction::Push => {
                            let reason = if same {
                                Reason::CloudFileGone
                            } else {
                                Reason::Changed
                            };
                            steps.push(push_upload(
                                f,
                                reason,
                                &root_lower,
                                &key_places,
                                &local_keys,
                            ));
                        }
                        Direction::Pull if !exists => steps.push(gone_step(side)),
                        Direction::Pull => steps.push(SyncStep {
                            action: Action::Download,
                            reason: Reason::Changed,
                            path: side.path.clone(),
                            from: None,
                            others: Vec::new(),
                            size: e.size,
                            sha1: Some(e.sha1().to_owned()),
                            cloud_key: Some(e.key.clone()),
                        }),
                    }
                }
            }
            (None, None) => {}
        }
    }

    pair_renames(&mut steps, options);
    steps.sort_by(|a, b| lower_cmp(&a.path, &b.path));
    SyncPlan {
        direction,
        options,
        cloud_folder,
        steps,
    }
}

fn conflict_step(
    kind: Collision,
    path: String,
    others: Vec<String>,
    local: Option<&LocalFile>,
    entry: Option<&IndexEntry>,
) -> SyncStep {
    SyncStep {
        action: Action::Conflict,
        reason: Reason::Collision(kind),
        path,
        from: None,
        others,
        size: local.map(|f| f.size),
        sha1: local.map(|f| f.sha1.clone()),
        cloud_key: entry.map(|e| e.key.clone()),
    }
}

/// The index entry is listed but its cloud file is gone, so a pull cannot
/// write it.
fn gone_step(side: &CloudSide) -> SyncStep {
    SyncStep {
        action: Action::Skip,
        reason: Reason::CloudFileGone,
        path: side.path.clone(),
        from: None,
        others: Vec::new(),
        size: side.entry.size,
        sha1: Some(side.entry.sha1().to_owned()),
        cloud_key: Some(side.entry.key.clone()),
    }
}

/// A file only the target has: deleted with prune, otherwise kept.
fn target_only(
    options: PlanOptions,
    path: String,
    size: u64,
    sha1: String,
    cloud_key: Option<String>,
) -> SyncStep {
    let (action, reason) = if options.prune {
        (Action::Delete, Reason::Removed)
    } else {
        (Action::Skip, Reason::NotPruned)
    };
    SyncStep {
        action,
        reason,
        path,
        from: None,
        others: Vec::new(),
        size: Some(size),
        sha1: Some(sha1),
        cloud_key,
    }
}

/// An upload, or a conflict if the file would share its cloud file with
/// another file in a different folder.
fn push_upload(
    f: &LocalFile,
    reason: Reason,
    root_lower: &str,
    key_places: &KeyPlaces,
    local_keys: &HashMap<String, Vec<&LocalFile>>,
) -> SyncStep {
    let name = f.path.rsplit_once('/').map_or(f.path.as_str(), |p| p.1);
    let key = cloud_key(&f.sha1, name);
    let key_lower = key.to_ascii_lowercase();
    let place = local_place(&f.path);
    let abs_folder = if place.0.is_empty() {
        root_lower.to_owned()
    } else if root_lower.is_empty() {
        place.0.clone()
    } else {
        format!("{root_lower}/{}", place.0)
    };
    let abs_place = (abs_folder, place.1.clone());

    let mut others: Vec<String> = Vec::new();
    if let Some(places) = key_places.get(&key_lower) {
        for (_, shown) in places.iter().filter(|(p, _)| *p != abs_place) {
            others.push(format!("cloud:{shown}"));
        }
    }
    if let Some(files) = local_keys.get(&key_lower) {
        for o in files.iter().filter(|o| local_place(&o.path) != place) {
            others.push(o.path.clone());
        }
    }
    if !others.is_empty() {
        others.sort();
        return conflict_step(
            Collision::SharedCloudFile,
            f.path.clone(),
            others,
            Some(f),
            None,
        );
    }
    SyncStep {
        action: Action::Upload,
        reason,
        path: f.path.clone(),
        from: None,
        others: Vec::new(),
        size: Some(f.size),
        sha1: Some(f.sha1.clone()),
        cloud_key: Some(key),
    }
}

/// Turns a delete and a new file with the same content, one of each, into a
/// rename. Only with prune: the old path must go away.
fn pair_renames(steps: &mut Vec<SyncStep>, options: PlanOptions) {
    if !options.prune {
        return;
    }
    let mut deletes: HashMap<String, Vec<usize>> = HashMap::new();
    let mut news: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, s) in steps.iter().enumerate() {
        let Some(sha) = &s.sha1 else { continue };
        let sha = sha.to_ascii_uppercase();
        match (s.action, s.reason) {
            (Action::Delete, Reason::Removed) => deletes.entry(sha).or_default().push(i),
            (Action::Upload | Action::Download, Reason::New) => {
                news.entry(sha).or_default().push(i);
            }
            _ => {}
        }
    }
    let mut remove: Vec<usize> = Vec::new();
    for (sha, d) in &deletes {
        if let Some(n) = news.get(sha)
            && d.len() == 1
            && n.len() == 1
        {
            let from = steps[d[0]].path.clone();
            let new = &mut steps[n[0]];
            new.action = Action::Rename;
            new.reason = Reason::Renamed;
            new.from = Some(from);
            remove.push(d[0]);
        }
    }
    remove.sort_unstable();
    for i in remove.into_iter().rev() {
        steps.remove(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha1_hex;

    fn sha(data: &str) -> String {
        sha1_hex(data.as_bytes())
    }

    fn local(path: &str, data: &str) -> LocalFile {
        LocalFile {
            path: path.to_owned(),
            size: data.len() as u64,
            sha1: sha(data),
        }
    }

    fn entry(data: &str, name: &str, folder: &str) -> IndexEntry {
        IndexEntry::new(
            format!("{}_{name}", sha(data)),
            name,
            Some(format!("https://u/{name}")),
            Some(data.len() as u64),
            None,
            folder,
        )
    }

    struct World {
        index: CloudIndex,
        files: Vec<CloudFile>,
    }

    /// Entries whose cloud files exist.
    fn world(entries: Vec<IndexEntry>) -> World {
        world_with(entries, true)
    }

    fn world_with(entries: Vec<IndexEntry>, all_present: bool) -> World {
        let files = entries
            .iter()
            .filter(|_| all_present)
            .map(|e| CloudFile {
                name: e.key.clone(),
                size: 1,
            })
            .collect();
        let mut index = CloudIndex::empty();
        index.entries = entries;
        World { index, files }
    }

    fn run(dir: Direction, w: &World, locals: &[LocalFile], folder: &str, prune: bool) -> SyncPlan {
        plan_sync(
            dir,
            locals,
            &w.index,
            &w.files,
            folder,
            PlanOptions { prune },
        )
    }

    fn summary(p: &SyncPlan) -> Vec<(&str, &'static str, &'static str)> {
        p.steps
            .iter()
            .map(|s| (s.path.as_str(), s.action.as_str(), s.reason.id()))
            .collect()
    }

    #[test]
    fn push_new_changed_removed_identical() {
        let w = world(vec![
            entry("same", "same.png", "G"),
            entry("old", "chg.png", "G"),
            entry("gone", "gone.png", "G"),
        ]);
        let locals = [
            local("same.png", "same"),
            local("chg.png", "new"),
            local("new.png", "n"),
        ];
        let p = run(Direction::Push, &w, &locals, "G", false);
        assert_eq!(
            summary(&p),
            [
                ("chg.png", "upload", "changed"),
                ("gone.png", "skip", "not_pruned"),
                ("new.png", "upload", "new"),
                ("same.png", "skip", "identical"),
            ]
        );
        assert!(!p.is_noop());
        assert!(!p.is_in_sync());
        let p = run(Direction::Push, &w, &locals, "G", true);
        assert_eq!(p.count(Action::Delete), 1);
        assert_eq!(p.steps[1].action, Action::Delete);
    }

    #[test]
    fn pull_is_the_mirror() {
        let w = world(vec![
            entry("same", "same.png", "G"),
            entry("cloud", "chg.png", "G"),
            entry("c", "cloudonly.png", "G"),
        ]);
        let locals = [
            local("same.png", "same"),
            local("chg.png", "local"),
            local("localonly.png", "l"),
        ];
        let p = run(Direction::Pull, &w, &locals, "G", false);
        assert_eq!(
            summary(&p),
            [
                ("chg.png", "download", "changed"),
                ("cloudonly.png", "download", "new"),
                ("localonly.png", "skip", "not_pruned"),
                ("same.png", "skip", "identical"),
            ]
        );
        let p = run(Direction::Pull, &w, &locals, "G", true);
        assert_eq!(p.steps[2].action, Action::Delete);
    }

    #[test]
    fn nothing_is_deleted_without_prune() {
        let w = world(vec![entry("a", "a.png", "G"), entry("b", "b.png", "G/Sub")]);
        let locals = [local("x.png", "x"), local("Sub/y.png", "y")];
        for dir in [Direction::Push, Direction::Pull] {
            let p = run(dir, &w, &locals, "G", false);
            assert_eq!(p.count(Action::Delete), 0, "{dir:?}");
            assert_eq!(p.count(Action::Rename), 0, "{dir:?}");
        }
    }

    #[test]
    fn rename_needs_prune_and_is_one_to_one() {
        let w = world(vec![entry("data", "old.png", "G")]);
        let locals = [local("new.png", "data")];
        let p = run(Direction::Push, &w, &locals, "G", false);
        assert_eq!(
            summary(&p),
            [
                ("new.png", "upload", "new"),
                ("old.png", "skip", "not_pruned")
            ]
        );
        let p = run(Direction::Push, &w, &locals, "G", true);
        assert_eq!(summary(&p), [("new.png", "rename", "renamed")]);
        assert_eq!(p.steps[0].from.as_deref(), Some("old.png"));
        // Pull: the local file is the target.
        let w2 = world(vec![entry("data", "new.png", "G")]);
        let locals2 = [local("old.png", "data")];
        let p = run(Direction::Pull, &w2, &locals2, "G", true);
        assert_eq!(summary(&p), [("new.png", "rename", "renamed")]);
        assert_eq!(p.steps[0].from.as_deref(), Some("old.png"));
        // A move between folders under a new name is a rename too.
        let w3 = world(vec![entry("data", "a.png", "G")]);
        let p = run(
            Direction::Push,
            &w3,
            &[local("Sub/b.png", "data")],
            "G",
            true,
        );
        assert_eq!(summary(&p), [("Sub/b.png", "rename", "renamed")]);
        assert_eq!(p.steps[0].from.as_deref(), Some("a.png"));
        // With the same name it would share one cloud file: a conflict, and the
        // old entry is still deleted only because the plan is refused as a whole.
        let p = run(
            Direction::Push,
            &w3,
            &[local("Sub/a.png", "data")],
            "G",
            true,
        );
        assert!(p.has_conflicts());
    }

    #[test]
    fn ambiguous_renames_stay_separate() {
        // Two old files with the same content, one new: not a 1:1 pair.
        let w = world(vec![entry("d", "a.png", "G"), entry("d", "b.png", "G")]);
        let p = run(Direction::Push, &w, &[local("c.png", "d")], "G", true);
        assert_eq!(p.count(Action::Rename), 0);
        assert_eq!(p.count(Action::Delete), 2);
        assert_eq!(p.count(Action::Upload), 1);
    }

    #[test]
    fn case_only_differences() {
        let w = world(vec![
            entry("same", "Back.png", "G"),
            entry("old", "Face.png", "G"),
        ]);
        let locals = [local("back.png", "same"), local("face.png", "new")];
        let p = run(Direction::Push, &w, &locals, "G", true);
        assert_eq!(
            summary(&p),
            [
                ("back.png", "skip", "case_only"),
                ("face.png", "upload", "changed")
            ]
        );
        assert_eq!(p.steps[0].others, ["Back.png"]);
        let p = run(Direction::Pull, &w, &locals, "G", true);
        assert_eq!(p.steps[0].path, "Back.png");
        assert_eq!(p.steps[1].path, "Face.png");
        assert_eq!(p.steps[1].action, Action::Download);
        // Only a case difference: in sync.
        let p = run(Direction::Push, &w, &locals[..1], "G", false);
        assert_eq!(p.count(Action::Skip), 2);
        let w1 = world(vec![entry("same", "Back.png", "G")]);
        let p = run(Direction::Push, &w1, &locals[..1], "G", true);
        assert!(p.is_noop() && p.is_in_sync());
    }

    #[test]
    fn folder_case_is_ignored_too() {
        let w = world(vec![entry("x", "a.png", "G\\Boards")]);
        let p = run(
            Direction::Push,
            &w,
            &[local("boards/a.png", "x")],
            "g",
            false,
        );
        assert_eq!(summary(&p), [("boards/a.png", "skip", "case_only")]);
        assert_eq!(p.cloud_folder, "G");
    }

    #[test]
    fn local_case_collision() {
        let w = world(vec![]);
        let locals = [
            local("A.png", "1"),
            local("a.PNG", "2"),
            local("ok.png", "3"),
        ];
        let p = run(Direction::Push, &w, &locals, "G", true);
        assert_eq!(
            summary(&p),
            [
                ("A.png", "conflict", "collision_local_case"),
                ("a.PNG", "conflict", "collision_local_case"),
                ("ok.png", "upload", "new"),
            ]
        );
        assert_eq!(p.steps[0].others, ["a.PNG"]);
        assert!(p.has_conflicts());
        assert!(!p.is_noop());
        let p = run(Direction::Pull, &w, &locals, "G", false);
        assert_eq!(p.count(Action::Conflict), 2);
    }

    #[test]
    fn duplicate_index_entries_conflict() {
        let w = world(vec![entry("1", "a.png", "G"), entry("2", "A.png", "G")]);
        for dir in [Direction::Push, Direction::Pull] {
            let p = run(dir, &w, &[local("a.png", "1")], "G", true);
            assert_eq!(
                summary(&p),
                [("a.png", "conflict", "collision_duplicate_entry")],
                "{dir:?}"
            );
            assert_eq!(p.steps[0].others, ["A.png"]);
        }
    }

    #[test]
    fn shared_cloud_file_conflicts_on_push() {
        // Same name and content already uploaded to another folder.
        let w = world(vec![entry("x", "a.png", "Other")]);
        let p = run(Direction::Push, &w, &[local("a.png", "x")], "G", false);
        assert_eq!(
            summary(&p),
            [("a.png", "conflict", "collision_shared_cloud_file")]
        );
        assert_eq!(p.steps[0].others, ["cloud:Other/a.png"]);
        // Two new local files in different subfolders.
        let w = world(vec![]);
        let locals = [local("A/a.png", "x"), local("B/a.png", "x")];
        let p = run(Direction::Push, &w, &locals, "G", false);
        assert_eq!(p.count(Action::Conflict), 2);
        // A pull has no such problem.
        let w = world(vec![entry("x", "a.png", "G/A"), entry("x", "a.png", "G/B")]);
        let p = run(Direction::Pull, &w, &[], "G", false);
        assert_eq!(p.count(Action::Download), 2);
    }

    #[test]
    fn broken_entries() {
        let w = world_with(
            vec![entry("x", "a.png", "G"), entry("y", "b.png", "G")],
            false,
        );
        let locals = [local("a.png", "x")];
        let p = run(Direction::Push, &w, &locals, "G", false);
        assert_eq!(
            summary(&p),
            [
                ("a.png", "upload", "cloud_file_gone"),
                ("b.png", "skip", "not_pruned")
            ]
        );
        let p = run(Direction::Push, &w, &locals, "G", true);
        assert_eq!(p.steps[1].action, Action::Delete);
        let p = run(Direction::Pull, &w, &locals, "G", false);
        assert_eq!(
            summary(&p),
            [
                ("a.png", "skip", "cloud_file_gone"),
                ("b.png", "skip", "cloud_file_gone")
            ]
        );
    }

    #[test]
    fn scope_and_missing_folder() {
        let w = world(vec![
            entry("1", "in.png", "G/Sub"),
            entry("2", "out.png", "Other"),
            entry("3", "sibling.png", "GG"),
        ]);
        let p = run(Direction::Pull, &w, &[], "G", false);
        assert_eq!(summary(&p), [("Sub/in.png", "download", "new")]);
        let p = run(
            Direction::Push,
            &w,
            &[local("a.png", "a")],
            "Nope/Deep",
            true,
        );
        assert_eq!(summary(&p), [("a.png", "upload", "new")]);
        assert_eq!(p.cloud_folder, "Nope/Deep");
        // Root folder: everything is in scope.
        let p = run(Direction::Pull, &w, &[], "", false);
        assert_eq!(p.count(Action::Download), 3);
    }

    #[test]
    fn empty_both_sides() {
        let w = world(vec![]);
        let p = run(Direction::Push, &w, &[], "G", true);
        assert!(p.steps.is_empty() && p.is_noop() && p.is_in_sync());
        assert_eq!(
            p.render_text(),
            "Push to cloud folder G (prune):\nNothing to do.\n"
        );
    }

    #[test]
    fn steps_are_sorted_ignoring_case() {
        let w = world(vec![entry("b", "b.png", "G")]);
        let locals = [
            local("C.png", "c"),
            local("a.png", "a"),
            local("B2.png", "x"),
        ];
        let p = run(Direction::Push, &w, &locals, "G", false);
        let paths: Vec<_> = p.steps.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["a.png", "b.png", "B2.png", "C.png"]);
    }

    #[test]
    fn text_output() {
        let w = world(vec![
            entry("same", "same.png", "G"),
            entry("old", "chg.png", "G"),
            entry("data", "old.png", "G"),
            entry("k", "keep.png", "G"),
        ]);
        let locals = [
            local("same.png", "same"),
            local("chg.png", "new"),
            local("new.png", "data"),
            local("x.png", "1"),
            local("X.png", "2"),
        ];
        let p = run(Direction::Push, &w, &locals, "G", true);
        assert_eq!(
            p.render_text(),
            "Push to cloud folder G (prune):\n\
             \x20 upload     chg.png  (changed)\n\
             \x20 delete     keep.png  (removed from the source)\n\
             \x20 rename     new.png <- old.png  (renamed)\n\
             \x20 conflict   X.png  (local files differ only by case: x.png)\n\
             \x20 conflict   x.png  (local files differ only by case: X.png)\n\
             2 conflict, 1 upload, 1 rename, 1 delete, 1 identical\n"
        );
    }

    #[test]
    fn json_output() {
        let w = world(vec![entry("old", "a.png", "G")]);
        let p = run(Direction::Push, &w, &[local("a.png", "new")], "G", false);
        let j = p.to_json();
        assert_eq!(j["direction"], "push");
        assert_eq!(j["cloud_folder"], "G");
        assert_eq!(j["prune"], false);
        assert_eq!(j["has_conflicts"], false);
        assert_eq!(j["counts"]["upload"], 1);
        assert_eq!(j["counts"]["delete"], 0);
        let s = &j["steps"][0];
        assert_eq!(s["action"], "upload");
        assert_eq!(s["reason"], "changed");
        assert_eq!(s["path"], "a.png");
        assert_eq!(s["from"], Value::Null);
        assert_eq!(s["sha1"], sha("new"));
        assert_eq!(s["cloud_key"], format!("{}_a.png", sha("new")));
    }
}
