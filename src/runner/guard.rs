//! A guard for each running job, so a job never outlives a daemon that dies.
//!
//! When the daemon dies abruptly (`kill -9`, a crash), nothing in it can stop the jobs it was
//! running: their process groups would run on until some later start noticed them. So beside
//! each job the daemon starts a small guard process, the same binary run as
//! `drmanhattan __guard <process group> <grace ms> <daemon pid>`, which watches its parent.
//! The moment the daemon is gone (the guard's parent pid changes, however the daemon died),
//! the guard ends the group: TERM, then KILL after the grace period.
//!
//! The guard lives exactly as long as its job. When the daemon has seen the group end, it
//! writes `done` on the guard's stdin and the guard exits; if it does not exit promptly, or
//! the daemon's handle on it is dropped some other way, the daemon kills it. So a guard only
//! ever signals a group that was the job's a moment before, never a stranger that has since
//! been given the same id.
//!
//! Death is detected from the parent pid rather than from end of input on the pipe, because
//! the pipe's writing end can leak into other processes the daemon starts (on macOS the
//! standard library marks it close-on-exec only after creating it), and a leaked end would
//! keep the pipe open. The guard ignores Ctrl-C, hang-up and a plain `kill`, which belong to
//! the daemon's own graceful shutdown.

use std::io::{BufRead, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::log;
use crate::process;

pub(crate) const SUBCOMMAND: &str = "__guard";
const STAND_DOWN: &str = "done";
/// How often the guard checks that the daemon is still its parent.
const CHECK_EVERY: Duration = Duration::from_millis(200);
/// How long the daemon waits for a guard to exit after `done` before killing it.
const EXIT_WAIT: Duration = Duration::from_secs(2);

/// The daemon's end of one guard. Dropped without `stand_down`, it kills the guard.
pub(super) struct GroupGuard {
    child: Child,
    stdin: Option<ChildStdin>,
}

/// The binary to run the guard from. On Linux, `/proc/self/exe` still runs this binary
/// after it has been replaced or deleted on disk (an upgrade); elsewhere, its path.
fn this_binary() -> std::io::Result<std::path::PathBuf> {
    if cfg!(target_os = "linux") {
        Ok("/proc/self/exe".into())
    } else {
        std::env::current_exe()
    }
}

impl GroupGuard {
    /// Starts a guard for `pgid`. None (with a warning) if it cannot be started: the job
    /// still runs, and if the daemon then dies, only its next start cleans up.
    pub(super) fn start(pgid: i32, grace: Duration) -> Option<GroupGuard> {
        let started = this_binary().and_then(|exe| {
            Command::new(exe)
                .arg0("drmanhattan")
                .args([SUBCOMMAND, &pgid.to_string(), &grace.as_millis().to_string(), &std::process::id().to_string()])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .process_group(0)
                .spawn()
        });
        match started {
            Ok(mut child) => Some(GroupGuard { stdin: child.stdin.take(), child }),
            Err(e) => {
                log::problem(format_args!(
                    "could not start a guard for process group {pgid} ({e}); if the daemon dies, this job runs on until the next start"
                ));
                None
            }
        }
    }

    /// Tells the guard its group has ended and waits, briefly, for it to exit.
    pub(super) fn stand_down(mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = writeln!(stdin, "{STAND_DOWN}");
        }
        let deadline = Instant::now() + EXIT_WAIT;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_)) | Err(_)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Drop kills it.
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The guard process: `drmanhattan __guard <process group> <grace ms> <daemon pid>`.
pub(crate) fn run(args: &[String]) -> i32 {
    let number = |i: usize| args.get(i).and_then(|a| a.parse::<i64>().ok());
    let (Some(pgid), Some(grace), Some(daemon)) = (number(0), number(1), number(2)) else {
        log::error("usage: drmanhattan __guard <process group> <grace ms> <daemon pid> (started by the daemon)");
        return 2;
    };
    let (Ok(pgid), Ok(grace), Ok(daemon)) = (i32::try_from(pgid), u64::try_from(grace), i32::try_from(daemon)) else {
        return 2;
    };
    if pgid <= 1 {
        return 2;
    }
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGPIPE] {
        // SAFETY: setting a signal's disposition to "ignore" has no preconditions.
        unsafe { libc::signal(signal, libc::SIG_IGN) };
    }
    // The pipe is read on its own thread, one line at a time, so a leaked writing end can
    // never keep the guard waiting.
    let (done_tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        while std::io::stdin().lock().read_line(&mut line).is_ok_and(|n| n > 0) {
            if line.trim() == STAND_DOWN {
                let _ = done_tx.send(());
                return;
            }
            line.clear();
        }
    });
    loop {
        match done.recv_timeout(CHECK_EVERY) {
            Ok(()) => return 0,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // The pipe closed without `done`: keep watching the daemon at the same pace.
            Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(CHECK_EVERY),
        }
        // SAFETY: getppid has no preconditions and cannot fail.
        if unsafe { libc::getppid() } != daemon {
            break;
        }
    }
    // The daemon's stderr may be gone with it; its log file is not.
    log::log_to_file(&crate::paths::Paths::from_env().daemon_log());
    if process::group_alive(pgid) {
        log::notice(log::Tone::Warning, "guard", format_args!("the daemon stopped; ending process group {pgid}"));
        process::terminate_group(pgid, Duration::from_millis(grace));
    }
    0
}
