//! How many jobs may run at once: `DRMANHATTAN_SLOTS`, else `config.json`'s `slots`, else 1.
//! `drmanhattan slots N` and the dashboard's − and + set `config.json`'s, which stays until it
//! is changed again.

use crate::error::{invalid, Result};
use crate::fsutil;
use crate::paths::Paths;
use crate::settings::{load_config, update_config};

/// Only a guard against a typo: a machine may run as many jobs at once as its owner likes.
pub(crate) const MAX_SLOTS: u32 = 256;

fn check(slots: u32, source: &str) -> Result<u32> {
    if (1..=MAX_SLOTS).contains(&slots) {
        Ok(slots)
    } else {
        invalid(format!("{source} must be from 1 to {MAX_SLOTS}, not {slots}"))
    }
}

/// The standing capacity, and where it came from ("DRMANHATTAN_SLOTS", "config" or "default").
/// `slots_env` is the daemon's `DRMANHATTAN_SLOTS` (see `Settings::slots_env`), which wins
/// over `config.json`.
pub(crate) fn standing_capacity(paths: &Paths, slots_env: Option<&str>) -> Result<(u32, &'static str)> {
    if let Some(raw) = slots_env {
        let slots =
            raw.trim().parse().or_else(|_| invalid(format!("DRMANHATTAN_SLOTS must be a number, not {raw:?}")))?;
        return Ok((check(slots, "DRMANHATTAN_SLOTS")?, "DRMANHATTAN_SLOTS"));
    }
    match load_config(paths)?.slots {
        Some(slots) => Ok((check(slots, "\"slots\" in config.json")?, "config")),
        None => Ok((1, "default")),
    }
}

/// Sets the standing count in `config.json`, as `drmanhattan slots N` and the dashboard's −
/// and + do. It stays until it is changed again; `DRMANHATTAN_SLOTS`, when the daemon has
/// it, still wins.
pub(crate) fn set_slots(paths: &Paths, slots: u32) -> Result<u32> {
    let slots = check(slots, "slots")?;
    update_config(paths, |config| {
        config.slots = Some(slots);
        Ok(slots)
    })
}

/// Sets how many of a worker's slots this machine may use, in `config.json`; 0 pauses it.
pub(crate) fn set_worker_slots(paths: &Paths, worker: &str, slots: u32) -> Result<u32> {
    if slots > MAX_SLOTS {
        return invalid(format!("slots must be from 0 to {MAX_SLOTS}, not {slots}"));
    }
    update_config(paths, |config| {
        let Some(w) = config.workers.iter_mut().find(|w| w.name == worker) else {
            return invalid(format!("no worker is named {worker:?}"));
        };
        w.slots = slots;
        Ok(slots)
    })
}

/// Pauses or resumes this machine as a worker for other machines, in `config.json`. Its own
/// jobs, and jobs it is already running for them, are not affected.
pub(crate) fn set_paused(paths: &Paths, paused: bool) -> Result<bool> {
    update_config(paths, |config| {
        config.paused = paused;
        Ok(paused)
    })
}

/// The capacity admission honours: the standing count.
pub(crate) fn effective_capacity(paths: &Paths, slots_env: Option<&str>) -> Result<u32> {
    standing_capacity(paths, slots_env).map(|(slots, _)| slots)
}

/// Removes the timed capacity the dashboard wrote before 0.7.1, which no longer applies.
pub(crate) fn remove_timed_overrides(paths: &Paths) {
    let Ok(entries) = std::fs::read_dir(&paths.state) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with("capacity-override") {
            let _ = fsutil::remove_if_present(&entry.path());
        }
    }
}
