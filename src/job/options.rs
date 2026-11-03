//! The job fields that say where and how a job runs beyond its command: on a worker
//! (`remote`, `sync`) and as a service (`ready`).

use serde::{Deserialize, Serialize};

use crate::error::{invalid, Result};
use crate::paths;

/// How a service shows it is ready: `{"port": 3000}` (something accepts connections on
/// 127.0.0.1:3000) or `{"log": "Listening on"}` (a line of its output matches).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Ready {
    Port(u16),
    Log(String),
}

/// How long a service may take to be ready, and to run, unless the job says.
pub(crate) const DEFAULT_READY_TIMEOUT: u64 = 120;
pub(crate) const DEFAULT_TTL: u64 = 8 * 3600;

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

/// Sync entries as absolute paths: `~/…` is under the home directory; anything else must be
/// absolute. They need not exist yet; a dispatch checks them.
pub(super) fn resolve_sync(entries: Vec<SyncSpec>) -> Result<Vec<SyncPath>> {
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
