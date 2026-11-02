//! The job file format. `JobSpec` is exactly what a `.json` file in the inbox may contain
//! (`submit` writes one too); `JobSpec::resolve` checks it and turns it into the `Job` the
//! queue runs. Unknown fields are refused, so a misspelt `timout` is an error, not a silence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::activity::{PatternConfig, Patterns};
use crate::error::{invalid, Result};
use crate::fsutil::FileId;
use crate::job::JobFiles;
use crate::naming;
use crate::paths;
use crate::sources::safe_link;

/// The longest timeout a job may ask for: a year.
const MAX_TIMEOUT_SECONDS: u64 = 365 * 86_400;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct JobSpec {
    pub(crate) command: CommandSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) priority: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) variant: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<SourceSpec>,
    /// Shorthand for `source.session`, for tools that only know their session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) activity: Option<PatternConfig>,
    /// Regular expressions that fail a run whose command exited 0, when a line of its log
    /// matches one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fail_patterns: Vec<String>,
    /// A command run while the job waits, outside any slot; the job runs only once it has
    /// succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prepare: Option<CommandSpec>,
    /// A concurrency group: at most one of its jobs runs and one waits; a newer waiting job
    /// replaces an older one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group: Option<String>,
    /// Whether the job may run on a worker (`config.json`'s `workers`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remote: Option<RemoteMode>,
    /// Tags a worker must have to run it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) requires: Vec<String>,
    /// What to mirror to a worker before running it there; the default is its `cwd`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) sync: Vec<SyncSpec>,
    /// Mirror the git metadata of synced checkouts too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) sync_git: bool,
    /// The daemon's environment variables, by name or `PREFIX_*`, that go with it to a worker.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) env_forward: Vec<String>,
    /// Set by a dispatcher on a job it hands to a worker: the dispatching machine, its run
    /// id, and how many of this machine's slots it was granted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dispatched_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dispatch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slots_hint: Option<u32>,
}

/// Where a job may run: here only (the default), on a worker when every slot here is busy,
/// or on a worker first.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemoteMode {
    #[default]
    Never,
    Allowed,
    Prefer,
}

/// A path to mirror to a worker: a path, or `{"path": …, "exclude": […]}`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged, expecting = "each sync entry must be a path or an object with path and exclude")]
pub(crate) enum SyncSpec {
    Path(String),
    Full(SyncPath),
}

/// One path mirrored to a worker, at the same absolute path, and rsync patterns left out.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SyncPath {
    pub(crate) path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) exclude: Vec<String>,
}

/// A command as an argv list, or a string run with `/bin/sh -c`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged, expecting = "command must be a string or a list of strings")]
pub(crate) enum CommandSpec {
    Shell(String),
    Argv(Vec<String>),
}

/// Who submitted a job: a tool name ("Claude Code", "a cron job"), the session or thread
/// it came from, and a link that opens it. All optional; the dashboard shows it as the
/// run's source.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
}

/// `"source": "Cursor"` or `"source": {"tool": …, "session": …, "url": …}`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged, expecting = "source must be a tool name or an object with tool, session and url")]
pub(crate) enum SourceSpec {
    Tool(String),
    Full(Source),
}

/// A job ready to queue: every field checked, every default applied.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct Job {
    pub(crate) label: String,
    pub(crate) category: String,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: PathBuf,
    /// The claimed job file; its name is the job's name.
    pub(crate) job_file: PathBuf,
    pub(crate) log: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) variant: Option<String>,
    #[serde(default)]
    pub(crate) priority: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<Source>,
    /// Seconds of running time allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) activity: Option<PatternConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fail_patterns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prepare: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group: Option<String>,
    #[serde(default)]
    pub(crate) remote: RemoteMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) requires: Vec<String>,
    /// Absolute paths; empty means the job's `cwd`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) sync: Vec<SyncPath>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) sync_git: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) env_forward: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dispatched_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dispatch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slots_hint: Option<u32>,
    /// The claimed job file's identity, so recovery can tell this job's file from that of a
    /// later job reusing the name. Set when the file is claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) claim: Option<FileId>,
}

impl Job {
    pub(crate) fn files(&self) -> JobFiles {
        JobFiles::of(&self.job_file).unwrap_or_else(|| JobFiles::new(Path::new("."), "job"))
    }
}

