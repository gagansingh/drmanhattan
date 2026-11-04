//! Workers: other machines, each running its own drmanhattan, that take jobs from this one
//! over SSH.
//!
//! A job that may run remotely (`remote`) is admitted to a free worker slot like a local
//! one. Its runner then hands it over (`dispatch`): it mirrors the job's files (`mirror`) and
//! writes the job into the worker's inbox, where the worker's daemon runs it under its own
//! slots and writes its results as for any job. This machine follows that log back into the
//! job's own (`runner::remote`). `host` is the worker's side of all this. A poller asks each
//! worker for its readings every few seconds (`poll`), and `pool` keeps what it last said.

mod config;
mod dispatch;
mod host;
mod mirror;
mod poll;
mod pool;
mod ssh;

use crate::paths::Paths;
use crate::settings::Config;

pub(crate) use config::{validate, validate_route, WorkerConfig};
pub(crate) use dispatch::{dispatch, unusable};
pub(crate) use host::{dispatch_probe, dispatch_submit, machine_stats, PROTOCOL};
pub(crate) use poll::{push_pause, push_slots, start_poller};
pub(crate) use pool::{pool, read_live, Live};
pub(crate) use ssh::{command as ssh_command, drmanhattan as remote_argv, run as ssh_run};

/// What admission needs to know of one worker.
#[derive(Debug, Clone)]
pub(crate) struct Slots {
    pub(crate) name: String,
    /// How many of its slots this machine may use now (0: paused).
    pub(crate) capacity: u32,
    pub(crate) tags: Vec<String>,
    /// Why it is out of admission for a while, after a failure.
    pub(crate) unavailable: Option<String>,
    /// In `config.json`'s `route`: jobs that may run on a worker go to it (or another on the
    /// route), and only there.
    pub(crate) routed: bool,
}

/// The workers in `config.json`, as admission in this daemon sees them now.
pub(crate) fn slots(config: &Config) -> Vec<Slots> {
    slots_with(config, |name| pool().get(name))
}

/// The same, from what the daemon last wrote, for commands run beside it.
pub(crate) fn slots_read(paths: &Paths, config: &Config) -> Vec<Slots> {
    slots_with(config, |name| read_live(paths, name))
}

fn slots_with(config: &Config, live: impl Fn(&str) -> Live) -> Vec<Slots> {
    config
        .workers
        .iter()
        .map(|w| Slots {
            name: w.name.clone(),
            capacity: w.slots,
            tags: w.tags.clone(),
            unavailable: live(&w.name).unavailable().map(String::from),
            routed: config.route.contains(&w.name),
        })
        .collect()
}
