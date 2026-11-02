//! Watching a running command: its exit, its timeout, cancel requests, the load, and what
//! its log says it is doing.
//!
//! Deadlines are measured on the monotonic clock, so a clock change or a laptop asleep
//! never counts as running time: a job's timeout is time it actually ran.

use std::os::unix::process::ExitStatusExt;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::activity::{self, Activity, Patterns, Typicals};
use crate::job::{Record, State};
use crate::paths::Paths;
use crate::process;
use crate::queue::RunEvent;
use crate::settings::Settings;
use crate::time::{human, Timestamp};

/// How often the log is read for the dashboard's activity line.
const ACTIVITY_EVERY: Duration = Duration::from_millis(500);

/// What a finished command amounted to.
pub(super) struct Outcome {
    pub(super) state: State,
    pub(super) exit: i32,
    pub(super) duration: Duration,
    pub(super) load_samples: Vec<f64>,
    /// Set when something other than the exit status failed the run.
    pub(super) failure_reason: Option<String>,
    /// The line to read first, when it is known without searching the log.
    pub(super) error_line: Option<String>,
    /// The `::note` badges its log printed.
    pub(super) notes: Vec<String>,
}

/// Everything `supervise` reads.
pub(super) struct Watch<'a> {
    pub(super) rec: &'a Record,
    pub(super) events: &'a Receiver<RunEvent>,
    /// Where the dashboard reads what the log says; the runner publishes a copy there after
    /// each read, so the lock is never held across reading the log.
    pub(super) activity: &'a Mutex<Activity>,
    pub(super) patterns: &'a Patterns,
    pub(super) typicals: &'a Mutex<Typicals>,
    pub(super) paths: &'a Paths,
    pub(super) settings: &'a Settings,
}

/// Why the command is being stopped, once it is.
#[derive(Clone, Copy, PartialEq)]
enum Stopping {
    No,
    Cancelled,
    TimedOut,
}

/// Waits for the command to exit, stopping it on a cancel or at its timeout (TERM, then
/// KILL after the grace period), sampling the load every heartbeat and following its log.
/// Returns once the whole process group is gone: descendants that outlive the leader still
/// hold what the slot stands for (CPU, ports, locks).
pub(super) fn supervise(w: &Watch, pgid: i32, say: &dyn Fn(&str)) -> Outcome {
    let (rec, settings) = (w.rec, w.settings);
    let started = Instant::now();
    let timeout_at = rec.job.timeout.and_then(|t| started.checked_add(Duration::from_secs(t)));
    let mut samples: Vec<f64> = rec.load_at_start.into_iter().collect();
    let mut next_sample = started + settings.heartbeat;
    let mut next_scan = started;
    let mut stopping = Stopping::No;
    let mut kill_at: Option<Instant> = None;
    let mut killed = false;
    let mut activity = Activity::default();
    let status = loop {
        let now = Instant::now();
        if now >= next_scan {
            next_scan = now + ACTIVITY_EVERY;
            crate::joblog::keep_bounded(&rec.job.log, settings.max_log_bytes);
            follow_log(w, &mut activity);
        }
        if now >= next_sample {
            next_sample = now + settings.heartbeat;
            samples.extend(process::load_now());
        }
        if stopping == Stopping::No && timeout_at.is_some_and(|t| now >= t) {
            stopping = Stopping::TimedOut;
            say(&format!(
                "timed out after {}; stopping it",
                human(Duration::from_secs(rec.job.timeout.unwrap_or_default()))
            ));
        }
        if stopping != Stopping::No && kill_at.is_none() {
            process::signal_group(pgid, libc::SIGTERM);
            kill_at = Some(now + settings.term_grace);
        }
        if !killed && kill_at.is_some_and(|k| now >= k) {
            process::signal_group(pgid, libc::SIGKILL);
            killed = true;
        }
        let pending_kill = kill_at.filter(|_| !killed);
        let wake = [Some(next_scan), Some(next_sample), timeout_at.filter(|_| stopping == Stopping::No), pending_kill];
        let next = wake.into_iter().flatten().min().unwrap_or(now + ACTIVITY_EVERY);
        match w.events.recv_timeout(next.saturating_duration_since(Instant::now())) {
            Ok(RunEvent::Exited(status)) => break status,
            Ok(RunEvent::Cancel) if stopping == Stopping::No => {
                stopping = Stopping::Cancelled;
                say("cancelled; stopping it");
            }
            Ok(RunEvent::Cancel) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break None,
        }
    };
    process::terminate_group(pgid, settings.term_grace);
    follow_log(w, &mut activity);
    samples.extend(process::load_now());
    // A command killed by a signal has no exit code; report it the shell's way, 128 + signal.
    let code = status.map_or(1, |s| s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(1)));
    let (state, exit) = match stopping {
        Stopping::TimedOut => (State::TimedOut, 124),
        // A command that traps TERM and exits 0 was still cancelled: never report success.
        Stopping::Cancelled => (State::Cancelled, if code == 0 { 128 + libc::SIGTERM } else { code }),
        Stopping::No if code == 0 => (State::Succeeded, 0),
        Stopping::No => (State::Failed, code),
    };
    Outcome {
        state,
        exit,
        duration: started.elapsed(),
        load_samples: samples,
        failure_reason: None,
        error_line: None,
        notes: activity.notes().to_vec(),
    }
}

/// Reads what the log gained, publishes the run's activity, and learns the durations of
/// the commands that finished. The log is read with no lock held.
fn follow_log(w: &Watch, activity: &mut Activity) {
    let finished = activity.scan(&w.rec.job.log, w.patterns, Timestamp::now());
    *crate::sync::lock(w.activity) = activity.clone();
    if finished.is_empty() {
        return;
    }
    let mut typicals = crate::sync::lock(w.typicals);
    for row in &finished {
        typicals.learn(row);
    }
    drop(typicals);
    activity::append_steps(w.paths, &finished);
}
