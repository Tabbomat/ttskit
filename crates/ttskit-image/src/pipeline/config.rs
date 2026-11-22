//! Parsing and validation of `[[image.pipeline]]` rules.
//!
//! ```toml
//! [[image.pipeline]]
//! src = "art/boards/**/*.png"   # glob, relative to the directory of ttskit.toml
//! out = "assets/Boards"         # output directory, relative to the same directory
//! steps = [
//!   { op = "resize", max-side = 4096 },
//!   { op = "optimize", level = 4 },
//! ]
//! deck = { name = "base", hidden = "art/cards/hidden.png" }   # optional
//! ```
//!
//! The `[cloud]`, `[local]` and `[lint]` tables belong to other tools and are
//! ignored here.
//!
//! All keys are kebab-case and unknown keys are refused at every level.

use std::path::PathBuf;

use globset::{GlobBuilder, GlobMatcher};
use serde::Deserialize;
use serde::de::IgnoredAny;

use super::values::{parse_card_size, parse_dither, parse_quality, parse_scale};
use crate::{
    ConvertOptions, DEFAULT_COLORS, DEFAULT_JPEG_QUALITY, DEFAULT_LEVEL, DeckOptions, Engine,
    Filter, Fit, MAX_COLORS, MAX_COLUMNS, MAX_LEVEL, MAX_ROWS, MIN_COLORS, OptimizeOptions,
    OutputFormat, QuantizeOptions, ResizeOptions, ResizeTarget, StripMode, parse_color,
};

/// Errors from parsing a project config's image pipeline.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PipelineError {
    /// The text is not valid TOML, has a missing or unknown key, or a value
    /// of the wrong type. The message includes the position.
    #[error("invalid config: {0}")]
    Syntax(String),
    /// A value is not acceptable.
    #[error("invalid config: `{key}` {message}")]
    Invalid {
        /// The key with 0-based indexes, e.g. `image.pipeline[2].steps[1].level`.
        key: String,
        /// Why the value is refused.
        message: String,
    },
}

fn invalid<T>(key: &str, message: impl Into<String>) -> Result<T, PipelineError> {
    Err(PipelineError::Invalid {
        key: key.to_owned(),
        message: message.into(),
    })
}

/// The validated image pipeline: rules in config order.
#[derive(Debug, Clone)]
pub struct Pipeline {
    pub rules: Vec<Rule>,
}

/// One `[[image.pipeline]]` rule.
#[derive(Clone)]
pub struct Rule {
    /// The source glob, normalized: `/` separators, no leading `./`.
    pub src: String,
    /// The leading directory of `src` without glob characters, relative to
    /// the config directory (empty for the config directory itself). For a
    /// `src` without glob characters (a single file) it is the file's parent.
    /// A matched file `<base>/a/x.png` maps to `<out>/a/x.png`.
    pub base: PathBuf,
    matcher: GlobMatcher,
    /// The output directory, relative to the config directory (empty for the
    /// config directory itself).
    pub out: PathBuf,
    pub steps: Vec<Step>,
    pub deck: Option<DeckRule>,
}

impl std::fmt::Debug for Rule {
    // The compiled matcher is huge and says nothing the `src` does not.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rule")
            .field("src", &self.src)
            .field("base", &self.base)
            .field("out", &self.out)
            .field("steps", &self.steps)
            .field("deck", &self.deck)
            .finish()
    }
}

impl Rule {
    /// Whether `rel`, a `/`-separated path relative to the config directory,
    /// is a source of this rule. Case-insensitive; `*` does not cross `/`.
    pub fn matches(&self, rel: &str) -> bool {
        self.matcher.is_match(rel)
    }
}

/// A step of a rule, with every option validated and defaulted.
#[derive(Debug, Clone)]
pub enum Step {
    Convert(ConvertOptions),
    Resize(ResizeOptions),
    Optimize(OptimizeOptions),
    Quantize(QuantizeOptions),
}

