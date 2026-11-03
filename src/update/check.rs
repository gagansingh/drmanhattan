//! The daemon's look for a newer release, every 4 hours by the wall clock, kept in
//! `update-check.json` in the data directory for the dashboard and `status`. The wait is
//! short sleeps against `checked_at`, not one long sleep, whose monotonic clock stops while
//! a laptop sleeps: a machine that wakes after the interval checks within minutes.
//! `DRMANHATTAN_NO_UPDATE_CHECK=1` turns it off.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::release::{current, is_newer, latest_tag, releases_url};
use crate::fsutil;
use crate::paths::Paths;
use crate::time::Timestamp;

/// Between checks; `DRMANHATTAN_UPDATE_CHECK_SECONDS` for tests.
pub(super) fn check_every() -> Duration {
    super::seconds_env("DRMANHATTAN_UPDATE_CHECK_SECONDS", 4 * 3600)
}

/// How often the loop wakes to compare the clock; the first look is this long after start
/// too, at most a minute, so a restart never waits on the network.
fn tick() -> Duration {
    super::seconds_env("DRMANHATTAN_UPDATE_TICK_SECONDS", 300)
}

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

/// When the last check ran: the auto-updater tries a release once per check.
pub(super) fn last_checked(paths: &Paths) -> Option<Timestamp> {
    fsutil::read_json::<Check>(&paths.data.join("update-check.json")).map(|c| c.checked_at)
}

/// Starts the check on a thread of its own.
pub(crate) fn start_checker(paths: Paths) {
    if !checks_on() {
        return;
    }
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        let file = paths.data.join("update-check.json");
        let mut offline_until: Option<Timestamp> = None;
        std::thread::sleep(tick().min(Duration::from_secs(60)));
        loop {
            let last: Option<Check> = fsutil::read_json(&file);
            // A checked_at in the future means the clock moved back: check rather than wait.
            let due = last.map_or(true, |c| c.checked_at.elapsed() >= check_every() || c.checked_at > Timestamp::now());
            if due && offline_until.map_or(true, |t| Timestamp::now() >= t) {
                match latest_tag() {
                    Ok(latest) => {
                        offline_until = None;
                        let _ = fsutil::write_json(&file, &Check { checked_at: Timestamp::now(), latest });
                    }
                    // Offline: try again in an hour, or sooner when checks are that frequent.
                    Err(_) => offline_until = Some(Timestamp::now().plus(check_every().min(Duration::from_secs(3600)))),
                }
            }
            std::thread::sleep(tick());
        }
    });
}

pub(super) fn checks_on() -> bool {
    std::env::var("DRMANHATTAN_NO_UPDATE_CHECK").map_or(true, |v| v != "1")
}
