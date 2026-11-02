//! How many jobs may run at once: a standing count (`DRMANHATTAN_SLOTS`, else
//! `config.json`, else 1), which the dashboard's − and + buttons override for a few hours.

use std::time::Duration;

use serde::Serialize;

use crate::error::{invalid, Result};
use crate::fsutil;
use crate::paths::Paths;
use crate::settings::load_config;
use crate::time::Timestamp;

pub(crate) const MAX_SLOTS: u32 = 16;

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

/// A temporary capacity set from the dashboard, until a moment in time.
#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
pub(crate) struct Override {
    pub(crate) slots: u32,
    pub(crate) until: Timestamp,
}

/// The override, if one is set, well-formed and not yet expired. The file holds
/// `<slots> <until>`.
pub(crate) fn read_override(paths: &Paths) -> Option<Override> {
    read_override_file(&paths.capacity_override(), 1)
}

fn read_override_file(path: &std::path::Path, min: u32) -> Option<Override> {
    if !fsutil::is_own_file(path) {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let mut words = text.split_whitespace();
    let slots: u32 = words.next()?.parse().ok()?;
    let until: Timestamp = words.next()?.parse().ok()?;
    ((min..=MAX_SLOTS).contains(&slots) && until > Timestamp::now()).then_some(Override { slots, until })
}

/// A worker's override from the dashboard; 0 pauses it.
pub(crate) fn read_worker_override(paths: &Paths, worker: &str) -> Option<Override> {
    read_override_file(&paths.worker_override(worker), 0)
}

/// Sets a worker's override (0 to 16) for `lasting` from now, or removes it (`None`).
pub(crate) fn write_worker_override(
    paths: &Paths,
    worker: &str,
    slots: Option<u32>,
    lasting: Duration,
) -> Result<Option<Override>> {
    let path = paths.worker_override(worker);
    let Some(slots) = slots else {
        fsutil::remove_if_present(&path)?;
        return Ok(None);
    };
    if slots > MAX_SLOTS {
        return invalid(format!("slots must be from 0 to {MAX_SLOTS}, not {slots}"));
    }
    let o = Override { slots, until: Timestamp::now().plus(lasting) };
    fsutil::write_atomic(&path, &format!("{} {}\n", o.slots, o.until))?;
    Ok(Some(o))
}

/// How many of a worker's slots this machine may use now: its override, else its `slots`.
pub(crate) fn worker_capacity(paths: &Paths, worker: &crate::workers::WorkerConfig) -> u32 {
    read_worker_override(paths, &worker.name).map_or(worker.slots, |o| o.slots)
}

/// Sets the override for `lasting` from now, or removes it (`None`).
pub(crate) fn write_override(paths: &Paths, slots: Option<u32>, lasting: Duration) -> Result<Option<Override>> {
    let Some(slots) = slots else {
        fsutil::remove_if_present(&paths.capacity_override())?;
        return Ok(None);
    };
    let o = Override { slots: check(slots, "slots")?, until: Timestamp::now().plus(lasting) };
    fsutil::write_atomic(&paths.capacity_override(), &format!("{} {}\n", o.slots, o.until))?;
    Ok(Some(o))
}

/// What admission honours right now: the override while it lasts, else the standing capacity.
pub(crate) fn effective_capacity(paths: &Paths, slots_env: Option<&str>) -> Result<u32> {
    match read_override(paths) {
        Some(o) => Ok(o.slots),
        None => standing_capacity(paths, slots_env).map(|(slots, _)| slots),
    }
}
