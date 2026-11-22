//! Severities, per-rule settings and the `[lint]` configuration table.

use std::str::FromStr;

use toml::{Table, Value};

use crate::rules::RULES;

/// How serious a finding is. Ordered `Info < Warning < Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    /// Lowercase name: `info`, `warning` or `error`.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

impl FromStr for Severity {
    type Err = LintError;

    fn from_str(s: &str) -> Result<Severity, LintError> {
        match s {
            "info" => Ok(Severity::Info),
            "warning" => Ok(Severity::Warning),
            "error" => Ok(Severity::Error),
            _ => Err(LintError::BadSetting(s.to_owned())),
        }
    }
}

/// Whether a rule runs, and at which severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Off,
    On(Severity),
}

impl FromStr for Setting {
    type Err = LintError;

    fn from_str(s: &str) -> Result<Setting, LintError> {
        if s == "off" {
            Ok(Setting::Off)
        } else {
            s.parse().map(Setting::On)
        }
    }
}

/// Errors from reading or changing lint configuration.
#[derive(Debug, thiserror::Error)]
pub enum LintError {
    /// The configuration text is not valid TOML.
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    /// A rule id that is not in the registry.
    #[error("unknown lint rule `{0}`")]
    UnknownRule(String),
    /// A key in the `[lint]` table that is not recognized.
    #[error("unknown key `{0}` in [lint]")]
    UnknownKey(String),
    /// A rule setting other than `off`, `info`, `warning` or `error`.
    #[error("invalid setting `{0}` (expected off, info, warning or error)")]
    BadSetting(String),
    /// A value of the wrong type or out of range; names the key.
    #[error("invalid value for `{0}`")]
    BadValue(String),
}

/// Default for [`LintConfig::state_limit`], in bytes.
pub const DEFAULT_STATE_LIMIT: usize = 1_048_576;

/// Per-rule settings and thresholds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintConfig {
    /// Parallel to [`RULES`].
    settings: Vec<Setting>,
    /// Size limit in bytes for object state such as `LuaScriptState`.
    pub state_limit: usize,
}

impl Default for LintConfig {
    fn default() -> LintConfig {
        LintConfig {
            settings: RULES.iter().map(|r| Setting::On(r.default)).collect(),
            state_limit: DEFAULT_STATE_LIMIT,
        }
    }
}

impl LintConfig {
    fn index(id: &str) -> Option<usize> {
        RULES.iter().position(|r| r.id == id)
    }

    /// The setting of rule `id`; unknown ids count as off.
    pub fn setting(&self, id: &str) -> Setting {
        Self::index(id).map_or(Setting::Off, |i| self.settings[i])
    }

    /// Sets rule `id`; fails when the id is not in the registry.
    pub fn set(&mut self, id: &str, setting: Setting) -> Result<(), LintError> {
        let i = Self::index(id).ok_or_else(|| LintError::UnknownRule(id.to_owned()))?;
        self.settings[i] = setting;
        Ok(())
    }

    /// The severity of rule `id`, or `None` when it is off.
    pub fn severity(&self, id: &str) -> Option<Severity> {
        match self.setting(id) {
            Setting::Off => None,
            Setting::On(s) => Some(s),
        }
    }

    /// Parses the whole text of a `ttskit.toml` and reads only its `[lint]`
    /// table; every other top-level key belongs to other tools and is ignored.
    pub fn from_toml_table(text: &str) -> Result<LintConfig, LintError> {
        let root: Table = text.parse()?;
        let mut config = LintConfig::default();
        let Some(lint) = root.get("lint") else {
            return Ok(config);
        };
        let Value::Table(lint) = lint else {
            return Err(LintError::BadValue("lint".into()));
        };
        for (key, value) in lint {
            match key.as_str() {
                "rules" => {
                    let Value::Table(rules) = value else {
                        return Err(LintError::BadValue("lint.rules".into()));
                    };
                    for (id, setting) in rules {
                        let Value::String(s) = setting else {
                            return Err(LintError::BadValue(format!("lint.rules.{id}")));
                        };
                        config.set(id, s.parse()?)?;
                    }
                }
                "state_limit" => {
                    config.state_limit = match value {
                        Value::Integer(n) if *n > 0 => usize::try_from(*n)
                            .map_err(|_| LintError::BadValue("lint.state_limit".into()))?,
                        _ => return Err(LintError::BadValue("lint.state_limit".into())),
                    };
                }
                other => return Err(LintError::UnknownKey(other.to_owned())),
            }
        }
        Ok(config)
    }
}
