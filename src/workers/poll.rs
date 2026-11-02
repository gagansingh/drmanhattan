//! Asking each worker for its readings, every `DRMANHATTAN_WORKER_POLL_SECONDS` (15).

use std::time::Duration;

use crate::paths::Paths;
use crate::settings::load_config;
use crate::workers::host::MachineStats;
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

fn poll_one(paths: &Paths, worker: &crate::workers::WorkerConfig) {
    let out = ssh::run(worker, &ssh::drmanhattan(worker, &["machine-stats"]), None, ANSWER_LIMIT);
    let stats: Option<MachineStats> = (out.code == 0).then(|| serde_json::from_str(out.stdout.trim()).ok()).flatten();
    let Some(stats) = stats else {
        let why = if out.code == 0 { "its drmanhattan gave no readings".to_string() } else { out.first_line() };
        return pool().lost(paths, &worker.name, &format!("unreachable: {why}"));
    };
    let unusable = dispatch::unusable(&stats);
    pool().seen(paths, &worker.name, serde_json::to_value(&stats).unwrap_or_default());
    if let Some(why) = unusable {
        pool().polled_unavailable(paths, &worker.name, &why);
    }
}
