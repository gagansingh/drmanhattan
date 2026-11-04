//! One run as the page shows it. Times are Unix epoch seconds, which the page formats in
//! the viewer's time zone; durations are whole seconds.

use std::path::PathBuf;

use serde::Serialize;

use crate::activity::ActivityView;
use crate::estimate::Estimate;
use crate::job::{PrepareState, Record, RunId, State};
use crate::naming::short_path;
use crate::sources::{self, Sessions, SourceView};
use crate::time::Timestamp;

#[derive(Serialize)]
pub(super) struct RunView {
    run_id: RunId,
    state: State,
    label: String,
    category: String,
    cwd: PathBuf,
    cwd_short: String,
    command: Vec<String>,
    /// Its `prepare` command, so the log's line for it can be shown as a command too.
    prepare_command: Option<Vec<String>>,
    process_group: Option<i32>,
    slot: Option<u32>,
    priority: bool,
    variant: Option<String>,
    job_file: PathBuf,
    log: PathBuf,
    source: Option<SourceView>,
    wait_started_at: f64,
    started_at: Option<f64>,
    finished_at: Option<f64>,
    exit_status: Option<i32>,
    failure_reason: Option<String>,
    error_line: Option<String>,
    load_at_start: Option<f64>,
    load_avg: Option<f64>,
    load_max: Option<f64>,
    /// `::note` badges: the record's once it has finished, else what its log says so far.
    pub(super) notes: Vec<String>,
    /// Where its `prepare` command is, for a job that has one.
    prepare: Option<PrepareState>,
    group: Option<String>,
    superseded_by: Option<RunId>,
    /// The worker it runs or ran on; none for this machine.
    machine: Option<String>,
    /// Its job's name on the worker, once it has been handed over.
    remote_job: Option<String>,
    remote_fallback: Option<String>,
    service: bool,
    /// A quick run, or a quick waiter: it starts without waiting for a slot.
    quick: bool,
    /// Why it is quick, or waits for a slot.
    lane_reason: Option<String>,
    /// A service's port, when it is ready by its port.
    port: Option<u16>,
    ready_at: Option<f64>,
    ttl: Option<u64>,
    /// Waiting: another job of its group is running, so it cannot start yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) group_busy: Option<bool>,
    timeout: Option<u64>,
    typical_seconds: Option<f64>,
    typical_p90: Option<f64>,
    typical_basis: Option<&'static str>,
    typical_samples: usize,
    /// Set when today's load scales the estimate shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) load_factor: Option<f64>,
    #[serde(flatten)]
    pub(super) timing: Timing,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) activity: Option<ActivityView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) overdue: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) manual: Option<bool>,
    #[serde(flatten)]
    pub(super) eta: Option<Eta>,
}

/// The durations that make sense for the run's state.
#[derive(Serialize, Default)]
pub(super) struct Timing {
    /// Queued: how long so far.
    #[serde(skip_serializing_if = "Option::is_none")]
    waiting: Option<f64>,
    /// Running: how long so far, the expected time left, and whether it is past p90.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) elapsed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) remaining_estimate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    over_p90: Option<bool>,
    /// Finished: how long it ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<f64>,
    /// Running or finished: how long it waited first.
    #[serde(skip_serializing_if = "Option::is_none")]
    wait_seconds: Option<f64>,
}

/// A waiter's estimated start.
#[derive(Serialize)]
pub(super) struct Eta {
    pub(super) eta_start: f64,
    pub(super) eta_wait: u64,
    /// A run ahead of it has no estimate, so the start is unknown.
    pub(super) eta_open_ended: bool,
}

fn epoch(t: Option<Timestamp>) -> Option<f64> {
    t.map(Timestamp::epoch_seconds)
}

impl RunView {
    pub(super) fn is_service(&self) -> bool {
        self.service
    }

    pub(super) fn is_quick(&self) -> bool {
        self.quick
    }

    /// The (load-scaled) typical duration the view shows.
    pub(super) fn typical_seconds(&self) -> Option<f64> {
        self.typical_seconds
    }

    pub(super) fn new(rec: &Record, est: &Estimate, sessions: &Sessions, now: Timestamp) -> RunView {
        let job = &rec.job;
        RunView {
            run_id: rec.run_id.clone(),
            state: rec.state,
            label: job.label.clone(),
            category: job.category.clone(),
            cwd: job.cwd.clone(),
            cwd_short: short_path(&job.cwd.to_string_lossy()),
            command: job.command.clone(),
            prepare_command: job.prepare.clone(),
            process_group: rec.process_group,
            slot: rec.slot,
            priority: job.priority,
            variant: job.variant.clone(),
            job_file: job.job_file.clone(),
            log: job.log.clone(),
            source: sources::view(job.source.as_ref(), sessions, &job.cwd),
            wait_started_at: rec.queued_at.epoch_seconds(),
            started_at: epoch(rec.started_at),
            finished_at: epoch(rec.finished_at),
            exit_status: rec.exit_status,
            failure_reason: rec.failure_reason.clone(),
            error_line: rec.error_line.clone(),
            load_at_start: rec.load_at_start,
            load_avg: rec.load.map(|l| l.average),
            load_max: rec.load.map(|l| l.peak),
            notes: rec.notes.clone(),
            prepare: rec.prepare_status.as_ref().map(|p| p.state),
            group: job.group.clone(),
            superseded_by: rec.superseded_by.clone(),
            machine: rec.machine.clone(),
            remote_job: rec.remote_run.as_ref().map(|r| r.job.clone()),
            remote_fallback: rec.remote_fallback.clone(),
            service: job.service,
            quick: rec.quick,
            lane_reason: rec.lane_reason.clone(),
            port: match &job.ready {
                Some(crate::job::Ready::Port(p)) => Some(*p),
                _ => None,
            },
            ready_at: epoch(rec.ready_at),
            ttl: job.ttl,
            group_busy: None,
            timeout: job.timeout,
            typical_seconds: est.p50.map(ms_to_secs),
            typical_p90: est.p90.map(ms_to_secs),
            typical_basis: est.basis,
            typical_samples: est.samples.len(),
            load_factor: None,
            timing: timing(rec, est, now),
            activity: None,
            overdue: None,
            manual: None,
            eta: None,
        }
    }
}

/// Seconds, to the millisecond: the page shows short runs as `120ms` or `2.4s`.
fn secs(d: std::time::Duration) -> f64 {
    d.as_millis() as f64 / 1000.0
}

fn ms_to_secs(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

fn timing(rec: &Record, est: &Estimate, now: Timestamp) -> Timing {
    let waited_until = |end: Timestamp| Some(secs(rec.queued_at.until(end)));
    match (rec.state, rec.started_at) {
        (State::Queued, _) => Timing { waiting: waited_until(now), ..Timing::default() },
        (State::Running, started) => {
            let started = started.unwrap_or(now);
            let elapsed = started.until(now);
            let elapsed_ms = elapsed.as_millis() as u64;
            Timing {
                elapsed: Some(secs(elapsed)),
                remaining_estimate: est.remaining(elapsed_ms).map(ms_to_secs),
                over_p90: Some(est.p90.is_some_and(|p| elapsed_ms > p)),
                wait_seconds: waited_until(started),
                ..Timing::default()
            }
        }
        (_, Some(started)) => Timing {
            duration: Some(secs(started.until(rec.finished_at.unwrap_or(now)))),
            wait_seconds: waited_until(started),
            ..Timing::default()
        },
        // Cancelled or abandoned before it started: all of its time was waiting.
        (_, None) => Timing { wait_seconds: waited_until(rec.finished_at.unwrap_or(now)), ..Timing::default() },
    }
}
