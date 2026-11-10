//! Updating drmanhattan in place, and knowing when there is something to update to.
//!
//! `drmanhattan update [--now]` finds the latest release (`release`), downloads this
//! platform's archive, checks its SHA-256, and with `sign` in `config.json` signs both
//! commands (`sign`). Then it holds new starts until the jobs running on this machine have
//! finished (`hold`; `--now` does not wait for them), replaces both commands beside the
//! running executable, and restarts the daemon (`restart`). Queued jobs keep their places. A
//! failed download, checksum or signature leaves the old version in place. The daemon looks
//! for a newer release every 4 hours (`check`) and may install it itself (`auto`).

mod auto;
mod check;
mod hold;
mod installed;
mod release;
mod restart;
mod sign;
mod updated;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub(crate) use auto::{start_auto_updater, summary as auto_update_summary};
pub(crate) use check::{available, start_checker, Available};
pub(crate) use hold::{clear as clear_hold, held};
pub(crate) use installed::{daemon_behind, installed_version};
pub(crate) use release::current;
use release::{asset, download, is_newer, latest_tag, releases_url, sha256};
pub(crate) use sign::SignConfig;
pub(crate) use updated::{announce, updated, Updated};

use crate::error::{invalid, Error, Result};
use crate::job::State;
use crate::paths::Paths;
use crate::queue::read_records;

/// The commands a release holds.
const PROGRAMS: [&str; 2] = ["drmanhattan", "manhattan"];

/// A whole number of seconds from the environment, for tests to shorten a wait.
fn seconds_env(name: &str, default: u64) -> Duration {
    Duration::from_secs_f64(std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default as f64))
}

/// `drmanhattan update`.
pub(crate) fn update(now: bool) -> Result<i32> {
    let paths = Paths::from_env();
    let tag = latest_tag()?;
    if !is_newer(&tag, current()) {
        // Installed already, by an update whose restart has not happened: restart now.
        if let Some(running) = daemon_behind(crate::daemon::running(&paths).as_ref()) {
            let _hold = if now { None } else { Some(drain(&paths)?) };
            restart::restart(&paths, &install_dir()?)?;
            println!("restarted the daemon: {running} → {}", current());
            return Ok(0);
        }
        println!("drmanhattan {} is already up to date", current());
        return Ok(0);
    }
    let dir = install_dir()?;
    // Staged beside the old files, so the swap is a rename on one file system.
    let stage = dir.join(format!(".drmanhattan-update-{}", std::process::id()));
    if std::fs::create_dir(&stage).is_err() {
        return invalid(format!(
            "cannot write to {}, where drmanhattan is installed; install the new version with:\n  curl -fsSL {}/latest/download/install.sh | sh -s -- <a directory you can write to>",
            dir.display(),
            releases_url()
        ));
    }
    let swapped = stage_and_swap(&paths, &tag, &stage, &dir, now);
    let _ = std::fs::remove_dir_all(&stage);
    // Held through the restart: the new daemon lifts it.
    let _hold = swapped?;
    let to = tag.trim_start_matches('v');
    updated::record(&paths, current(), to);
    restart::restart(&paths, &dir)?;
    println!("updated {} → {to}", current());
    Ok(0)
}

/// Downloads, checks and signs the release, then (unless `now`) holds new starts until the
/// running jobs are done, and swaps the commands in. Returns the hold, for the restart.
fn stage_and_swap(paths: &Paths, tag: &str, stage: &Path, dir: &Path, now: bool) -> Result<Option<hold::Hold>> {
    let name = asset(tag)?;
    let base = format!("{}/download/{tag}", releases_url());
    let archive = stage.join(&name);
    download(&format!("{base}/{name}"), &archive)?;
    download(&format!("{base}/{name}.sha256"), &stage.join("sum"))?;
    let want = std::fs::read_to_string(stage.join("sum")).unwrap_or_default();
    let want = want.split_whitespace().next().unwrap_or_default().to_lowercase();
    if want.is_empty() || sha256(&archive).as_deref() != Some(want.as_str()) {
        return invalid(format!("the checksum of {name} does not match; the installed version is unchanged"));
    }
    let unpacked = Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(stage).status();
    let new_dir = stage.join("drmanhattan");
    if !unpacked.is_ok_and(|s| s.success()) || !new_dir.join("drmanhattan").is_file() {
        return invalid(format!("{name} is not a drmanhattan archive; the installed version is unchanged"));
    }
    if let Some(config) = crate::settings::load_config(paths)?.sign {
        let programs: Vec<&str> = PROGRAMS.into_iter().filter(|p| new_dir.join(p).is_file()).collect();
        sign::sign(&config, &new_dir, &programs)?;
    }
    let held = if now {
        if !idle(paths) {
            crate::log::warn(format_args!("updating now: running jobs end when the daemon restarts"));
        }
        None
    } else {
        Some(drain(paths)?)
    };
    for program in PROGRAMS {
        let (from, to) = (new_dir.join(program), dir.join(program));
        if from.is_file() {
            std::fs::rename(&from, &to)
                .map_err(|e| Error::Invalid(format!("could not replace {}: {e}", to.display())))?;
        }
    }
    Ok(held)
}

/// The directory both commands live in: the running executable's.
fn install_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().and_then(|e| e.canonicalize()).map_err(|e| Error::Invalid(e.to_string()))?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| Error::Invalid("no install directory".into()))
}

/// The jobs a restart would end: those running on this machine. Services start again with
/// the new daemon, and runs on workers carry on there; queued jobs keep their places.
fn running_here(paths: &Paths) -> Vec<String> {
    read_records(paths)
        .into_iter()
        .filter(|r| r.state == State::Running && !r.job.service && r.machine.is_none())
        .map(|r| r.job.label)
        .collect()
}

/// Nothing a restart would end is running.
pub(crate) fn idle(paths: &Paths) -> bool {
    running_here(paths).is_empty()
}

/// Holds new starts, and waits until the jobs running on this machine have finished.
fn drain(paths: &Paths) -> Result<hold::Hold> {
    let held = hold::hold(paths)?;
    let mut said = String::new();
    loop {
        let jobs = running_here(paths);
        if jobs.is_empty() {
            return Ok(held);
        }
        let line =
            format!("waiting for {} to finish; new jobs wait until the restart (or run with --now)", jobs.join(", "));
        if line != said {
            println!("{line}");
            said = line;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Starts `drmanhattan update` on its own, for the dashboard's button: it outlives the
/// daemon it restarts.
pub(crate) fn start_detached() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| Error::Invalid(e.to_string()))?;
    Command::new(exe)
        .arg("update")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(drop)
        .map_err(|e| Error::Invalid(format!("could not start the update: {e}")))
}
