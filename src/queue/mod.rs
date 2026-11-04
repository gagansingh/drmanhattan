//! The queue: job files in, runs out.
//!
//! One daemon owns everything. Each scheduler pass picks up new job files from the inbox
//! (`inbox`), honours `.cancel` files, and admits waiting runs into free slots while fewer
//! than the capacity are running (`admit`, in the order `rank` gives). A runner thread per
//! admitted run executes it (`crate::runner`). Every state change rewrites the run's record
//! under `state/runs/` and its `.status.json` beside the job file, and the end of a run
//! writes `.exit` (`persist`). A restarted daemon picks up where the last one stopped
//! (`recover`); old runs and files are cleared away (`prune`).
//!
//! Locking: the whole queue sits behind one mutex (`Shared`). The scheduler loop, the runner
//! threads and the dashboard lock it briefly; nothing slow happens under it. Reading inbox
//! files and settings, compiling patterns and following logs all happen outside it. Persisting
//! does happen under it, so the files always change in the same order as the records: a few
//! small writes. Two smaller locks sit beside it, each held only to copy a value in or out: a
//! running run's `Activity`, which its runner publishes after reading its log, and the
//! learned `Typicals`. When both are needed, the queue's is taken first, or released first.

mod admit;
mod inbox;
mod persist;
mod prune;
mod rank;
mod recover;

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub(crate) use admit::Admitted;
pub(crate) use inbox::{claim_job, scan_inbox};
pub(crate) use persist::read_records;
pub(crate) use prune::prune_files;
pub(crate) use rank::rank;
pub(crate) use recover::HEARTBEAT_EVERY;

use crate::activity::{Activity, Patterns, Typicals};
use crate::estimate::{Estimate, HistoryIndex, Lane, LoadModel};
use crate::history::{self, HistoryRow};
use crate::job::{Job, Record, RemoteMode, RunId, State};
use crate::log::{self, Tone};
use crate::naming::tilde_path;
use crate::paths::Paths;
use crate::process;
use crate::settings::Settings;
use crate::time::{human, Timestamp};

/// A message to a run's runner thread: its command exited, or someone asked to cancel it.
pub(crate) enum RunEvent {
    Exited(Option<std::process::ExitStatus>),
    Cancel,
}

/// What a running run has besides its record: the way to reach its runner thread, and what
/// its log says it is doing (written by the runner, read by the dashboard).
pub(crate) struct Live {
    pub(crate) control: Sender<RunEvent>,
    pub(crate) activity: Arc<Mutex<Activity>>,
    /// A cancel was asked for. Its runner may have stopped listening (a prepare command
    /// that just ended), so it also checks this before going on.
    pub(crate) cancel_requested: bool,
}

/// One run as the daemon holds it.
pub(crate) struct Entry {
    pub(crate) rec: Record,
    /// Some while it runs.
    pub(crate) live: Option<Live>,
    /// Queue the same job again once this run has ended (a service's restart).
    pub(crate) restart: bool,
}

pub(crate) struct Queue {
    pub(crate) paths: Paths,
    pub(crate) settings: Settings,
    pub(crate) inbox: PathBuf,
    pub(crate) runs: BTreeMap<RunId, Entry>,
    /// The newest rows of `history.jsonl`, which the load model reads.
    pub(crate) history: Vec<HistoryRow>,
    /// All of it, by what the estimates and lanes match on.
    index: HistoryIndex,
    /// Fitted from `history`, refitted when a run finishes.
    pub(crate) model: LoadModel,
    /// Usual durations of `::start`/`::done` commands. Its own lock, so runners can learn
    /// from their logs without taking the queue's.
    pub(crate) typicals: Arc<Mutex<Typicals>>,
    /// The activity patterns of a job that gives none: `config.json`'s, else the markers.
    pub(crate) default_patterns: Arc<Patterns>,
}

pub(crate) type Shared = Arc<Mutex<Queue>>;

pub(crate) fn lock(shared: &Shared) -> MutexGuard<'_, Queue> {
    crate::sync::lock(shared)
}

impl Queue {
    pub(crate) fn new(paths: Paths, settings: Settings, inbox: PathBuf, default_patterns: Patterns) -> Queue {
        let mut all = history::read_recent(&paths, usize::MAX);
        let index = HistoryIndex::of(&all);
        let history = all.split_off(all.len().saturating_sub(history::IN_MEMORY));
        Queue {
            model: LoadModel::fit(&history, process::cpu_count()),
            index,
            typicals: Arc::new(Mutex::new(Typicals::load(&paths))),
            default_patterns: Arc::new(default_patterns),
            history,
            paths,
            settings,
            inbox,
            runs: BTreeMap::new(),
        }
    }

