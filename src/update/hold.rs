//! Holding new starts while an update waits for the running jobs to finish.
//!
//! A restart ends the jobs running on this machine (each one's guard sees to that), so an
//! update restarts the daemon only once none is running. On a busy machine that moment may
//! never come by itself, so `drmanhattan update` makes it: with the new version downloaded
//! and checked, it writes `update-hold` in the state directory with its own pid, and while
//! that process lives the daemon starts nothing new. Jobs submitted meanwhile are queued as
//! usual and keep their places through the restart; the queue waits at most as long as the
//! jobs already running take. Services and runs on workers don't hold the update up: services
//! start again with the new daemon, and workers' runs are followed again where they are.
//!
//! The new daemon removes the hold when it starts, a failed update removes it when it gives
//! up, and a hold whose process is gone (killed mid-wait) is ignored.

use std::path::PathBuf;

use crate::fsutil;
use crate::paths::Paths;
use crate::process;

fn file(paths: &Paths) -> PathBuf {
    paths.state.join("update-hold")
}

/// Held for as long as it lives; dropping it lets the queue start jobs again.
pub(crate) struct Hold {
    path: PathBuf,
}

impl Drop for Hold {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Asks the daemon to start nothing new until this process ends or drops the hold.
pub(crate) fn hold(paths: &Paths) -> crate::error::Result<Hold> {
    let path = file(paths);
    fsutil::write_atomic(&path, &format!("{}\n", std::process::id()))?;
    Ok(Hold { path })
}

/// Whether an update is holding new starts: its process is still there.
pub(crate) fn held(paths: &Paths) -> bool {
    let Ok(text) = std::fs::read_to_string(file(paths)) else { return false };
    text.trim().parse::<i32>().is_ok_and(process::process_alive)
}

/// What a starting daemon does: any hold was for the daemon before it.
pub(crate) fn clear(paths: &Paths) {
    let _ = std::fs::remove_file(file(paths));
}
