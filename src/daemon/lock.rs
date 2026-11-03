//! One daemon per state directory, and how other commands find it.
//!
//! The daemon holds an exclusive `flock(2)` on `daemon.lock` for as long as it runs. The
//! kernel releases it the moment the process exits, however it exits (`kill -9` included),
//! so a stale lock can never block a restart, and "is a daemon running?" is answered by the
//! lock, never by a pid in a file that may be stale. The lock file holds the holder's pid,
//! and `dashboard.json` its inbox and dashboard URL; both are trusted only while the lock
//! is held.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{invalid, IoContext, Result};
use crate::fsutil;
use crate::paths::Paths;
use crate::time::Timestamp;

/// How often, and how far apart, the lock and the pid are tried before giving up.
const LOCK_ATTEMPTS: u32 = 10;
const RETRY: std::time::Duration = std::time::Duration::from_millis(20);

/// Held for the life of the daemon.
pub(crate) struct DaemonLock {
    _file: File,
}

/// `dashboard.json`: what other commands need to know about the running daemon.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct DaemonInfo {
    pub(crate) pid: u32,
    pub(crate) inbox: PathBuf,
    pub(crate) started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
    /// The daemon's `DRMANHATTAN_SLOTS`, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slots_env: Option<String>,
    /// Where jobs' base environment came from: `zsh (/bin/zsh)`, `fallback: …` or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) shell_env: Option<String>,
}

/// Takes the lock, or returns None when another daemon holds it.
pub(crate) fn take(paths: &Paths) -> Result<Option<DaemonLock>> {
    let path = paths.daemon_lock();
    if fsutil::is_symlink(&path) {
        return invalid(format!("refusing a symlinked lock file: {}", path.display()));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false).mode(0o600);
    let mut file = options.open(&path).context("open the lock file", &path)?;
    // `running` probes with a brief shared lock, so a moment's refusal may be a probe rather
    // than a daemon: try a few times before concluding one is running.
    for attempt in 0..LOCK_ATTEMPTS {
        // SAFETY: the descriptor belongs to `file`, which outlives the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        if attempt + 1 == LOCK_ATTEMPTS {
            return Ok(None);
        }
        std::thread::sleep(RETRY);
    }
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    Ok(Some(DaemonLock { _file: file }))
}

/// The daemon holding the lock right now.
pub(crate) struct RunningDaemon {
    pub(crate) pid: u32,
    /// None until it has published `dashboard.json`.
    pub(crate) info: Option<DaemonInfo>,
}

pub(crate) fn running(paths: &Paths) -> Option<RunningDaemon> {
    let path = paths.daemon_lock();
    let file = File::open(&path).ok()?;
    // A shared lock succeeds only when nobody holds the exclusive one.
    // SAFETY: the descriptor belongs to `file`, which outlives both calls.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        // SAFETY: as above.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        return None;
    }
    // A daemon that has just taken the lock may not have written its pid yet.
    let read_pid = || std::fs::read_to_string(&path).ok()?.trim().parse::<u32>().ok();
    let pid = (0..LOCK_ATTEMPTS).find_map(|attempt| {
        if attempt > 0 {
            std::thread::sleep(RETRY);
        }
        read_pid()
    })?;
    let info = fsutil::read_json::<DaemonInfo>(&paths.daemon_info()).filter(|i| i.pid == pid);
    Some(RunningDaemon { pid, info })
}

/// The `DRMANHATTAN_SLOTS` that decides the capacity: the running daemon's, else (with no
/// daemon to ask) this process's own.
pub(crate) fn slots_env(running: Option<&RunningDaemon>) -> Option<String> {
    match running.and_then(|r| r.info.as_ref()) {
        Some(info) => info.slots_env.clone(),
        None => std::env::var("DRMANHATTAN_SLOTS").ok(),
    }
}
