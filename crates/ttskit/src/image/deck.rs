//! `ttskit image deck`: assemble card images into the sheets of a custom deck.

use std::io::Write;
use std::path::PathBuf;

#[cfg(feature = "image")]
use super::{Destinations, plan_targets, severity_label, write_atomic};
#[cfg(feature = "image")]
use serde::Serialize;
#[cfg(feature = "image")]
use std::path::Path;

/// How `image deck build` fits a card of another size into the cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum FitKind {
    Contain,
    Cover,
    Stretch,
}

#[cfg_attr(not(feature = "image"), allow(dead_code))]
pub struct DeckBuildArgs {
    pub cards: Vec<PathBuf>,
    pub list: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    pub name: String,
    pub to: super::Target,
    pub quality: Option<u8>,
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub card_size: Option<(u32, u32)>,
    pub fit: Option<FitKind>,
    pub hidden: Option<PathBuf>,
    pub no_hidden_slot: bool,
    pub fill: String,
    pub max_sheet_side: Option<u32>,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
}

#[cfg_attr(not(feature = "image"), allow(dead_code))]
pub struct DeckSplitArgs {
    pub sheet: PathBuf,
    pub out_dir: Option<PathBuf>,
    pub name: Option<String>,
    pub to: super::Target,
    pub quality: Option<u8>,
    pub fill: String,
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub description: Option<PathBuf>,
    pub save: Option<PathBuf>,
    pub deck_id: Option<String>,
    pub count: Option<u32>,
    pub no_hidden_slot: bool,
    pub skip_hidden: bool,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
}

#[cfg_attr(not(feature = "image"), allow(dead_code))]
pub struct DeckObjectArgs {
    pub description: PathBuf,
    pub out: Option<PathBuf>,
    pub face_urls: Vec<String>,
    pub back_urls: Vec<String>,
    pub unique_backs: bool,
    pub config: Option<PathBuf>,
    pub nicknames_from_files: bool,
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
}

/// Parses `--card-size`: `WIDTHxHEIGHT`, both at least 1.
pub fn parse_card_size(text: &str) -> Result<(u32, u32), String> {
    let bad = || format!("invalid card size '{text}': use WIDTHxHEIGHT in pixels, e.g. 300x420");
    let (w, h) = text.split_once(['x', 'X']).ok_or_else(bad)?;
    match (w.trim().parse::<u32>(), h.trim().parse::<u32>()) {
        (Ok(w), Ok(h)) if w >= 1 && h >= 1 => Ok((w, h)),
        _ => Err(bad()),
    }
}

/// `--name` is the stem of the output files: a plain name, no path.
#[cfg(feature = "image")]
fn check_name(name: &str) -> Result<(), String> {
    let bad_char = |c: char| c.is_control() || "/\\<>:\"|?*".contains(c);
    if name.is_empty() || name == "." || name == ".." || name.contains(bad_char) {
        return Err(format!(
            "invalid --name '{name}': use a plain file name without a path (--out-dir says where the files go)"
        ));
    }
    Ok(())
}

#[cfg(not(feature = "image"))]
pub fn deck_build(_args: &DeckBuildArgs, _out: &mut dyn Write) -> Result<bool, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