/// A display field: absent or empty means "use the default".
fn text_field(field: &str, value: Option<String>) -> Result<Option<String>> {
    match value.filter(|v| !v.is_empty()) {
        Some(v) => naming::validate_display(field, &v).map(|()| Some(v)),
        None => Ok(None),
    }
}

impl JobSpec {
    /// Parses a job file's text.
    pub(crate) fn parse(text: &str) -> Result<JobSpec> {
        serde_json::from_str(text).or_else(|e| match e.classify() {
            serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
                invalid(format!("not valid JSON: {e}"))
            }
            _ => invalid(format!("not a valid job: {e}")),
        })
    }

    /// Checks the spec and applies defaults. `files` are the claimed job's files; a relative
    /// `cwd` (and a missing one) resolves against `inbox`.
    pub(crate) fn resolve(self, files: &JobFiles, inbox: &Path) -> Result<Job> {
        let command = argv("command", self.command)?;
        let prepare = self.prepare.map(|p| argv("prepare", p)).transpose()?;
        let cwd = resolve_cwd(self.cwd.as_deref(), inbox)?;
        let timeout = match self.timeout {
            // A fraction rounds up, so 0.5 allows one second rather than none.
            Some(t) if t > 0.0 && t <= MAX_TIMEOUT_SECONDS as f64 => Some(t.ceil() as u64),
            Some(_) => {
                return invalid(format!("timeout must be a positive number of seconds, at most {MAX_TIMEOUT_SECONDS}"))
            }
            None => None,
        };
        if let Some((name, _)) = self.env.iter().find(|(k, _)| k.is_empty() || k.contains(['=', '\0'])) {
            return invalid(format!("env has an invalid variable name: {name:?}"));
        }
        if let Some(cfg) = &self.activity {
            Patterns::compile(cfg)?; // a bad regex is reported now, not when the job runs
        }
        compile_fail_patterns(&self.fail_patterns)?;
        Ok(Job {
            label: text_field("label", self.label)?.unwrap_or_else(|| files.name().to_string()),
            category: text_field("category", self.category)?.unwrap_or_else(|| naming::derive_category(&command)),
            source: resolve_source(self.source, self.session)?,
            variant: text_field("variant", self.variant)?,
            command,
            cwd,
            job_file: files.claimed(),
            log: files.log(),
            priority: self.priority,
            timeout,
            activity: self.activity,
            env: self.env,
            fail_patterns: self.fail_patterns,
            prepare,
            group: text_field("group", self.group)?,
            remote: self.remote.unwrap_or_default(),
            requires: self.requires,
            sync: resolve_sync(self.sync)?,
            sync_git: self.sync_git,
            env_forward: self.env_forward,
            dispatched_from: text_field("dispatched_from", self.dispatched_from)?,
            dispatch_id: text_field("dispatch_id", self.dispatch_id)?,
            slots_hint: self.slots_hint.filter(|n| *n > 0).map(|n| n.min(crate::settings::MAX_SLOTS)),
            claim: None,
        })
    }
}

/// Sync entries as absolute paths: `~/…` is under the home directory; anything else must be
/// absolute. They need not exist yet; a dispatch checks them.
fn resolve_sync(entries: Vec<SyncSpec>) -> Result<Vec<SyncPath>> {
    entries
        .into_iter()
        .map(|entry| {
            let mut sync = match entry {
                SyncSpec::Path(path) => SyncPath { path, exclude: Vec::new() },
                SyncSpec::Full(sync) => sync,
            };
            sync.path = match sync.path.strip_prefix("~/") {
                Some(rest) => paths::home().join(rest).to_string_lossy().into_owned(),
                None if sync.path.starts_with('/') => sync.path,
                None => return invalid(format!("sync paths must be absolute or start with ~/, not {:?}", sync.path)),
            };
            if sync.exclude.iter().any(|e| e.is_empty() || e.contains(['\n', '\0'])) {
                return invalid(format!("sync.exclude for {} has an empty or multi-line pattern", sync.path));
            }
            Ok(sync)
        })
        .collect()
}

