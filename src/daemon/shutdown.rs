//! Signals and stopping.
//!
//! The first Ctrl-C (or SIGTERM, which `drmanhattan stop` sends) is graceful: no new runs
//! start, running ones get the shutdown grace period to finish, queued ones stay for the
//! next start. A second one stops at once: running jobs are cancelled and recorded so.
//! Jobs running on workers are never stopped: they carry on there, and the next start
//! follows them again.

use std::time::{Duration, Instant};

use crate::job::{RunId, State};
use crate::log::{self, Tone};
use crate::queue::{self, Shared};
use crate::settings::Settings;
use crate::signals;
use crate::time::{count, human};

pub(super) fn install_signal_handlers() {
    signals::count(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP]);
}

/// How many stop signals have arrived.
pub(super) fn signals() -> usize {
    signals::received()
}

pub(super) fn announce(shared: &Shared, settings: &Settings) {
    let (running, queued) = {
        let q = queue::lock(shared);
        let running: Vec<String> =
            q.records(State::Running).filter(|r| r.machine.is_none()).map(|r| r.job.label.clone()).collect();
        (running, q.count(State::Queued))
    };
    let waiting = if running.is_empty() {
        String::new()
    } else {
        format!(
            "waiting up to {} for {} to finish ({})",
            human(settings.shutdown_grace),
            count(running.len(), "running job", "running jobs"),
            running.join(", ")
        )
    };
    let staying = if queued > 0 {
        format!("{} for the next start", count(queued, "waiting job stays", "waiting jobs stay"))
    } else {
        String::new()
    };
    let again = if running.is_empty() { String::new() } else { "Ctrl-C again to stop at once".into() };
    let parts: Vec<String> =
        ["no new jobs will start".into(), waiting, staying, again].into_iter().filter(|p| !p.is_empty()).collect();
    log::notice(Tone::Warning, "stopping", parts.join(" · "));
}

/// Cancels every running job and waits until each has recorded how it ended.
pub(super) fn force_stop(shared: &Shared, settings: &Settings) {
    let running: Vec<RunId> = {
        let mut q = queue::lock(shared);
        // Runs on workers are left to finish there: the next start follows them again.
        let ids: Vec<RunId> =
            q.records(State::Running).filter(|r| r.machine.is_none()).map(|r| r.run_id.clone()).collect();
        for run_id in &ids {
            q.cancel(run_id);
        }
        ids
    };
    let deadline = Instant::now() + settings.term_grace + Duration::from_secs(5);
    while Instant::now() < deadline {
        let q = queue::lock(shared);
        if !running.iter().any(|id| q.runs.get(id).is_some_and(|e| e.rec.state == State::Running)) {
            return;
        }
        drop(q);
        std::thread::sleep(Duration::from_millis(50));
    }
}
