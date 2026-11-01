//! `drmanhattan stop`.

use std::time::{Duration, Instant};

use crate::daemon::lock;
use crate::error::{invalid, Result};
use crate::paths::Paths;
use crate::process;
use crate::settings::Settings;

/// Asks the running daemon to shut down (SIGTERM, the graceful path) and waits for it to
/// go. Only a daemon that holds the lock is signalled, so a stale pid in a leftover file
/// can never reach an unrelated process.
pub(crate) fn stop(paths: &Paths, settings: &Settings) -> Result<()> {
    let Some(daemon) = lock::running(paths) else {
        println!("drmanhattan is not running");
        return Ok(());
    };
    let pid = daemon.pid;
    if !process::signal_process(pid as i32, libc::SIGTERM) {
        return invalid(format!("could not signal drmanhattan (pid {pid})"));
    }
    // It has gone once its lock is free: the kernel releases it as the process exits.
    let deadline = Instant::now() + settings.shutdown_grace + settings.term_grace + Duration::from_secs(10);
    while Instant::now() < deadline {
        if lock::running(paths).is_none() {
            println!("drmanhattan (pid {pid}) stopped");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    invalid(format!(
        "drmanhattan (pid {pid}) has not stopped yet; running jobs get {} s to finish, then are cancelled. Check again with `drmanhattan status`.",
        settings.shutdown_grace.as_secs()
    ))
}
