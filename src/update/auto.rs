//! Auto-update, when `config.json` says `auto_update: true`: a newer release of the same
//! major version is installed once the queue has been idle for a while (5 minutes, or
//! `DRMANHATTAN_AUTO_UPDATE_IDLE_SECONDS`). New work resets the wait; a major release only
//! shows the notice. The install is `drmanhattan update`, run as its own process so that it
//! outlives the daemon it restarts. A failure is logged and tried again an hour later
//! (`DRMANHATTAN_AUTO_UPDATE_RETRY_SECONDS`). What it is doing is kept in
//! `auto-update.json`, for `summary`.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::check::{check_every, checks_on, last_checked};
use super::release::{current, is_minor_update};
use super::{available, idle, seconds_env};
use crate::fsutil;
use crate::log;
use crate::paths::Paths;
use crate::settings::load_config;
use crate::time::Timestamp;

const POLL: Duration = Duration::from_secs(1);

/// What the updater is doing about a release, written when it changes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum Phase {
    /// The release waits for the queue to empty.
    Busy {
        version: String,
    },
    /// The queue is idle; the install starts at `install_at` unless work arrives.
    Waiting {
        version: String,
        install_at: Timestamp,
    },
    Updating {
        version: String,
    },
    /// The last install failed; it is tried again at `retry_at`.
    Failed {
        version: String,
        reason: String,
        retry_at: Timestamp,
    },
}

fn file(paths: &Paths) -> std::path::PathBuf {
    paths.data.join("auto-update.json")
}

pub(crate) fn start_auto_updater(paths: Paths) {
    let _ = std::thread::Builder::new().name("auto-update".into()).spawn(move || {
        let idle_wait = seconds_env("DRMANHATTAN_AUTO_UPDATE_IDLE_SECONDS", 300);
        let retry = seconds_env("DRMANHATTAN_AUTO_UPDATE_RETRY_SECONDS", 3600);
        let mut phase: Option<Phase> = None;
        let mut idle_since: Option<Timestamp> = None;
        loop {
            std::thread::sleep(POLL);
            let on = load_config(&paths).is_ok_and(|c| c.auto_update);
            let release = available(&paths).filter(|a| is_minor_update(&a.version, current()));
            let next = match (on, release) {
                (true, Some(r)) => {
                    let failed = matches!(&phase, Some(Phase::Failed { version, retry_at, .. })
                        if *version == r.version && Timestamp::now() < *retry_at);
                    if failed {
                        phase.clone()
                    } else if !idle(&paths) {
                        idle_since = None;
                        Some(Phase::Busy { version: r.version })
                    } else {
                        let since = *idle_since.get_or_insert_with(Timestamp::now);
                        Some(Phase::Waiting { version: r.version, install_at: since.plus(idle_wait) })
                    }
                }
                _ => None,
            };
            if let Some(Phase::Waiting { version, install_at }) = &next {
                if Timestamp::now() >= *install_at {
                    let version = version.clone();
                    set(&paths, &mut phase, Some(Phase::Updating { version: version.clone() }));
                    log::notice(log::Tone::Quiet, "updating", format_args!("to {version}: the queue is idle"));
                    let Err(reason) = install() else { return }; // the daemon is restarting
                    log::problem(format_args!(
                        "auto-update to {version} failed: {reason}; trying again in {}",
                        crate::time::human(retry)
                    ));
                    idle_since = None;
                    let failed = Phase::Failed { version, reason, retry_at: Timestamp::now().plus(retry) };
                    set(&paths, &mut phase, Some(failed));
                    continue;
                }
            }
            set(&paths, &mut phase, next);
        }
    });
}

/// Keeps the phase, and its file, when it changes.
fn set(paths: &Paths, phase: &mut Option<Phase>, next: Option<Phase>) {
    if *phase == next {
        return;
    }
    match &next {
        Some(p) => drop(fsutil::write_json(&file(paths), p)),
        None => drop(std::fs::remove_file(file(paths))),
    }
    *phase = next;
}

/// One line on auto-update for `status` and the dashboard: `off`, `up to date (checked
/// 2:10 PM, next ~6:10 PM)`, `0.6.1 found, waiting for 4 min idle`, `0.6.1 found, queue
/// busy`, `updating`, or `last attempt failed: …`.
pub(crate) fn summary(paths: &Paths) -> String {
    if !load_config(paths).is_ok_and(|c| c.auto_update) {
        return "off".into();
    }
    if !checks_on() {
        return "on, but release checks are off".into();
    }
    match fsutil::read_json::<Phase>(&file(paths)) {
        Some(Phase::Busy { version }) => return format!("{version} found, queue busy"),
        Some(Phase::Waiting { version, install_at }) => {
            let minutes = Timestamp::now().until(install_at).as_secs().div_ceil(60).max(1);
            return format!("{version} found, waiting for {minutes} min idle");
        }
        Some(Phase::Updating { .. }) => return "updating".into(),
        Some(Phase::Failed { reason, .. }) => return format!("last attempt failed: {reason}"),
        None => {}
    }
    match last_checked(paths) {
        Some(at) => {
            let next = if at.elapsed() >= check_every() { Timestamp::now() } else { at.plus(check_every()) };
            format!("up to date (checked {}, next ~{})", at.local_clock(), next.local_clock())
        }
        None => "up to date (not checked yet)".into(),
    }
}

/// Runs `drmanhattan update`, which on success restarts this daemon.
fn install() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = Command::new(exe)
        .arg("update")
        .env("DRMANHATTAN_AUTO_UPDATE_RUN", "1")
        .stdin(Stdio::null())
        .process_group(0)
        .output()
        .map_err(|e| e.to_string())?;
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    match out.status.success() {
        true => Ok(()),
        false => {
            let last = said.lines().last().unwrap_or("it failed");
            Err(last.trim_start_matches("drmanhattan: error: ").to_string())
        }
    }
}