impl Step {
    /// A canonical text of the step and all its effective options (defaults
    /// filled in). Equal for equal behaviour, so a cache can use it to
    /// detect a changed step.
    pub fn fingerprint(&self) -> String {
        match self {
            Step::Convert(o) => format!("convert {o:?}"),
            Step::Resize(o) => format!("resize {o:?}"),
            Step::Optimize(o) => format!("optimize {o:?}"),
            Step::Quantize(o) => format!("quantize {o:?}"),
        }
    }
}

/// The `deck` table of a rule.
#[derive(Debug, Clone)]
pub struct DeckRule {
    /// The stem of the sheet files.
    pub name: String,
    /// The hidden-card image, relative to the config directory. It is not
    /// copied into `options.hidden`; the planner resolves it.
    pub hidden: Option<PathBuf>,
    /// The validated deck options; `hidden` is always `None`.
    pub options: DeckOptions,
}

impl DeckRule {
    /// A canonical text of the deck description with defaults filled in.
    pub fn fingerprint(&self) -> String {
        format!(
            "deck name={:?} hidden={:?} {:?}",
            self.name, self.hidden, self.options
        )
    }
}

// ---- raw (serde) layer ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    image: Option<RawImage>,
    #[allow(dead_code)]
    cloud: Option<IgnoredAny>,
    #[allow(dead_code)]
    local: Option<IgnoredAny>,
    #[allow(dead_code)]
    lint: Option<IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawImage {
    #[serde(default)]
    pipeline: Vec<RawRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawRule {
    src: String,
    out: String,
    #[serde(default)]
    steps: Vec<RawStep>,
    deck: Option<RawDeck>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
enum RawStep {
    Convert(RawConvert),
    Resize(RawResize),
    Optimize(RawOptimize),
    Quantize(RawQuantize),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawConvert {
    to: String,
    quality: Option<i64>,
    background: Option<String>,
    #[serde(default)]
    keep_metadata: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawResize {
    max_side: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    scale: Option<f64>,
    #[serde(default)]
    allow_enlarge: bool,
    filter: Option<String>,
    quality: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawOptimize {
    level: Option<i64>,
    strip: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawQuantize {
    colors: Option<i64>,
    quality: Option<String>,
    dither: Option<f64>,
    level: Option<i64>,
    strip: Option<String>,
    engine: Option<String>,
    program: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawDeck {
    name: String,
    hidden: Option<String>,
    columns: Option<i64>,
    rows: Option<i64>,
    card_size: Option<String>,
    fit: Option<String>,
    hidden_slot: Option<bool>,
    fill: Option<String>,
    max_sheet_side: Option<i64>,
    to: Option<String>,
    quality: Option<i64>,
}

// ---- entry point ----

/// Parses the whole `ttskit.toml` text and validates its image pipeline. The
/// `[cloud]` and `[local]` sections are neither required nor checked. Without
/// an `[image]` section the pipeline has no rules.
pub fn parse_pipeline(text: &str) -> Result<Pipeline, PipelineError> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| PipelineError::Syntax(e.to_string()))?;
    let rules = raw
        .image
        .map(|i| i.pipeline)
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(i, rule)| validate_rule(&format!("image.pipeline[{i}]"), rule))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Pipeline { rules })
}

fn validate_rule(key: &str, raw: RawRule) -> Result<Rule, PipelineError> {
    let src = clean_path(&format!("{key}.src"), &raw.src, false)?;
    let (matcher, base) = compile_src(&format!("{key}.src"), &src)?;
    let out = clean_path(&format!("{key}.out"), &raw.out, true)?;
    let mut steps = Vec::new();
    for (j, step) in raw.steps.into_iter().enumerate() {
        let skey = format!("{key}.steps[{j}]");
        if raw.deck.is_some() && matches!(step, RawStep::Convert(_)) {
            return invalid(
                &skey,
                "must not be a `convert` step in a rule with `deck`; use `deck.to` instead, \
                 because the deck description names the sheet files",
            );
        }
        steps.push(validate_step(&skey, step)?);
    }
    let deck = raw
        .deck
        .map(|d| validate_deck(&format!("{key}.deck"), d))
        .transpose()?;
    Ok(Rule {
        src,
        base: PathBuf::from(base),
        matcher,
        out: if out == "." {
            PathBuf::new()
        } else {
            PathBuf::from(out)
        },
        steps,
        deck,
    })
}

// ---- paths and globs ----

/// Normalizes a config path: `/` separators, relative, no `..`, no `.` or
/// empty components. Returns `.` when nothing is left and `allow_dot` is set.
fn clean_path(key: &str, value: &str, allow_dot: bool) -> Result<String, PipelineError> {
    if value.trim().is_empty() {
        return invalid(key, "must not be empty");
    }
    if value.chars().any(char::is_control) {
        return invalid(key, "must not contain control characters");
    }
    let value = value.replace('\\', "/");
    if value.starts_with('/') || value.chars().nth(1) == Some(':') {
        return invalid(key, "must be relative to the config file");
    }
    if value.split('/').any(|c| c == "..") {
        return invalid(key, "must not contain `..`");
    }
    let parts: Vec<&str> = value
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if parts.is_empty() {
        if allow_dot {
            return Ok(".".to_owned());
        }
        return invalid(key, "must name a file or glob");
    }
    Ok(parts.join("/"))
}

fn has_glob_chars(text: &str) -> bool {
    text.contains(['*', '?', '[', ']', '{', '}'])
}

/// Compiles the glob and computes its base directory (`/` separated).
fn compile_src(key: &str, src: &str) -> Result<(GlobMatcher, String), PipelineError> {
    let glob = GlobBuilder::new(src)
        .case_insensitive(true)
        .literal_separator(true)
        .build();
    let matcher = match glob {
        Ok(g) => g.compile_matcher(),
        Err(e) => return invalid(key, format!("is not a valid glob: {e}")),
    };
    let parts: Vec<&str> = src.split('/').collect();
    let literal = if has_glob_chars(src) {
        parts.iter().take_while(|c| !has_glob_chars(c)).count()
    } else {
        parts.len() - 1 // a single file: its parent directory
    };
    Ok((matcher, parts[..literal].join("/")))
}

// ---- value helpers ----

/// Checks an integer against `lo..=hi`.
fn int_in(key: &str, value: i64, lo: i64, hi: i64) -> Result<i64, PipelineError> {
    if (lo..=hi).contains(&value) {
        Ok(value)
    } else if hi == i64::from(u32::MAX) {
        invalid(key, format!("must be at least {lo}, not {value}"))
    } else {
        invalid(key, format!("must be between {lo} and {hi}, not {value}"))
    }
}

fn size(key: &str, value: i64) -> Result<u32, PipelineError> {
    Ok(int_in(key, value, 1, i64::from(u32::MAX))? as u32)
}

fn jpeg_quality(key: &str, value: Option<i64>) -> Result<u8, PipelineError> {
    match value {
        Some(q) => Ok(int_in(key, q, 1, 100)? as u8),
        None => Ok(DEFAULT_JPEG_QUALITY),
    }
}

fn level(key: &str, value: Option<i64>) -> Result<u8, PipelineError> {
    match value {
        Some(l) => Ok(int_in(key, l, 0, i64::from(MAX_LEVEL))? as u8),
        None => Ok(DEFAULT_LEVEL),
    }
}

fn strip(key: &str, value: Option<&str>) -> Result<StripMode, PipelineError> {
    match value {
        None | Some("safe") => Ok(StripMode::Safe),
        Some("all") => Ok(StripMode::All),
        Some("none") => Ok(StripMode::None),
        Some(other) => invalid(
            key,
            format!("must be \"safe\", \"all\" or \"none\", not {other:?}"),
        ),
    }
}

fn colour(key: &str, text: &str) -> Result<[u8; 3], PipelineError> {
    parse_color(text).or_else(|m| invalid(key, m))
}

// ---- steps ----

fn validate_step(key: &str, raw: RawStep) -> Result<Step, PipelineError> {
    let k = |field: &str| format!("{key}.{field}");
    match raw {
        RawStep::Convert(c) => {
            let format = match c.to.as_str() {
                "png" => OutputFormat::Png,
                "jpg" | "jpeg" => OutputFormat::Jpeg,
                other => {
                    return invalid(
                        &k("to"),
                        format!("must be \"png\" or \"jpg\", not {other:?}"),
                    );
                }
            };
            let mut options = ConvertOptions::new(format);
            if format == OutputFormat::Png {
                if c.quality.is_some() {
                    return invalid(&k("quality"), "only applies to `to = \"jpg\"`");
                }
                if c.background.is_some() {
                    return invalid(&k("background"), "only applies to `to = \"jpg\"`");
                }
            }
            options.quality = jpeg_quality(&k("quality"), c.quality)?;
            if let Some(bg) = &c.background {
                options.background = Some(colour(&k("background"), bg)?);
            }
            options.keep_metadata = c.keep_metadata;
            Ok(Step::Convert(options))
        }
        RawStep::Resize(r) => {
            let target = match (r.max_side, r.width, r.height, r.scale) {
                (Some(n), None, None, None) => ResizeTarget::MaxSide(size(&k("max-side"), n)?),
                (None, Some(n), None, None) => ResizeTarget::Width(size(&k("width"), n)?),
                (None, None, Some(n), None) => ResizeTarget::Height(size(&k("height"), n)?),
                (None, None, None, Some(f)) => ResizeTarget::Scale(
                    parse_scale(&f.to_string()).or_else(|m| invalid(&k("scale"), m))?,
                ),
                _ => {
                    let names: Vec<String> = [
                        ("max-side", r.max_side.is_some()),
                        ("width", r.width.is_some()),
                        ("height", r.height.is_some()),
                        ("scale", r.scale.is_some()),
                    ]
                    .iter()
                    .filter(|(_, set)| *set)
                    .map(|(n, _)| format!("`{n}`"))
                    .collect();
                    let what = if names.is_empty() {
                        "needs one of `max-side`, `width`, `height` or `scale`".to_owned()
                    } else {
                        format!("takes exactly one size, but {} are given", names.join(", "))
                    };
                    return invalid(key, format!("(resize) {what}"));
                }
            };
            let filter = match r.filter.as_deref() {
                None | Some("lanczos3") => Filter::Lanczos3,
                Some("catmull-rom") => Filter::CatmullRom,
                Some("triangle") => Filter::Triangle,
                Some("nearest") => Filter::Nearest,
                Some(other) => {
                    return invalid(
                        &k("filter"),
                        format!(
                            "must be \"lanczos3\", \"catmull-rom\", \"triangle\" or \"nearest\", not {other:?}"
                        ),
                    );
                }
            };
            let mut options = ResizeOptions::new(target);
            options.allow_enlarge = r.allow_enlarge;
            options.filter = filter;
            options.jpeg_quality = jpeg_quality(&k("quality"), r.quality)?;
            Ok(Step::Resize(options))
        }
        RawStep::Optimize(o) => Ok(Step::Optimize(OptimizeOptions {
            level: level(&k("level"), o.level)?,
            strip: strip(&k("strip"), o.strip.as_deref())?,
        })),
        RawStep::Quantize(q) => {
            let mut options = QuantizeOptions {
                colors: DEFAULT_COLORS,
                ..QuantizeOptions::default()
            };
            if let Some(c) = q.colors {
                options.colors = int_in(
                    &k("colors"),
                    c,
                    i64::from(MIN_COLORS),
                    i64::from(MAX_COLORS),
                )? as u16;
            }
            if let Some(text) = &q.quality {
                options.quality = Some(parse_quality(text).or_else(|m| invalid(&k("quality"), m))?);
            }
            if let Some(d) = q.dither {
                options.dither =
                    parse_dither(&d.to_string()).or_else(|m| invalid(&k("dither"), m))?;
            }
            options.level = level(&k("level"), q.level)?;
            options.strip = strip(&k("strip"), q.strip.as_deref())?;
            let pngquant = match q.engine.as_deref() {
                None => q.program.is_some(),
                Some("builtin") => {
                    if q.program.is_some() {
                        return invalid(
                            &k("program"),
                            "needs the pngquant engine; it conflicts with `engine = \"builtin\"`",
                        );
                    }
                    false
                }
                Some("pngquant") => true,
                Some(other) => {
                    return invalid(
                        &k("engine"),
                        format!("must be \"builtin\" or \"pngquant\", not {other:?}"),
                    );
                }
            };
            if pngquant {
                let program = match &q.program {
                    Some(p) if p.trim().is_empty() => {
                        return invalid(&k("program"), "must not be empty");
                    }
                    Some(p) if p.chars().any(char::is_control) => {
                        return invalid(&k("program"), "must not contain control characters");
                    }
                    Some(p) => Some(PathBuf::from(p)),
                    None => None,
                };
                options.engine = Engine::Pngquant { program };
            }
            Ok(Step::Quantize(options))
        }
    }
}

// ---- deck ----

/// `name` is the stem of the output files: a plain name, no path (the same
/// rule as `ttskit image deck build --name`).
fn check_name(key: &str, name: &str) -> Result<(), PipelineError> {
    let bad_char = |c: char| c.is_control() || "/\\<>:\"|?*".contains(c);
    if name.is_empty() || name == "." || name == ".." || name.contains(bad_char) {
        return invalid(
            key,
            format!("{name:?} is not a plain file name; use a name without a path"),
        );
    }
    Ok(())
}

fn validate_deck(key: &str, raw: RawDeck) -> Result<DeckRule, PipelineError> {
    let k = |field: &str| format!("{key}.{field}");
    check_name(&k("name"), &raw.name)?;
    let format = match raw.to.as_deref() {
        None | Some("png") => OutputFormat::Png,
        Some("jpg" | "jpeg") => OutputFormat::Jpeg,
        Some(other) => {
            return invalid(
                &k("to"),
                format!("must be \"png\" or \"jpg\", not {other:?}"),
            );
        }
    };
    if format == OutputFormat::Png && raw.quality.is_some() {
        return invalid(&k("quality"), "only applies to `to = \"jpg\"`");
    }
    let hidden_slot = raw.hidden_slot.unwrap_or(true);
    let hidden = match &raw.hidden {
        Some(h) => {
            let h = clean_path(&k("hidden"), h, false)?;
            if !hidden_slot {
                return invalid(
                    &k("hidden"),
                    "needs the hidden-card slot; remove `hidden-slot = false`",
                );
            }
            Some(PathBuf::from(h))
        }
        None => None,
    };
    let mut options = DeckOptions {
        hidden_slot,
        format,
        quality: jpeg_quality(&k("quality"), raw.quality)?,
        ..DeckOptions::default()
    };
    if let Some(c) = raw.columns {
        options.columns = Some(int_in(&k("columns"), c, 1, i64::from(MAX_COLUMNS))? as u32);
    }
    if let Some(r) = raw.rows {
        options.rows = Some(int_in(&k("rows"), r, 1, i64::from(MAX_ROWS))? as u32);
    }
    if let Some(text) = &raw.card_size {
        options.card_size = Some(parse_card_size(text).or_else(|m| invalid(&k("card-size"), m))?);
    }
    options.fit = match raw.fit.as_deref() {
        None => None,
        Some("contain") => Some(Fit::Contain),
        Some("cover") => Some(Fit::Cover),
        Some("stretch") => Some(Fit::Stretch),
        Some(other) => {
            return invalid(
                &k("fit"),
                format!("must be \"contain\", \"cover\" or \"stretch\", not {other:?}"),
            );
        }
    };
    if let Some(f) = &raw.fill {
        options.fill = colour(&k("fill"), f)?;
    }
    if let Some(m) = raw.max_sheet_side {
        options.max_sheet_side = Some(size(&k("max-sheet-side"), m)?);
    }
    Ok(DeckRule {
        name: raw.name,
        hidden,
        options,
    })
}
