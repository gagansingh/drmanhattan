//! Updating drmanhattan in place, and knowing when there is something to update to.
//!
//! `drmanhattan update [--now]` finds the latest release (`release`), waits until no job is
//! running or queued (`--now` does not), downloads this platform's archive, checks its
//! SHA-256, replaces both commands beside the running executable, and restarts the daemon
//! (`restart`). A failed download or checksum leaves the old version in place. The daemon
//! looks for a newer release once a day (`check`).

mod check;
mod release;
mod restart;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub(crate) use check::{available, start_checker, Available};
pub(crate) use release::current;
use release::{asset, download, is_newer, latest_tag, releases_url, sha256};

use crate::error::{invalid, Error, Result};
use crate::job::State;
use crate::paths::Paths;
use crate::queue::read_records;

/// `drmanhattan update`.
pub(crate) fn update(now: bool) -> Result<i32> {
    let paths = Paths::from_env();
    let tag = latest_tag()?;
    if !is_newer(&tag, current()) {
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
    swapped?;
    restart::restart(&paths, &dir)?;
    println!("updated {} → {}", current(), tag.trim_start_matches('v'));
    Ok(0)
}

fn stage_and_swap(paths: &Paths, tag: &str, stage: &Path, dir: &Path, now: bool) -> Result<()> {
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
    if !now {
        wait_until_idle(paths);
    } else if !idle(paths) {
        crate::log::warn(format_args!("updating now: running jobs end when the daemon restarts"));
    }
    for program in ["drmanhattan", "manhattan"] {
        let (from, to) = (new_dir.join(program), dir.join(program));
        if from.is_file() {
            std::fs::rename(&from, &to)
                .map_err(|e| Error::Invalid(format!("could not replace {}: {e}", to.display())))?;
        }
    }
    Ok(())
}

/// The directory both commands live in: the running executable's.
fn install_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe().and_then(|e| e.canonicalize()).map_err(|e| Error::Invalid(e.to_string()))?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| Error::Invalid("no install directory".into()))
}

/// Jobs that hold the queue: running or queued, services aside (they restart with it).
fn busy(paths: &Paths) -> Vec<String> {
    read_records(paths)
        .into_iter()
        .filter(|r| !r.job.service && matches!(r.state, State::Running | State::Queued))
        .map(|r| format!("{} ({})", r.job.label, r.state))
        .collect()
}

/// Nothing runs or waits, services aside.
pub(crate) fn idle(paths: &Paths) -> bool {
    busy(paths).is_empty()
}

fn wait_until_idle(paths: &Paths) {
    let mut said = String::new();
    loop {
        let jobs = busy(paths);
        if jobs.is_empty() {
            return;
        }
        let line = format!("waiting for {} to finish (or run with --now)", jobs.join(", "));
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
