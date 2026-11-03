//! What the last update did, kept in `updated.json` in the data directory: the new daemon
//! says so in its log, and the dashboard shows a note about it.

use serde::{Deserialize, Serialize};

use super::release::{current, releases_url};
use crate::fsutil;
use crate::log;
use crate::paths::Paths;
use crate::time::Timestamp;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct Updated {
    pub(crate) from: String,
    pub(crate) to: String,
    /// Installed by the auto-updater rather than by `drmanhattan update`.
    pub(crate) auto: bool,
    pub(crate) at: Timestamp,
    #[serde(default)]
    pub(crate) url: String,
}

fn file(paths: &Paths) -> std::path::PathBuf {
    paths.data.join("updated.json")
}

/// Records an update about to restart the daemon.
pub(super) fn record(paths: &Paths, from: &str, to: &str) {
    let auto = std::env::var("DRMANHATTAN_AUTO_UPDATE_RUN").is_ok_and(|v| v == "1");
    let url = format!("{}/tag/v{to}", releases_url());
    let note = Updated { from: from.into(), to: to.into(), auto, at: Timestamp::now(), url };
    let _ = fsutil::write_json(&file(paths), &note);
}

/// The update that installed this version, for the dashboard's note: for a week.
pub(crate) fn updated(paths: &Paths) -> Option<Updated> {
    let note: Updated = fsutil::read_json(&file(paths))?;
    (note.to == current() && note.at.elapsed().as_secs() < 7 * 86_400).then_some(note)
}

/// At start-up: says in the daemon's log that it was updated, once.
pub(crate) fn announce(paths: &Paths) {
    let Some(note) = updated(paths).filter(|n| n.at.elapsed().as_secs() < 600) else { return };
    let how = if note.auto { "auto-updated" } else { "updated" };
    log::notice(log::Tone::Good, how, format_args!("{} → {}", note.from, note.to));
}
