//! Auto-update, when `config.json` says `auto_update: true`: a newer release of the same
//! major version is installed once the queue has been idle for a while (10 minutes, or
//! `DRMANHATTAN_AUTO_UPDATE_IDLE_SECONDS`). New work resets the wait; a major release only
//! shows the notice. The install is `drmanhattan update`, run as its own process so that it
//! outlives the daemon it restarts. A failure is logged and tried again after the next check.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::check::last_checked;
use super::release::{current, is_minor_update};
use super::{available, idle};
use crate::log;
use crate::paths::Paths;
use crate::settings::load_config;

const POLL: Duration = Duration::from_secs(1);

fn idle_wait() -> Duration {
    let seconds = std::env::var("DRMANHATTAN_AUTO_UPDATE_IDLE_SECONDS").ok().and_then(|s| s.parse().ok());
    Duration::from_secs_f64(seconds.unwrap_or(600.0))
}

pub(crate) fn start_auto_updater(paths: Paths) {
    let _ = std::thread::Builder::new().name("auto-update".into()).spawn(move || {
        let mut idle_since: Option<Instant> = None;
        let mut tried_after = None; // the check whose release was already tried
        loop {
            std::thread::sleep(POLL);
            let on = load_config(&paths).is_ok_and(|c| c.auto_update);
            let release = available(&paths).filter(|a| is_minor_update(&a.version, current()));
            let check = last_checked(&paths);
            if !on || release.is_none() || check == tried_after || !idle(&paths) {
                idle_since = None;
                continue;
            }
            if idle_since.get_or_insert_with(Instant::now).elapsed() < idle_wait() {
                continue;
            }
            tried_after = check;
            let version = release.map(|r| r.version).unwrap_or_default();
            log::notice(log::Tone::Quiet, "updating", format_args!("to {version}: the queue is idle"));
            if let Err(why) = install() {
                log::problem(format_args!("auto-update to {version} failed: {why}; trying again after the next check"));
            }
        }
    });
}

/// Runs `drmanhattan update`. It returns only if the update failed: on success it restarts
/// this daemon.
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
