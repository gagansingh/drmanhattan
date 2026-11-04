//! One scheduler pass: pick up new job files, honour cancels, admit waiters.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::job::Job;
use crate::log;
use crate::paths::Paths;
use crate::queue::{self, claim_job, scan_inbox, Admitted, Shared};
use crate::settings::{effective_capacity, load_config, read_manual_order};
use crate::workers;

/// The scheduler's memory between passes: which problems it has already reported, so a
/// file that stays wrong is reported once rather than five times a second.
pub(super) struct Scheduler {
    pub(super) paths: Paths,
    pub(super) inbox: PathBuf,
    slots_env: Option<String>,
    reported: HashSet<PathBuf>,
    capacity_warned: bool,
}

impl Scheduler {
    pub(super) fn new(paths: Paths, inbox: PathBuf, slots_env: Option<String>) -> Scheduler {
        Scheduler { paths, inbox, slots_env, reported: HashSet::new(), capacity_warned: false }
    }

    /// Reading the inbox, claiming and validating new job files, and reading the capacity
    /// and manual order all happen without the queue lock; the lock is held only to add
    /// the new runs, apply cancels, admit and start prepare commands. Returns what to start:
    /// admitted runs, and runs whose `prepare` is to run.
    pub(super) fn pass(&mut self, shared: &Shared, admitting: bool) -> Vec<Admitted> {
        let scan = scan_inbox(&self.inbox);
        for (path, why) in scan.refused {
            self.report_once(&path, format_args!("skipping {}: {why}", path.display()));
        }
        let live = queue::lock(shared).live_names();
        let mut new_jobs: Vec<Job> = Vec::new();
        for files in scan.jobs {
            // A job named like one still queued or running waits its turn in the inbox.
            if live.contains(files.name()) {
                continue;
            }
            match claim_job(&files, &self.inbox) {
                Ok(job) => new_jobs.extend(job),
                Err(e) => self.report_once(&files.job(), format_args!("cannot claim {}: {e}", files.name())),
            }
        }
        let capacity = self.capacity();
        let manual = read_manual_order(&self.paths);
        let workers = load_config(&self.paths).map(|c| workers::slots(&c.workers)).unwrap_or_default();

        let mut q = queue::lock(shared);
        for job in new_jobs {
            q.enqueue(job);
        }
        for name in scan.cancels {
            let cancel = self.inbox.join(format!("{name}.cancel"));
            if q.cancel_by_name(&name) {
                let _ = fs::remove_file(&cancel);
            } else if !scan.waiting.contains(&name) {
                // A cancel for a job file not yet picked up stays until the job is queued.
                let _ = fs::remove_file(&cancel);
                log::event(
                    log::Tone::Quiet,
                    "ignored",
                    &format!("{name}.cancel"),
                    None,
                    &["no queued or running job has that name".into()],
                );
            }
        }
        if !admitting {
            return Vec::new();
        }
        let max_prepares = q.settings.max_prepares;
        let mut started = q.admit(capacity, &workers, &manual);
        started.extend(q.start_prepares(max_prepares, &manual));
        started
    }

    /// The capacity to admit up to; a bad setting is reported once and counts as 1.
    fn capacity(&mut self) -> u32 {
        match effective_capacity(&self.paths, self.slots_env.as_deref()) {
            Ok(slots) => {
                self.capacity_warned = false;
                slots
            }
            Err(e) => {
                if !std::mem::replace(&mut self.capacity_warned, true) {
                    log::problem(format_args!("{e}; using 1 slot until it is fixed"));
                }
                1
            }
        }
    }

    fn report_once(&mut self, path: &Path, message: std::fmt::Arguments) {
        if self.reported.insert(path.to_path_buf()) {
            log::problem(message);
        }
    }
}