/// A command as the argv it runs: a string runs with `/bin/sh -c`.
fn argv(field: &str, command: CommandSpec) -> Result<Vec<String>> {
    match command {
        CommandSpec::Shell(s) if !s.trim().is_empty() => Ok(vec!["/bin/sh".into(), "-c".into(), s]),
        CommandSpec::Argv(argv) if !argv.is_empty() && !argv[0].is_empty() => Ok(argv),
        _ => invalid(format!("{field} must not be empty")),
    }
}

/// The most `fail_patterns` a job may give.
const MAX_FAIL_PATTERNS: usize = 32;

/// Compiles a job's `fail_patterns`; a pattern that does not compile is an error naming it.
pub(crate) fn compile_fail_patterns(patterns: &[String]) -> Result<Vec<regex::Regex>> {
    if patterns.len() > MAX_FAIL_PATTERNS {
        return invalid(format!("fail_patterns may hold at most {MAX_FAIL_PATTERNS} patterns"));
    }
    patterns
        .iter()
        .enumerate()
        .map(|(i, p)| match p.as_str() {
            "" => invalid(format!("fail_patterns[{i}] is empty")),
            p => regex::Regex::new(p).or_else(|e| invalid(format!("fail_patterns[{i}] is not a valid regex: {e}"))),
        })
        .collect()
}

/// The working directory: absolute, `~/…`, or relative to the inbox; it must exist.
fn resolve_cwd(cwd: Option<&str>, inbox: &Path) -> Result<PathBuf> {
    let path = match cwd.filter(|c| !c.is_empty()) {
        None => inbox.to_path_buf(),
        Some(c) => match c.strip_prefix("~/") {
            Some(rest) => paths::home().join(rest),
            None => inbox.join(c), // joining an absolute path replaces the base
        },
    };
    if !path.is_dir() {
        return invalid(format!("cwd does not exist: {}", path.display()));
    }
    Ok(path.canonicalize().unwrap_or(path))
}

