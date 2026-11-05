//! A worker as `config.json` lists it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{invalid, Result};
use crate::settings::MAX_SLOTS;

/// Free space the worker must keep on the disk holding its home directory, in GB.
const DEFAULT_MIN_FREE_GB: f64 = 20.0;
/// Mirrors a worker has not used for this many days are removed.
const DEFAULT_MIRROR_DAYS: u64 = 7;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkerConfig {
    /// Its name on the dashboard and in run records; the ssh alias when not given.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) name: String,
    /// An alias from `~/.ssh/config`, or `user@host`.
    pub(crate) ssh: String,
    /// How many of its slots this machine may use; 0 pauses it.
    #[serde(default = "one")]
    pub(crate) slots: u32,
    /// What it offers, which jobs' `requires` must be among.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) tags: Vec<String>,
    /// A command run on it before each dispatch: exit 0 means ready, else its first line of
    /// output says why not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) check: Option<String>,
    /// Argument prefixes rewritten for it: paths that exist only here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) args_path_map: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) min_free_gb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mirror_days: Option<u64>,
    /// The `drmanhattan` command on it, when it is not on the non-interactive `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) drmanhattan: Option<String>,
}

fn one() -> u32 {
    1
}

impl WorkerConfig {
    pub(crate) fn min_free_gb(&self) -> f64 {
        self.min_free_gb.unwrap_or(DEFAULT_MIN_FREE_GB)
    }

    pub(crate) fn mirror_days(&self) -> u64 {
        self.mirror_days.unwrap_or(DEFAULT_MIRROR_DAYS)
    }

    pub(crate) fn program(&self) -> &str {
        self.drmanhattan.as_deref().unwrap_or("drmanhattan")
    }

    /// `args` with this worker's `args_path_map` applied: an argument that starts with a
    /// mapped prefix (as the whole argument, or followed by `/`), or a `--flag=` value that
    /// does, gets the worker's prefix instead.
    pub(crate) fn map_args(&self, args: &[String]) -> Vec<String> {
        let map_one = |arg: &str| -> Option<String> {
            self.args_path_map.iter().find_map(|(from, to)| {
                let rest = arg.strip_prefix(from.trim_end_matches('/'))?;
                (rest.is_empty() || rest.starts_with('/')).then(|| format!("{}{rest}", to.trim_end_matches('/')))
            })
        };
        args.iter()
            .map(|arg| {
                if let Some(mapped) = map_one(arg) {
                    return mapped;
                }
                if let Some((flag, value)) = arg.split_once('=').filter(|(f, _)| f.starts_with('-')) {
                    if let Some(mapped) = map_one(value) {
                        return format!("{flag}={mapped}");
                    }
                }
                arg.clone()
            })
            .collect()
    }
}

/// A worker name: letters, digits, `.`, `_` and `-`, so it fits in file names and URLs.
pub(crate) fn is_worker_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Checks the workers and fills in default names.
pub(crate) fn validate(workers: &mut [WorkerConfig]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for w in workers.iter_mut() {
        if w.ssh.trim().is_empty() || w.ssh.starts_with('-') || w.ssh.contains(char::is_whitespace) {
            return invalid(format!("a worker's ssh must be an ssh alias or user@host, not {:?}", w.ssh));
        }
        if w.name.is_empty() {
            w.name = w.ssh.rsplit('@').next().unwrap_or(&w.ssh).to_string();
        }
        if !is_worker_name(&w.name) {
            return invalid(format!("worker name {:?} may hold only letters, digits, '.', '_' and '-'", w.name));
        }
        if w.name == "local" {
            return invalid("\"local\" names this machine; give the worker another name");
        }
        if !seen.insert(w.name.clone()) {
            return invalid(format!("two workers are named {}", w.name));
        }
        if w.slots > MAX_SLOTS {
            return invalid(format!("worker {}: slots must be from 0 to {MAX_SLOTS}", w.name));
        }
    }
    Ok(())
}

/// Checks that the route names configured workers, each once.
pub(crate) fn validate_route(route: &[String], workers: &[WorkerConfig]) -> Result<()> {
    for (i, name) in route.iter().enumerate() {
        if !workers.iter().any(|w| &w.name == name) {
            return invalid(format!("the route names {name:?}, which is not a worker"));
        }
        if route[..i].contains(name) {
            return invalid(format!("the route names {name} twice"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(text: &str) -> WorkerConfig {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn names_default_to_the_alias_and_must_be_unique() {
        let mut ws = vec![worker(r#"{"ssh": "me@air"}"#), worker(r#"{"ssh": "box", "slots": 0}"#)];
        validate(&mut ws).unwrap();
        assert_eq!((ws[0].name.as_str(), ws[0].slots, ws[1].slots), ("air", 1, 0));
        let mut twice = vec![worker(r#"{"ssh": "a"}"#), worker(r#"{"ssh": "b", "name": "a"}"#)];
        assert!(validate(&mut twice).unwrap_err().to_string().contains("two workers are named a"));
        assert!(validate(&mut [worker(r#"{"ssh": "-oProxyCommand=x"}"#)]).is_err());
        assert!(validate(&mut [worker(r#"{"ssh": "a", "name": "a b"}"#)]).is_err());
        assert!(serde_json::from_str::<WorkerConfig>(r#"{"ssh": "a", "slot": 1}"#).is_err());
    }

    #[test]
    fn a_route_names_configured_workers() {
        let mut ws = vec![worker(r#"{"ssh": "me@air"}"#)];
        validate(&mut ws).unwrap();
        assert!(validate_route(&["air".into()], &ws).is_ok());
        assert!(validate_route(&["box".into()], &ws).unwrap_err().to_string().contains("not a worker"));
        assert!(validate_route(&["air".into(), "air".into()], &ws).is_err());
    }

    #[test]
    fn mapped_paths_change_whole_components_only() {
        let w = worker(r#"{"ssh": "a", "args_path_map": {"/Volumes/Disk/derived": "~/Library/Caches/derived"}}"#);
        let args: Vec<String> =
            ["/Volumes/Disk/derived/x", "-derivedDataPath=/Volumes/Disk/derived", "/Volumes/Disk/derivedX", "build"]
                .map(String::from)
                .to_vec();
        assert_eq!(
            w.map_args(&args),
            [
                "~/Library/Caches/derived/x",
                "-derivedDataPath=~/Library/Caches/derived",
                "/Volumes/Disk/derivedX",
                "build"
            ]
        );
    }
}