    pub(crate) fn count(&self, state: State) -> usize {
        self.runs.values().filter(|e| e.rec.state == state).count()
    }

    pub(crate) fn records(&self, state: State) -> impl Iterator<Item = &Record> {
        self.runs.values().map(|e| &e.rec).filter(move |r| r.state == state)
    }

    /// The names of jobs queued or running. A second job file with one of these names waits
    /// in the inbox until the first is done.
    pub(crate) fn live_names(&self) -> HashSet<String> {
        self.runs
            .values()
            .filter(|e| !e.rec.state.is_terminal())
            .map(|e| e.rec.job.files().name().to_string())
            .collect()
    }

    /// The run's expected duration on this machine.
    pub(crate) fn estimate(&self, job: &Job) -> Estimate {
        self.estimate_on(job, None)
    }

    /// The run's expected duration on `machine` (a worker; None for this machine).
    pub(crate) fn estimate_on(&self, job: &Job, machine: Option<&str>) -> Estimate {
        let variant = job.variant.as_deref().unwrap_or_default();
        self.index.estimate(&job.category, &job.label, variant, machine)
    }

    /// Whether the job is quick here, and why; None for a service, which has a lane of its own.
    /// A job that may go to a worker, or came from another machine's queue, takes a slot:
    /// slots are what machines share out between them.
    pub(crate) fn lane(&self, job: &Job) -> Option<Lane> {
        if job.service {
            return None;
        }
        if job.remote != RemoteMode::Never {
            return Some(Lane { quick: false, reason: "it may run on a worker".into() });
        }
        if let Some(from) = &job.dispatched_from {
            return Some(Lane { quick: false, reason: format!("sent here by {from}") });
        }
        if self.settings.max_quick == 0 {
            return Some(Lane { quick: false, reason: "the quick lane is off (DRMANHATTAN_QUICK=0)".into() });
        }
        let shape = crate::naming::script_shape(&job.command);
        let script = shape.as_deref().map_err(String::as_str);
        Some(self.index.lane(&job.category, &job.label, job.variant.as_deref().unwrap_or_default(), script))
    }

    /// Decides the lane of every waiting run again: a run that finished may have shown a
    /// command to be quick, or not. Persists the runs whose lane changed.
    pub(crate) fn refresh_lanes(&mut self) {
        let changed: Vec<(RunId, Option<Lane>)> = self
            .records(State::Queued)
            .filter_map(|r| {
                let lane = self.lane(&r.job);
                let now = (lane.as_ref().is_some_and(|l| l.quick), lane.as_ref().map(|l| &l.reason));
                (now != (r.quick, r.lane_reason.as_ref())).then(|| (r.run_id.clone(), lane))
            })
            .collect();
        for (run_id, lane) in changed {
            if let Some(e) = self.runs.get_mut(&run_id) {
                e.rec.quick = lane.as_ref().is_some_and(|l| l.quick);
                e.rec.lane_reason = lane.map(|l| l.reason);
                self.persist(&run_id);
            }
        }
    }

    /// Queues a validated job: gives it a run id, its expected duration and its
    /// anti-starvation bound.
    pub(crate) fn enqueue(&mut self, job: Job) -> RunId {
        let expected = self.estimate(&job).p50.map(|ms| ms.div_ceil(1000));
        // Queued longer than this, the run goes ahead of shorter newcomers: twice its own
        // estimate, but never below the floor, so a tiny estimate cannot jump the queue.
        let floor = self.settings.max_skip.as_secs();
        let skip_after = expected.map_or(floor, |e| floor.max(2 * e));
        let run_id = RunId::new();
        let details = [
            job.category.clone(),
            tilde_path(&job.cwd),
            expected.map(|e| format!("usually {}", human(Duration::from_secs(e)))).unwrap_or_default(),
            if job.priority { "priority".into() } else { String::new() },
            format!("run {run_id}"),
        ];
        log::event(Tone::Quiet, "queued", &job.label, Some(&run_id), &details);
        let group = job.group.clone();
        let lane = self.lane(&job);
        let mut rec = Record::new(run_id.clone(), job, expected, skip_after);
        rec.quick = lane.as_ref().is_some_and(|l| l.quick);
        rec.lane_reason = lane.map(|l| l.reason);
        self.runs.insert(run_id.clone(), Entry { rec, live: None, restart: false });
        self.persist(&run_id);
        if let Some(group) = group {
            self.supersede_in_group(&group, &run_id);
        }
        run_id
    }