fn resolve_source(source: Option<SourceSpec>, session: Option<String>) -> Result<Option<Source>> {
    let mut src = match source {
        None => None,
        Some(SourceSpec::Tool(tool)) => Some(Source { tool: Some(tool), ..Source::default() }),
        Some(SourceSpec::Full(s)) => Some(s),
    };
    if let Some(session) = session.filter(|s| !s.is_empty()) {
        src.get_or_insert_with(Source::default).session.get_or_insert(session);
    }
    let Some(mut src) = src else { return Ok(None) };
    src.tool = text_field("source.tool", src.tool.take())?;
    src.session = text_field("source.session", src.session.take())?;
    src.url = src.url.filter(|u| !u.is_empty());
    if src.url.as_deref().is_some_and(|u| !safe_link(u)) {
        return invalid("source.url must be a URL (javascript:, data:, vbscript: and file: are not allowed)");
    }
    Ok((src != Source::default()).then_some(src))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(text: &str) -> Result<Job> {
        let inbox = std::env::temp_dir();
        JobSpec::parse(text)?.resolve(&JobFiles::new(&inbox, "build"), &inbox)
    }

    #[test]
    fn defaults_are_applied() {
        let job = resolve(r#"{"command": ["cargo", "test"]}"#).unwrap();
        assert_eq!((job.label.as_str(), job.category.as_str()), ("build", "cargo"));
        assert_eq!(job.cwd, std::env::temp_dir().canonicalize().unwrap());
        assert!(job.log.ends_with("build.log") && job.job_file.ends_with("build.json.claimed"));
        let shell = resolve(r#"{"command": "make", "timeout": 0.5, "label": "b", "priority": true}"#).unwrap();
        assert_eq!(shell.command, ["/bin/sh", "-c", "make"]);
        assert_eq!(shell.timeout, Some(1), "a fraction of a second rounds up, never to 0");
        assert!(shell.priority);
        let prep = resolve(r#"{"command": ["make"], "prepare": "make deps"}"#).unwrap();
        assert_eq!(prep.prepare.unwrap(), ["/bin/sh", "-c", "make deps"]);
        let rel = resolve(r#"{"command": ["true"], "cwd": "."}"#).unwrap();
        assert_eq!(rel.cwd, std::env::temp_dir().canonicalize().unwrap(), "a relative cwd is the inbox's");
    }

    #[test]
    fn sources_take_several_shapes() {
        let src = |text: &str| resolve(text).unwrap().source;
        assert_eq!(src(r#"{"command": "x", "source": "Cursor"}"#).unwrap().tool.as_deref(), Some("Cursor"));
        let both = src(r#"{"command": "x", "source": {"tool": "Codex"}, "session": "t1"}"#).unwrap();
        assert_eq!((both.tool.as_deref(), both.session.as_deref()), (Some("Codex"), Some("t1")));
        assert_eq!(src(r#"{"command": "x", "session": "s"}"#).unwrap().session.as_deref(), Some("s"));
        assert_eq!(src(r#"{"command": "x", "source": {}}"#), None);
    }

    #[test]
    fn bad_jobs_say_what_is_wrong() {
        for (job, message) in [
            ("[]", "not a valid job"),
            ("{", "not valid JSON"),
            ("{}", "missing field `command`"),
            (r#"{"command": 3}"#, "command must be a string or a list of strings"),
            (r#"{"command": ""}"#, "command must not be empty"),
            (r#"{"command": ["true"], "timout": 5}"#, "unknown field `timout`"),
            (r#"{"command": ["true"], "timeout": -1}"#, "timeout"),
            (r#"{"command": ["true"], "timeout": 1e300}"#, "at most"),
            (r#"{"command": ["true"], "priority": "yes"}"#, "invalid type"),
            (r#"{"command": ["true"], "env": {"A": 1}}"#, "invalid type"),
            (r#"{"command": ["true"], "env": {"A=B": "1"}}"#, "invalid variable name"),
            (r#"{"command": ["true"], "label": "a\nb"}"#, "line breaks"),
            (r#"{"command": ["true"], "source": {"url": "javascript:alert(1)"}}"#, "source.url"),
            (r#"{"command": ["true"], "source": {"url": "FILE:///etc/passwd"}}"#, "source.url"),
            (r#"{"command": ["true"], "source": 5}"#, "source must be"),
            (r#"{"command": ["true"], "cwd": "/no/such/dir"}"#, "cwd does not exist"),
            (r#"{"command": ["true"], "activity": {"step": "("}}"#, "activity.step"),
            (r#"{"command": ["true"], "fail_patterns": ["ok", "("]}"#, "fail_patterns[1] is not a valid regex"),
            (r#"{"command": ["true"], "fail_patterns": [""]}"#, "fail_patterns[0] is empty"),
            (r#"{"command": ["true"], "fail_patterns": "x"}"#, "invalid type"),
            (r#"{"command": ["true"], "prepare": []}"#, "prepare must not be empty"),
            (r#"{"command": ["true"], "prepare": 3}"#, "command must be a string"),
            (r#"{"command": ["true"], "group": "a\nb"}"#, "line breaks"),
            (r#"{"command": ["true"], "remote": "sometimes"}"#, "unknown variant"),
            (r#"{"command": ["true"], "sync": ["relative/path"]}"#, "sync paths must be absolute"),
            (r#"{"command": ["true"], "sync": [{"path": "/a", "exclud": []}]}"#, "each sync entry"),
        ] {
            let e = resolve(job).unwrap_err().to_string();
            assert!(e.contains(message), "{job}: {e}");
        }
    }

    #[test]
    fn submit_and_the_daemon_agree_on_the_format() {
        let spec = JobSpec {
            command: CommandSpec::Argv(vec!["make".into()]),
            cwd: Some("/tmp".into()),
            label: Some("x".into()),
            category: None,
            timeout: Some(60.0),
            priority: false,
            variant: None,
            env: BTreeMap::new(),
            source: Some(SourceSpec::Tool("Codex".into())),
            session: None,
            activity: None,
            fail_patterns: Vec::new(),
            prepare: None,
            group: None,
            remote: None,
            requires: Vec::new(),
            sync: Vec::new(),
            sync_git: false,
            env_forward: Vec::new(),
            dispatched_from: None,
            dispatch_id: None,
            slots_hint: None,
        };
        let text = serde_json::to_string(&spec).unwrap();
        assert_eq!(text, r#"{"command":["make"],"cwd":"/tmp","label":"x","timeout":60.0,"source":"Codex"}"#);
        assert_eq!(JobSpec::parse(&text).unwrap(), spec);
    }
}