#[cfg(feature = "image")]
pub fn deck_build(args: &DeckBuildArgs, out: &mut dyn Write) -> Result<bool, String> {
    use ttskit_image::{DEFAULT_JPEG_QUALITY, DeckOptions, Fit, OutputFormat};

    check_name(&args.name)?;
    let format = match args.to {
        super::Target::Png => OutputFormat::Png,
        super::Target::Jpg => OutputFormat::Jpeg,
    };
    if format == OutputFormat::Png && args.quality.is_some() {
        return Err("--quality only applies to --to jpg".to_owned());
    }
    if args.hidden.is_some() && args.no_hidden_slot {
        return Err(
            "--hidden needs the hidden-card slot; do not combine it with --no-hidden-slot"
                .to_owned(),
        );
    }
    if args.out_dir.is_none() && !args.dry_run {
        return Err(
            "say where the sheets go: --out-dir DIR (or use --dry-run to only see the plan)"
                .to_owned(),
        );
    }
    let options = DeckOptions {
        columns: args.columns,
        rows: args.rows,
        card_size: args.card_size,
        fit: args.fit.map(|f| match f {
            FitKind::Contain => Fit::Contain,
            FitKind::Cover => Fit::Cover,
            FitKind::Stretch => Fit::Stretch,
        }),
        hidden: args.hidden.clone(),
        hidden_slot: !args.no_hidden_slot,
        fill: ttskit_image::parse_color(&args.fill)?,
        max_sheet_side: args.max_sheet_side,
        format,
        quality: args.quality.unwrap_or(DEFAULT_JPEG_QUALITY),
    };

    let mut entries = args.cards.clone();
    if let Some(list) = &args.list {
        entries.extend(ttskit_image::read_card_list(list).map_err(|e| e.to_string())?);
    }
    let cards = ttskit_image::expand_card_inputs(&entries).map_err(|e| e.to_string())?;
    if cards.is_empty() {
        return Err("no cards: give image files or directories, or --list FILE".to_owned());
    }

    // Every output path is decided, and the run refused, before any card is
    // decoded or anything is written.
    let plans = ttskit_image::plan_sheets(
        cards.len(),
        options.columns,
        options.rows,
        options.hidden_slot,
    )
    .map_err(|e| e.to_string())?;
    let sheet_names: Vec<String> = if plans.len() == 1 {
        vec![format!("{}.{}", args.name, format.extension())]
    } else {
        (1..=plans.len())
            .map(|n| format!("{}_{n}.{}", args.name, format.extension()))
            .collect()
    };
    let json_name = format!("{}.json", args.name);
    let mut names = sheet_names.clone();
    names.push(json_name);
    let targets: Vec<PathBuf> = match &args.out_dir {
        Some(dir) => {
            let files: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
            let dest = Destinations {
                out: None,
                out_dir: Some(dir),
                yes: args.yes,
            };
            plan_targets(&files, &dest, None)?
                .into_iter()
                .map(|(_, target)| target)
                .collect()
        }
        None => names.iter().map(PathBuf::from).collect(),
    };
    let used: Vec<&Path> = cards
        .iter()
        .map(PathBuf::as_path)
        .chain(args.hidden.as_deref())
        .chain(args.list.as_deref())
        .collect();
    let clashes: Vec<String> = targets
        .iter()
        .filter(|t| used.iter().any(|input| same_file(input, t)))
        .map(|t| format!("{} would overwrite an input of this run", t.display()))
        .collect();
    if !clashes.is_empty() {
        return Err(clashes.join("\n"));
    }

    let mut report = DeckReport {
        dry_run: args.dry_run,
        description: None,
        files: Vec::new(),
        notes: Vec::new(),
        errors: Vec::new(),
    };
    match ttskit_image::build_deck(&cards, &options) {
        Err(error) => report.errors.push(error.to_string()),
        Ok(deck) => {
            let description = deck.description(&args.name, &sheet_names);
            let mut json = serde_json::to_string_pretty(&description).map_err(|e| e.to_string())?;
            json.push('\n');
            let outputs = deck
                .sheets
                .iter()
                .map(|s| ("sheet", s.bytes.as_slice()))
                .chain(std::iter::once(("description", json.as_bytes())));
            for ((kind, bytes), target) in outputs.zip(&targets) {
                let overwrote = target.exists();
                if !args.dry_run
                    && let Err(error) = write_atomic(target, bytes)
                {
                    report.errors.push(format!("{}: {error}", target.display()));
                    break;
                }
                report.files.push(DeckFile {
                    kind,
                    path: target.display().to_string(),
                    bytes: bytes.len() as u64,
                    overwrote,
                    written: !args.dry_run,
                });
            }
            report.notes = deck.notes;
            report.description = Some(description);
        }
    }
    report.write(args.json, out).map_err(|e| e.to_string())?;
    Ok(report.errors.is_empty())
}

