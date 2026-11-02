//! The daemon's daily look for a newer release, kept in `update-check.json` in the data
//! directory for the dashboard and `status`. `DRMANHATTAN_NO_UPDATE_CHECK=1` turns it off.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::release::{current, is_newer, latest_tag, releases_url};
use crate::fsutil;
use crate::paths::Paths;
use crate::time::Timestamp;

const CHECK_EVERY: Duration = Duration::from_secs(86_400);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Check {
    checked_at: Timestamp,
    latest: String,
}

/// A newer release than this one.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub(crate) struct Available {
    pub(crate) version: String,
    pub(crate) url: String,
}

/// The newer release the last check found, if any.
pub(crate) fn available(paths: &Paths) -> Option<Available> {
    let check: Check = fsutil::read_json(&paths.data.join("update-check.json"))?;
    is_newer(&check.latest, current()).then(|| Available {
        version: check.latest.trim_start_matches('v').to_string(),
        url: format!("{}/tag/{}", releases_url(), check.latest),
    })
}

/// Starts the check on a thread of its own: at most once a day, never in start-up's way.
pub(crate) fn start_checker(paths: Paths) {
    if std::env::var("DRMANHATTAN_NO_UPDATE_CHECK").is_ok_and(|v| v == "1") {
        return;
    }
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || loop {
        let file = paths.data.join("update-check.json");
        let last: Option<Check> = fsutil::read_json(&file);
        let due = last.as_ref().map_or(Duration::ZERO, |c| CHECK_EVERY.saturating_sub(c.checked_at.elapsed()));
        std::thread::sleep(due);
        match latest_tag() {
            Ok(latest) => {
                let _ = fsutil::write_json(&file, &Check { checked_at: Timestamp::now(), latest });
            }
            Err(_) => std::thread::sleep(Duration::from_secs(3600)), // offline: try again later
        }
    });
}