    /// Cancels the runs of `group` still waiting other than `newer`, which replaces them:
    /// a group has at most one waiter, and it is the newest.
    fn supersede_in_group(&mut self, group: &str, newer: &RunId) {
        let older: Vec<RunId> = self
            .records(State::Queued)
            .filter(|r| r.run_id != *newer && r.job.group.as_deref() == Some(group))
            .map(|r| r.run_id.clone())
            .collect();
        for run_id in older {
            let Some(e) = self.runs.get_mut(&run_id) else { continue };
            e.rec.superseded_by = Some(newer.clone());
            e.rec.failure_reason = Some(format!("replaced by a newer job in group {group} (run {newer})"));
            let label = e.rec.job.label.clone();
            log::event(Tone::Quiet, "replaced", &label, Some(&run_id), &[format!("by run {newer} in group {group}")]);
            self.cancel(&run_id);
        }
    }

    /// Cancels the queued or running job named `name` (from `<name>.cancel`). Returns false
    /// when no such job is live.
    pub(crate) fn cancel_by_name(&mut self, name: &str) -> bool {
        let target = self
            .runs
            .values()
            .find(|e| !e.rec.state.is_terminal() && e.rec.job.files().name() == name)
            .map(|e| e.rec.run_id.clone());
        match target {
            Some(run_id) => {
                self.cancel(&run_id);
                true
            }
            None => false,
        }
    }

    /// A queued run is cancelled on the spot; a running one is asked to stop, and its runner
    /// records the outcome once the process group is gone.
    pub(crate) fn cancel(&mut self, run_id: &RunId) {
        let Some(e) = self.runs.get_mut(run_id) else { return };
        match e.rec.state {
            // Preparing: its prepare thread stops the command and records the cancel.
            State::Queued if e.live.is_some() => {
                if let Some(live) = e.live.as_mut() {
                    live.cancel_requested = true;
                    let _ = live.control.send(RunEvent::Cancel);
                }
                log::event(Tone::Warning, "cancel", &e.rec.job.label, Some(run_id), &["stopping its prepare".into()]);
            }
            State::Queued => {
                e.rec.state = State::Cancelled;
                e.rec.finished_at = Some(Timestamp::now());
                let label = e.rec.job.label.clone();
                self.persist(run_id);
                log::event(Tone::Quiet, "cancelled", &label, Some(run_id), &["before it started".into()]);
            }
            State::Running => {
                if let Some(live) = &e.live {
                    let _ = live.control.send(RunEvent::Cancel);
                }
                log::event(Tone::Warning, "cancel", &e.rec.job.label, Some(run_id), &["stopping it".into()]);
            }
            _ => {}
        }
    }

    /// Stops a run and queues its job again once it has ended: a service's restart.
    pub(crate) fn restart(&mut self, run_id: &RunId) {
        let Some(e) = self.runs.get_mut(run_id) else { return };
        if e.rec.state.is_terminal() {
            return;
        }
        e.restart = true;
        self.cancel(run_id);
        // A run cancelled before it started has ended already.
        self.restart_if_asked(run_id);
    }

    /// Queues an ended run's job again if a restart was asked for.
    pub(crate) fn restart_if_asked(&mut self, run_id: &RunId) {
        let Some(e) = self.runs.get_mut(run_id) else { return };
        if !e.restart || !e.rec.state.is_terminal() {
            return;
        }
        e.restart = false;
        let job = e.rec.job.clone();
        // The new run's results replace these; its .exit must not read as already written.
        let _ = crate::fsutil::remove_if_present(&job.files().exit());
        self.enqueue(job);
    }

    /// Records a run that was admitted but whose runner could not be started.
    pub(crate) fn fail_to_start(&mut self, run_id: &RunId, reason: &str) {
        let Some(e) = self.runs.get_mut(run_id) else { return };
        e.rec.state = State::Failed;
        e.rec.exit_status = Some(126);
        e.rec.failure_reason = Some(reason.to_string());
        e.rec.finished_at = Some(Timestamp::now());
        e.live = None;
        let label = e.rec.job.label.clone();
        self.persist(run_id);
        log::event(Tone::Bad, "failed", &label, Some(run_id), &[reason.to_string()]);
    }

    /// Records a finished run's timing: `history.jsonl`, the rows in memory, and the load
    /// model refitted from them.
    pub(crate) fn record_history(&mut self, row: HistoryRow) {
        if let Err(e) = history::append(&self.paths, &row) {
            log::problem(format_args!("could not record timing history: {e}"));
        }
        self.index.add(&row);
        self.history.push(row);
        let excess = self.history.len().saturating_sub(history::IN_MEMORY);
        self.history.drain(..excess);
        self.model = LoadModel::fit(&self.history, process::cpu_count());
        self.refresh_lanes();
    }
}