#[cfg(not(feature = "image"))]
pub fn deck_split(_args: &DeckSplitArgs, _out: &mut dyn Write) -> Result<bool, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

#[cfg(feature = "image")]
pub fn deck_split(args: &DeckSplitArgs, out: &mut dyn Write) -> Result<bool, String> {
    use super::deck_grid;
    use ttskit_image::{DEFAULT_JPEG_QUALITY, OutputFormat, SplitOptions};

    let name = match &args.name {
        Some(name) => name.clone(),
        None => args
            .sheet
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{}: not a file name", args.sheet.display()))?,
    };
    check_name(&name)?;
    let format = match args.to {
        super::Target::Png => OutputFormat::Png,
        super::Target::Jpg => OutputFormat::Jpeg,
    };
    if format == OutputFormat::Png && args.quality.is_some() {
        return Err("--quality only applies to --to jpg".to_owned());
    }
    if args.out_dir.is_none() && !args.dry_run {
        return Err(
            "say where the cards go: --out-dir DIR (or use --dry-run to only see the plan)"
                .to_owned(),
        );
    }
    let fill = ttskit_image::parse_color(&args.fill)?;

    let (spec, grid_source) = if let Some(path) = &args.description {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("{}: cannot read the description: {e}", path.display()))?;
        let file = args
            .sheet
            .file_name()
            .ok_or_else(|| format!("{}: not a file name", args.sheet.display()))?
            .to_string_lossy();
        let spec = deck_grid::from_description(&text, &file)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        (spec, "description")
    } else if let Some(path) = &args.save {
        let id = args.deck_id.as_deref().ok_or("--save needs --deck-id ID")?;
        let data = std::fs::read(path)
            .map_err(|e| format!("{}: cannot read the save: {e}", path.display()))?;
        let save: serde_json::Value = serde_json::from_slice(&data)
            .map_err(|e| format!("{}: not a valid save file: {e}", path.display()))?;
        let spec =
            deck_grid::from_save(&save, id).map_err(|e| format!("{}: {e}", path.display()))?;
        (spec, "save")
    } else {
        // `deck build` reserves the hidden-card slot by default; so does this.
        let (columns, rows) = args
            .columns
            .zip(args.rows)
            .ok_or("give --columns and --rows, --description FILE or --save FILE --deck-id ID")?;
        let spec = deck_grid::GridSpec {
            columns,
            rows,
            count: None,
            hidden_slot: true,
        };
        (spec, "flags")
    };
    let options = SplitOptions {
        columns: spec.columns,
        rows: spec.rows,
        count: args.count.or(spec.count),
        hidden_slot: spec.hidden_slot && !args.no_hidden_slot,
        skip_hidden: args.skip_hidden,
        format,
        quality: args.quality.unwrap_or(DEFAULT_JPEG_QUALITY),
        fill,
    };

    // Every output path is decided, and the run refused, before the sheet is
    // decoded or anything is written.
    let plan = ttskit_image::plan_split(
        options.columns,
        options.rows,
        options.count,
        options.hidden_slot,
        options.skip_hidden,
    )
    .map_err(|e| e.to_string())?;
    let names: Vec<PathBuf> = plan
        .slots
        .iter()
        .map(|s| PathBuf::from(plan.file_name(&name, s, format.extension())))
        .collect();
    let targets: Vec<PathBuf> = match &args.out_dir {
        Some(dir) => {
            let dest = Destinations {
                out: None,
                out_dir: Some(dir),
                yes: args.yes,
            };
            plan_targets(&names, &dest, None)?
                .into_iter()
                .map(|(_, target)| target)
                .collect()
        }
        None => names,
    };
    let clashes: Vec<String> = targets
        .iter()
        .filter(|t| {
            same_file(&args.sheet, t)
                || args.description.as_deref().is_some_and(|d| same_file(d, t))
                || args.save.as_deref().is_some_and(|d| same_file(d, t))
        })
        .map(|t| format!("{} would overwrite an input of this run", t.display()))
        .collect();
    if !clashes.is_empty() {
        return Err(clashes.join("\n"));
    }

    let mut report = SplitReport {
        dry_run: args.dry_run,
        sheet: args.sheet.display().to_string(),
        columns: options.columns,
        rows: options.rows,
        cell_width: None,
        cell_height: None,
        grid_source,
        files: Vec::new(),
        notes: Vec::new(),
        errors: Vec::new(),
    };
    match ttskit_image::split_sheet(&args.sheet, &options) {
        Err(error) => report.errors.push(error.to_string()),
        Ok(split) => {
            report.cell_width = Some(split.cell_width);
            report.cell_height = Some(split.cell_height);
            for (slot, target) in split.slots.iter().zip(&targets) {
                let overwrote = target.exists();
                if !args.dry_run
                    && let Err(error) = write_atomic(target, &slot.bytes)
                {
                    report.errors.push(format!("{}: {error}", target.display()));
                    break;
                }
                report.files.push(SplitFile {
                    path: target.display().to_string(),
                    slot: slot.slot,
                    column: slot.column,
                    row: slot.row,
                    hidden: slot.is_hidden,
                    width: split.cell_width,
                    height: split.cell_height,
                    bytes: slot.bytes.len() as u64,
                    overwrote,
                    written: !args.dry_run,
                });
            }
            report.notes = split.notes;
        }
    }
    report.write(args.json, out).map_err(|e| e.to_string())?;
    Ok(report.errors.is_empty())
}

