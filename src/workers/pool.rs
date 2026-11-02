//! What this daemon knows of its workers right now: whether each answered, its latest
//! machine readings, and why it is not taking work, until when. Kept in memory and written
//! to `state/workers/<name>.json` for `drmanhattan workers` and `status`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::fsutil;
use crate::paths::Paths;
use crate::time::Timestamp;

/// Readings older than this are not shown.
pub(crate) const STATS_STALE: Duration = Duration::from_secs(60);
/// How long a worker that failed stays out of admission.
pub(crate) const RETRY_AFTER: Duration = Duration::from_secs(120);

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub(crate) struct Live {
    /// None until it has been asked.
    pub(crate) online: Option<bool>,
    pub(crate) last_seen: Option<Timestamp>,
    pub(crate) checked_at: Option<Timestamp>,
    /// Its `machine-stats`: CPU, load, memory, temperature, running jobs.
    pub(crate) stats: Option<serde_json::Value>,
    /// Why it is not taking work, while `unavailable_until` lasts.
    pub(crate) reason: Option<String>,
    pub(crate) unavailable_until: Option<Timestamp>,
    /// The poller set the reason (it did not answer, or its drmanhattan cannot take
    /// work), so the next good answer clears it; a failed dispatch's reason lasts its window.
    #[serde(default)]
    pub(crate) from_poll: bool,
}

impl Live {
    /// Out of admission: it failed, and its retry window has not passed.
    pub(crate) fn unavailable(&self) -> Option<&str> {
        let until = self.unavailable_until?;
        (until > Timestamp::now()).then(|| self.reason.as_deref().unwrap_or("unavailable"))
    }

    /// Its readings, unless they are stale.
    pub(crate) fn fresh_stats(&self) -> Option<&serde_json::Value> {
        let seen = self.last_seen?;
        (seen.elapsed() <= STATS_STALE).then_some(self.stats.as_ref()).flatten()
    }
}

#[derive(Default)]
pub(crate) struct Pool {
    live: Mutex<HashMap<String, Live>>,
    dispatching: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

pub(crate) fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(Pool::default)
}

impl Pool {
    pub(crate) fn get(&self, worker: &str) -> Live {
        crate::sync::lock(&self.live).get(worker).cloned().unwrap_or_default()
    }

    fn update(&self, paths: &Paths, worker: &str, change: impl FnOnce(&mut Live)) {
        let live = {
            let mut all = crate::sync::lock(&self.live);
            let live = all.entry(worker.to_string()).or_default();
            change(live);
            live.clone()
        };
        if fsutil::ensure_private_dir(&paths.worker_states()).is_ok() {
            let _ = fsutil::write_json(&paths.worker_state(worker), &live);
        }
    }

    /// It answered, with these readings: a reason the poller gave before no longer holds.
    pub(crate) fn seen(&self, paths: &Paths, worker: &str, stats: serde_json::Value) {
        self.update(paths, worker, |l| {
            let now = Timestamp::now();
            (l.online, l.last_seen, l.checked_at, l.stats) = (Some(true), Some(now), Some(now), Some(stats));
            if l.from_poll {
                (l.reason, l.unavailable_until, l.from_poll) = (None, None, false);
            }
        });
    }

    /// It did not answer.
    pub(crate) fn lost(&self, paths: &Paths, worker: &str, reason: &str) {
        self.update(paths, worker, |l| {
            (l.online, l.checked_at) = (Some(false), Some(Timestamp::now()));
        });
        self.polled_unavailable(paths, worker, reason);
    }

    /// The poller found it cannot take work: until a later poll finds it can.
    pub(crate) fn polled_unavailable(&self, paths: &Paths, worker: &str, reason: &str) {
        self.unavailable(paths, worker, reason);
        self.update(paths, worker, |l| l.from_poll = true);
    }

    /// Keeps it out of admission for the retry window, saying why.
    pub(crate) fn unavailable(&self, paths: &Paths, worker: &str, reason: &str) {
        self.update(paths, worker, |l| {
            l.reason = Some(reason.to_string());
            l.unavailable_until = Some(Timestamp::now().plus(RETRY_AFTER));
            l.from_poll = false;
        });
    }

    /// Dispatches to one worker happen one at a time: hold this while dispatching.
    pub(crate) fn dispatch_lock(&self, worker: &str) -> Arc<Mutex<()>> {
        crate::sync::lock(&self.dispatching).entry(worker.to_string()).or_default().clone()
    }
}

/// What the last daemon wrote about each worker, for commands run beside it.
pub(crate) fn read_live(paths: &Paths, worker: &str) -> Live {
    fsutil::read_json(&paths.worker_state(worker)).unwrap_or_default()
}
