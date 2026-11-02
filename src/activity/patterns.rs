//! The regular expressions a job's log lines are matched against.

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{invalid, Result};

// Anchored: a marker is a line of its own, not text that happens to contain `::start`.
/// `::step 2/9 Building`, or `::step 3 Running` when the number of steps is not known.
const DEFAULT_STEP: &str = r"^\s*::step\s+(?P<n>\d+)(?:\s*/\s*(?P<of>\d+))?\s*(?P<text>.*)";
const DEFAULT_START: &str = r"^\s*::start\s+(?P<label>\S.*?)\s*$";
const DEFAULT_DONE: &str = r"^\s*::done\s+(?P<label>\S.*?)(?:\s+exit=\d+)?\s*$";
/// `::note built on the spare machine`: a badge on the run.
const DEFAULT_NOTE: &str = r"^\s*::note\s+(?P<text>\S.*?)\s*$";

/// Patterns as a job file or `config.json` gives them. A pattern left out keeps its
/// default; an empty string turns it off.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatternConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) done: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
}

/// Compiled patterns; `None` for a pattern that is off.
#[derive(Clone, Debug)]
pub(crate) struct Patterns {
    pub(crate) step: Option<Regex>,
    pub(crate) start: Option<Regex>,
    pub(crate) done: Option<Regex>,
    pub(crate) note: Option<Regex>,
}

fn compile_one(name: &str, given: Option<&str>, default: &str, groups: &[&str]) -> Result<Option<Regex>> {
    let source = given.unwrap_or(default);
    if source.is_empty() {
        return Ok(None);
    }
    let re = Regex::new(source).or_else(|e| invalid(format!("activity.{name} is not a valid regex: {e}")))?;
    if let Some(missing) = groups.iter().find(|g| !re.capture_names().any(|c| c == Some(g))) {
        return invalid(format!("activity.{name} must have a named group (?P<{missing}>...)"));
    }
    Ok(Some(re))
}

impl Patterns {
    /// Compiles a config over the defaults. A regex that does not compile, or lacks its
    /// named groups, is an error naming the pattern.
    pub(crate) fn compile(config: &PatternConfig) -> Result<Patterns> {
        Ok(Patterns {
            step: compile_one("step", config.step.as_deref(), DEFAULT_STEP, &["n"])?,
            start: compile_one("start", config.start.as_deref(), DEFAULT_START, &["label"])?,
            done: compile_one("done", config.done.as_deref(), DEFAULT_DONE, &["label"])?,
            note: compile_one("note", config.note.as_deref(), DEFAULT_NOTE, &["text"])?,
        })
    }

    /// The built-in `::step`, `::start` and `::done` markers.
    #[cfg(test)]
    pub(crate) fn defaults() -> &'static Patterns {
        static DEFAULTS: std::sync::OnceLock<Patterns> = std::sync::OnceLock::new();
        DEFAULTS.get_or_init(|| Patterns::compile(&PatternConfig::default()).expect("the built-in patterns compile"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_patterns_need_their_named_groups() {
        let cfg = |step: Option<&str>, start: Option<&str>| PatternConfig {
            step: step.map(String::from),
            start: start.map(String::from),
            done: None,
            note: None,
        };
        assert!(Patterns::compile(&cfg(Some("("), None)).is_err());
        let missing = Patterns::compile(&cfg(None, Some("go (.*)"))).unwrap_err();
        assert!(missing.to_string().contains("(?P<label>"), "{missing}");
        let off = Patterns::compile(&cfg(Some(""), None)).unwrap();
        assert!(off.step.is_none() && off.start.is_some());
        let start = Patterns::defaults().start.as_ref().unwrap();
        assert!(start.is_match("  ::start cc") && !start.is_match("grep '::start' build.log"));
    }
}