#[cfg(not(feature = "image"))]
pub fn deck_object(_args: &DeckObjectArgs, _out: &mut dyn Write) -> Result<bool, String> {
    Err(
        "this ttskit build has no image support; rebuild with `cargo build --features image`"
            .to_owned(),
    )
}

/// Finds the shared URL of local files in the project manifest.
#[cfg(feature = "image")]
struct ManifestUrls {
    manifest: ttskit_cloud::Manifest,
    local_dir: PathBuf,
}

#[cfg(feature = "image")]
impl ManifestUrls {
    fn load(config: &Path) -> Result<Self, String> {
        let project = ttskit_cloud::ProjectConfig::load(config).map_err(|e| e.to_string())?;
        let config_dir = match config.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let manifest_file = config_dir.join(ttskit_cloud::Manifest::FILE_NAME);
        let manifest = ttskit_cloud::Manifest::load(&manifest_file).map_err(|e| e.to_string())?;
        Ok(ManifestUrls {
            manifest,
            local_dir: project.resolve_local_dir(config_dir),
        })
    }

    /// The URL of the uploaded copy of `file`. Refused when the manifest has
    /// no entry for it, or only for other content (the file changed after the
    /// upload), so that an object never points at an outdated sheet.
    fn url_of(&self, file: &Path) -> Result<String, String> {
        let shown = file.display();
        let rel = crate::cloud::local_path_in(&self.local_dir, file).ok_or_else(|| {
            format!(
                "{shown}: not a file below the project's local directory ({}), so the manifest cannot know it",
                self.local_dir.display()
            )
        })?;
        let data = std::fs::read(file).map_err(|e| format!("{shown}: {e}"))?;
        let sha1 = ttskit_cloud::sha1_hex(&data);
        let of_path: Vec<_> = self
            .manifest
            .entries
            .iter()
            .filter(|e| e.path.as_deref() == Some(rel.as_str()))
            .collect();
        if of_path.is_empty() {
            return Err(format!(
                "{shown}: '{rel}' is not in the manifest; upload it first (`ttskit cloud sync push`) or give the URL"
            ));
        }
        let mut urls: Vec<&str> = of_path
            .iter()
            .filter(|e| e.sha1.eq_ignore_ascii_case(&sha1))
            .map(|e| e.url.as_str())
            .collect();
        urls.sort_unstable();
        urls.dedup();
        match urls.as_slice() {
            [url] => Ok((*url).to_owned()),
            [] => Err(format!(
                "{shown}: changed since it was uploaded (the manifest has other content for '{rel}'); upload it again (`ttskit cloud sync push`) or give the URL"
            )),
            _ => Err(format!(
                "{shown}: the manifest has {} different URLs for this content of '{rel}'; give the URL",
                urls.len()
            )),
        }
    }
}

