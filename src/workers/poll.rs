//! Asking each worker for its readings, every `DRMANHATTAN_WORKER_POLL_SECONDS` (15).
//!
//! A worker's slot count is one number wherever it is changed: the − and + here push it to
//! the worker (`push_slots`), and a count changed on the worker comes back with its readings.
//! Pausing a worker (0) stays here: it only says this machine sends it nothing.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::log;
use crate::paths::Paths;
use crate::settings::{load_config, update_config, MAX_SLOTS};
use crate::workers::host::MachineStats;
use crate::workers::WorkerConfig;
use crate::workers::{dispatch, pool, ssh};

const ANSWER_LIMIT: Duration = Duration::from_secs(20);

/// Starts the poller thread. Workers added to `config.json` later are picked up.
pub(crate) fn start_poller(paths: Paths, every: Duration) {
    let _ = std::thread::Builder::new().name("workers".into()).spawn(move || loop {
        let workers = load_config(&paths).map(|c| c.workers).unwrap_or_default();
        // One thread per worker, so one that hangs does not hold up the others.
        let asks: Vec<_> = workers
            .into_iter()
            .map(|w| {
                let paths = paths.clone();
                std::thread::spawn(move || poll_one(&paths, &w))
            })
            .collect();
        for ask in asks {
            let _ = ask.join();
        }
        std::thread::sleep(every);
    });
}

fn poll_one(paths: &Paths, worker: &WorkerConfig) {
    let asked = Instant::now();
    let out = ssh::run(worker, &ssh::drmanhattan(worker, &["machine-stats"]), None, ANSWER_LIMIT);
    let stats: Option<MachineStats> = (out.code == 0).then(|| serde_json::from_str(out.stdout.trim()).ok()).flatten();
    let Some(stats) = stats else {
        let why = if out.code == 0 { "its drmanhattan gave no readings".to_string() } else { out.first_line() };
        return pool().lost(paths, &worker.name, &format!("unreachable: {why}"));
    };
    let unusable = dispatch::unusable(&stats);
    if worker.slots != 0 && stats.capacity != worker.slots && pool().may_adopt_slots(&worker.name, asked) {
        adopt_slots(paths, worker, stats.capacity);
    }
    pool().seen(paths, &worker.name, serde_json::to_value(&stats).unwrap_or_default());
    if let Some(why) = unusable {
        pool().polled_unavailable(paths, &worker.name, &why);
    }
}

/// Takes the worker's own slot count, unless the count here changed since the poll read it:
/// a pause (0) set meanwhile must not be undone by an answer to an earlier question.
fn adopt_slots(paths: &Paths, worker: &WorkerConfig, slots: u32) {
    let _ = update_config(paths, |config| {
        if let Some(w) = config.workers.iter_mut().find(|w| w.name == worker.name && w.slots == worker.slots) {
            w.slots = slots.min(MAX_SLOTS);
        }
        Ok(())
    });
}

/// Sets the worker's own slot count to the one set here, as `drmanhattan slots N` there, in
/// the background; a failure is logged, and the worker's count comes back with the next poll.
/// Pushes go one at a time, each with the count as it is then, so quick clicks end on the last.
pub(crate) fn push_slots(paths: &Paths, worker: &str) {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    pool().slots_set(worker, true);
    let (paths, worker) = (paths.clone(), worker.to_string());
    let _ = std::thread::Builder::new().name("worker-slots".into()).spawn(move || {
        let _turn = crate::sync::lock(&ONE_AT_A_TIME);
        let found = load_config(&paths).ok().and_then(|c| c.workers.into_iter().find(|w| w.name == worker));
        let Some(w) = found.filter(|w| w.slots > 0) else { return pool().slots_set(&worker, false) };
        let slots = w.slots;
        let out = ssh::run(&w, &ssh::drmanhattan(&w, &["slots", &slots.to_string()]), None, ANSWER_LIMIT);
        if out.code != 0 {
            log::warn(format!("could not set {}'s slots to {slots}: {}", w.name, out.first_line()));
        }
        pool().slots_set(&w.name, false);
    });
}

/// Pauses or resumes the worker, as `drmanhattan pause` or `resume` there, in the background.
/// A pause keeps it out of admission here at once; either way it is asked again right after,
/// so the dashboard shows what it says.
pub(crate) fn push_pause(paths: &Paths, worker: &str, paused: bool) -> crate::error::Result<()> {
    let Some(w) = load_config(paths)?.workers.into_iter().find(|w| w.name == worker) else {
        return crate::error::invalid(format!("no worker is named {worker:?}"));
    };
    if paused {
        pool().polled_unavailable(paths, worker, &format!("paused on {worker}"));
    }
    let paths = paths.clone();
    let _ = std::thread::Builder::new().name("worker-pause".into()).spawn(move || {
        let word = if paused { "pause" } else { "resume" };
        let out = ssh::run(&w, &ssh::drmanhattan(&w, &[word]), None, ANSWER_LIMIT);
        if out.code != 0 {
            log::warn(format!("could not {word} {}: {}", w.name, out.first_line()));
        }
        poll_one(&paths, &w);
    });
    Ok(())
}