#[cfg(feature = "image")]
fn is_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

#[cfg(feature = "image")]
pub fn deck_object(args: &DeckObjectArgs, out: &mut dyn Write) -> Result<bool, String> {
    use super::deck_object::{Meta, ObjectInput, build_object};

    if args.out.is_none() && !args.dry_run {
        return Err(
            "say where the object goes: --out FILE (or use --dry-run to only see the plan)"
                .to_owned(),
        );
    }
    let text = std::fs::read_to_string(&args.description).map_err(|e| {
        format!(
            "{}: cannot read the description: {e}",
            args.description.display()
        )
    })?;
    let description = super::deck_grid::parse_description(&text)
        .map_err(|e| format!("{}: {e}", args.description.display()))?;
    let sheets = description.sheets.len();
    let base = match args.description.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };

    let config = match &args.config {
        Some(path) => Some(path.clone()),
        None => {
            let default = PathBuf::from(ttskit_cloud::ProjectConfig::FILE_NAME);
            default.exists().then_some(default)
        }
    };
    // A value is a URL, or the local path of an uploaded file; the manifest is
    // only loaded when a path needs it.
    let mut manifest: Option<ManifestUrls> = None;
    let mut resolve = |value: &str, what: &str| -> Result<String, String> {
        if is_url(value) {
            return Ok(value.to_owned());
        }
        if manifest.is_none() {
            let config = config.as_deref().ok_or_else(|| {
                format!(
                    "{what} needs the project manifest: give --config FILE (or run in the project directory), or give URLs"
                )
            })?;
            manifest = Some(ManifestUrls::load(config)?);
        }
        manifest
            .as_ref()
            .expect("just loaded")
            .url_of(Path::new(value))
    };

    let face_urls: Vec<String> = if args.face_urls.is_empty() {
        description
            .sheets
            .iter()
            .map(|s| {
                let file = base.join(&s.file);
                resolve(&file.to_string_lossy(), "looking up the face sheets")
            })
            .collect::<Result<_, _>>()?
    } else {
        args.face_urls
            .iter()
            .map(|v| resolve(v, "a face sheet given as a path"))
            .collect::<Result<_, _>>()?
    };
    if args.back_urls.is_empty() {
        return Err(
            "give --back-url (a URL or the local path of an uploaded back image)".to_owned(),
        );
    }
    let back_urls: Vec<String> = args
        .back_urls
        .iter()
        .map(|v| resolve(v, "a back image given as a path"))
        .collect::<Result<_, _>>()?;
    if face_urls.len() != sheets {
        return Err(format!(
            "the description has {sheets} sheets, but {} --face-url values were given",
            face_urls.len()
        ));
    }

    let meta = Meta::default();
    let value = build_object(&ObjectInput {
        description: &description,
        face_urls: &face_urls,
        back_urls: &back_urls,
        unique_backs: args.unique_backs,
        meta: &meta,
        nicknames_from_files: args.nicknames_from_files,
    })?;
    let cards: usize = description.sheets.iter().map(|s| s.cards.len()).sum();
    let bytes = ttskit_core::Save::from_value(value)
        .map_err(|e| e.to_string())?
        .to_vec();

    let mut report = ObjectReport {
        dry_run: args.dry_run,
        object: if cards == 1 { "card" } else { "deck" },
        cards,
        sheets,
        face_urls,
        back_urls,
        file: None,
        errors: Vec::new(),
    };
    if let Some(target) = &args.out {
        if same_file(&args.description, target) {
            return Err(format!(
                "{} would overwrite an input of this run",
                target.display()
            ));
        }
        let overwrote = target.exists();
        if overwrote && !args.yes && !args.dry_run {
            return Err(format!(
                "{} exists; use --yes to overwrite it",
                target.display()
            ));
        }
        if !args.dry_run
            && let Err(error) = write_atomic(target, &bytes)
        {
            report.errors.push(format!("{}: {error}", target.display()));
        }
        report.file = Some(ObjectFile {
            path: target.display().to_string(),
            bytes: bytes.len() as u64,
            overwrote,
            written: !args.dry_run && report.errors.is_empty(),
        });
    }
    report.write(args.json, out).map_err(|e| e.to_string())?;
    Ok(report.errors.is_empty())
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct ObjectFile {
    path: String,
    bytes: u64,
    overwrote: bool,
    written: bool,
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct ObjectReport {
    dry_run: bool,
    /// `deck`, or `card` for a single card.
    object: &'static str,
    cards: usize,
    sheets: usize,
    face_urls: Vec<String>,
    back_urls: Vec<String>,
    file: Option<ObjectFile>,
    errors: Vec<String>,
}

#[cfg(feature = "image")]
impl ObjectReport {
    fn write(&self, json: bool, out: &mut dyn Write) -> std::io::Result<()> {
        if json {
            serde_json::to_writer(&mut *out, self)?;
            return writeln!(out);
        }
        for (n, url) in self.face_urls.iter().enumerate() {
            writeln!(out, "  sheet {} face: {url}", n + 1)?;
        }
        for (n, url) in self.back_urls.iter().enumerate() {
            writeln!(out, "  back {}: {url}", n + 1)?;
        }
        for e in &self.errors {
            writeln!(out, "error: {e}")?;
        }
        let what = format!(
            "{} with {} card{} on {} sheet{}",
            self.object,
            self.cards,
            if self.cards == 1 { "" } else { "s" },
            self.sheets,
            if self.sheets == 1 { "" } else { "s" }
        );
        let replaces = |f: &ObjectFile| if f.overwrote { ", overwrites" } else { "" };
        match &self.file {
            Some(f) if f.written => {
                writeln!(out, "{}: {what}, {} bytes{}", f.path, f.bytes, replaces(f))
            }
            Some(f) => writeln!(
                out,
                "{}: {what}, {} bytes{}{}",
                f.path,
                f.bytes,
                replaces(f),
                if self.dry_run {
                    " (dry run, nothing written)"
                } else {
                    " (not written)"
                }
            ),
            None => writeln!(out, "{what} (dry run, nothing written)"),
        }
    }
}

/// True when both paths name the same file: the same path as text, or the
/// same file on disk.
#[cfg(feature = "image")]
fn same_file(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| p.to_string_lossy().to_lowercase().replace('\\', "/");
    key(a) == key(b) || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct DeckFile {
    /// `sheet` or `description`.
    kind: &'static str,
    path: String,
    bytes: u64,
    overwrote: bool,
    written: bool,
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct DeckReport {
    dry_run: bool,
    /// The content of the description file; absent if no deck was built.
    description: Option<ttskit_image::Description>,
    files: Vec<DeckFile>,
    notes: Vec<ttskit_image::Problem>,
    errors: Vec<String>,
}

#[cfg(feature = "image")]
impl DeckReport {
    fn write(&self, json: bool, out: &mut dyn Write) -> std::io::Result<()> {
        if json {
            serde_json::to_writer(&mut *out, self)?;
            return writeln!(out);
        }
        let mut sheets = self.description.iter().flat_map(|d| d.sheets.iter());
        for file in &self.files {
            let replaces = if file.overwrote { ", overwrites" } else { "" };
            if file.kind == "sheet"
                && let Some(s) = sheets.next()
            {
                writeln!(
                    out,
                    "{}: {}x{} grid, {} cards, {}x{} px, {} bytes{replaces}",
                    file.path, s.columns, s.rows, s.card_count, s.width, s.height, file.bytes
                )?;
            } else {
                writeln!(
                    out,
                    "{}: description, {} bytes{replaces}",
                    file.path, file.bytes
                )?;
            }
        }
        for n in &self.notes {
            writeln!(out, "  {}: {}", severity_label(n.severity), n.message)?;
        }
        for e in &self.errors {
            writeln!(out, "error: {e}")?;
        }
        let verb = if self.dry_run {
            "would be written (dry run, nothing written)"
        } else {
            "written"
        };
        match &self.description {
            Some(d) => {
                let cards: u32 = d.sheets.iter().map(|s| s.card_count).sum();
                writeln!(
                    out,
                    "{cards} cards on {} sheet{} (card size {}x{} px), {} files {verb}, {} failed",
                    d.sheets.len(),
                    if d.sheets.len() == 1 { "" } else { "s" },
                    d.card_width,
                    d.card_height,
                    self.files.len(),
                    self.errors.len()
                )
            }
            None => writeln!(out, "no deck built, {} failed", self.errors.len()),
        }
    }
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct SplitFile {
    path: String,
    slot: u32,
    column: u32,
    row: u32,
    hidden: bool,
    width: u32,
    height: u32,
    bytes: u64,
    overwrote: bool,
    written: bool,
}

#[cfg(feature = "image")]
#[derive(Serialize)]
struct SplitReport {
    dry_run: bool,
    sheet: String,
    columns: u32,
    rows: u32,
    /// Absent if the sheet could not be cut.
    cell_width: Option<u32>,
    cell_height: Option<u32>,
    /// `flags`, `description` or `save`.
    grid_source: &'static str,
    files: Vec<SplitFile>,
    notes: Vec<ttskit_image::Problem>,
    errors: Vec<String>,
}

#[cfg(feature = "image")]
impl SplitReport {
    fn write(&self, json: bool, out: &mut dyn Write) -> std::io::Result<()> {
        if json {
            serde_json::to_writer(&mut *out, self)?;
            return writeln!(out);
        }
        for file in &self.files {
            let replaces = if file.overwrote { ", overwrites" } else { "" };
            let hidden = if file.hidden {
                ", hidden-card slot"
            } else {
                ""
            };
            writeln!(
                out,
                "{}: slot {} (column {}, row {}), {}x{} px, {} bytes{hidden}{replaces}",
                file.path, file.slot, file.column, file.row, file.width, file.height, file.bytes
            )?;
        }
        for n in &self.notes {
            writeln!(out, "  {}: {}", severity_label(n.severity), n.message)?;
        }
        for e in &self.errors {
            writeln!(out, "error: {e}")?;
        }
        let verb = if self.dry_run {
            "would be written (dry run, nothing written)"
        } else {
            "written"
        };
        match self.cell_width.zip(self.cell_height) {
            Some((w, h)) => writeln!(
                out,
                "{} files {verb} from a {}x{} grid of {w}x{h} px cells, {} failed",
                self.files.len(),
                self.columns,
                self.rows,
                self.errors.len()
            ),
            None => writeln!(out, "sheet not split, {} failed", self.errors.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_card_size;

    #[test]
    fn card_size_is_width_x_height() {
        assert_eq!(parse_card_size("300x420"), Ok((300, 420)));
        assert_eq!(parse_card_size(" 3 X 4 "), Ok((3, 4)));
        for bad in ["", "300", "0x4", "4x0", "-1x4", "axb", "3x4x5", "3x"] {
            assert!(parse_card_size(bad).is_err(), "{bad}");
        }
    }
}
