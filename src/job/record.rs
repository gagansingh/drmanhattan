//! A run's record: the job plus everything that happened to it. The same record is written
//! to `state/runs/<run id>.json` (where a restarted daemon reads it back) and, without the
//! job's environment, to `<job>.status.json` beside the job file.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::job::{Job, RunId};
use crate::time::Timestamp;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum State {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Abandoned,
}

impl State {
    pub(crate) fn is_terminal(self) -> bool {
        !matches!(self, State::Queued | State::Running)
    }

    /// Failed, timed out or abandoned: the run did not do its job. (A cancel was asked for.)
    pub(crate) fn is_failure(self) -> bool {
        matches!(self, State::Failed | State::TimedOut | State::Abandoned)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Running => "running",
            State::Succeeded => "succeeded",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
            State::TimedOut => "timed_out",
            State::Abandoned => "abandoned",
        }
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a job's `prepare` command is.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrepareState {
    /// Waiting for a turn to start.
    Pending,
    Running,
    Succeeded,
    /// It failed, timed out or was cancelled, and so did the run.
    Failed,
}

impl PrepareState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PrepareState::Pending => "pending",
            PrepareState::Running => "running",
            PrepareState::Succeeded => "succeeded",
            PrepareState::Failed => "failed",
        }
    }
}

/// The run's `prepare` command, which runs while the run is queued.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Prepare {
    pub(crate) state: PrepareState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) finished_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exit_status: Option<i32>,
}

/// The machine's load average while a run ran: sampled at its start, every heartbeat, and
/// at its end.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub(crate) struct LoadStats {
    pub(crate) average: f64,
    pub(crate) peak: f64,
    pub(crate) samples: u32,
}

impl LoadStats {
    pub(crate) fn of(samples: &[f64]) -> Option<LoadStats> {
        let round = |x: f64| (x * 10.0).round() / 10.0;
        (!samples.is_empty()).then(|| LoadStats {
            average: round(samples.iter().sum::<f64>() / samples.len() as f64),
            peak: round(samples.iter().copied().fold(0.0, f64::max)),
            samples: u32::try_from(samples.len()).unwrap_or(u32::MAX),
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Record {
    pub(crate) run_id: RunId,
    pub(crate) state: State,
    #[serde(flatten)]
    pub(crate) job: Job,
    /// The daemon that queued or ran this run.
    pub(crate) daemon_pid: u32,
    pub(crate) queued_at: Timestamp,
    /// The typical duration this run was ranked by, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expected_seconds: Option<u64>,
    /// Queued longer than this, it goes ahead of shorter newcomers.
    pub(crate) skip_after_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slot: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) started_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) process_group: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exit_status: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) finished_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) failure_reason: Option<String>,
    /// For a failed run: the line of its log to read first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error_line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) load_at_start: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) load: Option<LoadStats>,
    /// The `::note` badges its log printed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) notes: Vec<String>,
    /// For a job with a `prepare` command: how that is going. While it runs, the record's
    /// `process_group` is the prepare command's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prepare_status: Option<Prepare>,
    /// Replaced while it waited by this newer run of its `group`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) superseded_by: Option<RunId>,
    /// The worker it runs or ran on (its slot is that worker's); none for this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) machine: Option<String>,
    /// Once handed to a worker: where it is there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remote_run: Option<RemoteRun>,
    /// A service: when it became ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ready_at: Option<Timestamp>,
    /// Why a worker could not take it: it runs here from then on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remote_fallback: Option<String>,
    /// A quick run: it starts without waiting for a slot (see `HistoryIndex::lane`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) quick: bool,
    /// Why it is quick, or waits for a slot; none for a service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) lane_reason: Option<String>,
    /// Seconds of its current phase (waiting, then running) spent with the queue paused
    /// (`drmanhattan pause`): they count toward neither its wait nor its running time.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) paused_seconds: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// A run handed to a worker: its job there, and how much of its log came back.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct RemoteRun {
    pub(crate) worker: String,
    /// The job's name in the worker's inbox.
    pub(crate) job: String,
    /// The worker's run id, once it has queued it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) run_id: Option<String>,
    /// Bytes of output in the local log before the worker's: what the worker sent so far is
    /// the local log's output less this.
    pub(crate) log_base: u64,
}

impl Record {
    pub(crate) fn new(run_id: RunId, job: Job, expected_seconds: Option<u64>, skip_after_seconds: u64) -> Record {
        Record {
            run_id,
            state: State::Queued,
            daemon_pid: std::process::id(),
            queued_at: Timestamp::now(),
            expected_seconds,
            skip_after_seconds,
            slot: None,
            started_at: None,
            process_group: None,
            exit_status: None,
            finished_at: None,
            failure_reason: None,
            error_line: None,
            load_at_start: None,
            load: None,
            notes: Vec::new(),
            prepare_status: job.prepare.is_some().then_some(Prepare {
                state: PrepareState::Pending,
                started_at: None,
                finished_at: None,
                exit_status: None,
            }),
            superseded_by: None,
            machine: None,
            remote_run: None,
            remote_fallback: None,
            ready_at: None,
            quick: false,
            lane_reason: None,
            paused_seconds: 0,
            job,
        }
    }

    /// Whether pausing the queue stops it: a waiter, or a run here that is not a service
    /// (services, and runs on a worker, carry on), unless the pause is `after_current`,
    /// which leaves the runs to finish.
    pub(crate) fn stopped_by_pause(&self, after_current: bool) -> bool {
        match self.state {
            State::Queued => true,
            State::Running => !after_current && self.machine.is_none() && !self.job.service,
            _ => false,
        }
    }

    /// A running job that SIGSTOP may not pause cleanly: one that drives a simulator, which
    /// runs outside its process group and carries on, so the job may fail once continued.
    pub(crate) fn may_not_pause_cleanly(&self) -> bool {
        self.state == State::Running && drives_a_simulator(&self.job.command)
    }

    /// Adds the part of a pause since `since` that falls in its current phase, up to `now`.
    pub(crate) fn add_pause(&mut self, since: Timestamp, now: Timestamp, after_current: bool) {
        if !self.stopped_by_pause(after_current) {
            return;
        }
        let phase_start = if self.state == State::Running { self.started_at } else { Some(self.queued_at) };
        let from = phase_start.map_or(since, |t| t.max(since));
        self.paused_seconds += from.until(now).as_secs();
    }

    /// How long it has run by `end`, paused time aside.
    pub(crate) fn ran(&self, end: Timestamp) -> Option<Duration> {
        Some(self.started_at?.until(end).saturating_sub(Duration::from_secs(self.paused_seconds)))
    }

    /// How long it has waited, paused time aside.
    pub(crate) fn waited(&self, now: Timestamp) -> Duration {
        self.queued_at.until(now).saturating_sub(Duration::from_secs(self.paused_seconds))
    }

    /// The record as `<job>.status.json` shows it: the job's environment may hold secrets
    /// and stays in the private state directory.
    pub(crate) fn public(&self) -> Record {
        let mut public = self.clone();
        public.job.env.clear();
        public
    }

    /// What `<job>.exit` says once the run is over: the command's own status when it has
    /// one; otherwise 124 for a timeout, 130 for a cancel before it started, 1 otherwise.
    pub(crate) fn exit_code(&self) -> i32 {
        self.exit_status.unwrap_or(match self.state {
            State::TimedOut => 124,
            State::Cancelled => 130,
            _ => 1,
        })
    }

    /// Holds one of its machine's slots: neither a service nor a quick run.
    pub(crate) fn holds_slot(&self) -> bool {
        !self.job.service && !self.quick
    }

    /// Queued, and its `prepare` (if it has one) has succeeded: it may take a slot.
    pub(crate) fn is_ready(&self) -> bool {
        self.state == State::Queued && self.prepare_status.as_ref().map_or(true, |p| p.state == PrepareState::Succeeded)
    }

    /// When the run finished, or the latest thing that happened to it.
    pub(crate) fn last_event(&self) -> Timestamp {
        self.finished_at.or(self.started_at).unwrap_or(self.queued_at)
    }
}

/// `<job>.status.json` for a job file that never became a run: one that was not a valid
/// job, or one a previous daemon claimed and lost track of.
#[derive(Serialize)]
pub(crate) struct FileStatus<'a> {
    pub(crate) state: &'static str,
    pub(crate) failure_reason: &'a str,
    pub(crate) job_file: &'a std::path::Path,
    pub(crate) finished_at: Timestamp,
}

impl<'a> FileStatus<'a> {
    pub(crate) fn rejected(reason: &'a str, job_file: &'a std::path::Path) -> FileStatus<'a> {
        FileStatus { state: "rejected", failure_reason: reason, job_file, finished_at: Timestamp::now() }
    }

    pub(crate) fn abandoned(reason: &'a str, job_file: &'a std::path::Path) -> FileStatus<'a> {
        FileStatus { state: State::Abandoned.as_str(), failure_reason: reason, job_file, finished_at: Timestamp::now() }
    }
}

/// Whether a command looks like it drives an iOS or other Apple simulator: `xcodebuild`
/// testing, or with a simulator `-destination`, or `simctl`. The words are read from the whole
/// command, so `sh -c "..."` and wrappers count too.
pub(crate) fn drives_a_simulator(command: &[String]) -> bool {
    let words: Vec<&str> = command.iter().flat_map(|a| a.split_whitespace()).collect();
    let program = |name: &str| words.iter().any(|w| *w == name || w.ends_with(&format!("/{name}")));
    let testing =
        words.iter().any(|w| matches!(w.trim_matches(|c| c == '"' || c == '\''), "test" | "test-without-building"));
    // `-destination 'platform=iOS Simulator,…'`: the platform has a space in it.
    let text = command.join(" ");
    let simulator_destination = text.contains("-destination") && text.contains("Simulator");
    (program("xcodebuild") && (testing || simulator_destination)) || program("simctl")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{JobFiles, JobSpec};

    #[test]
    fn a_record_round_trips_through_its_file() {
        let inbox = std::env::temp_dir();
        let files = JobFiles::new(&inbox, "round-trip");
        let job =
            JobSpec::parse(r#"{"command": ["true"], "env": {"A": "1"}, "prepare": "make deps", "remote": "allowed"}"#)
                .unwrap()
                .resolve(&files, &inbox)
                .unwrap();
        std::fs::write(files.claimed(), "{}").unwrap();
        let job = Job { claim: crate::fsutil::file_id(&files.claimed()), ..job };
        let mut rec = Record::new(RunId::new(), job, Some(5), 600);
        rec.remote_run = Some(RemoteRun { worker: "w".into(), job: "j".into(), run_id: None, log_base: 3 });
        rec.load = LoadStats::of(&[1.0, 2.0]);
        let text = serde_json::to_string(&rec).unwrap();
        let back: Record = serde_json::from_str(&text).unwrap();
        assert_eq!(back.job, rec.job);
        assert_eq!((&back.run_id, back.load), (&rec.run_id, rec.load));
        assert_eq!(back.prepare_status, rec.prepare_status, "the job's prepare and the run's status do not collide");
        assert_eq!(back.remote_run, rec.remote_run, "nor the job's remote and where the run went");
        let fields = serde_json::to_value(&rec).unwrap();
        let job_fields = serde_json::to_value(&rec.job).unwrap();
        let record_only = [
            "run_id",
            "state",
            "daemon_pid",
            "queued_at",
            "skip_after_seconds",
            "expected_seconds",
            "notes",
            "prepare_status",
            "remote_run",
            "quick",
            "lane_reason",
        ];
        for key in record_only {
            assert!(job_fields.get(key).is_none(), "a job field is named {key}, which the record uses too");
        }
        assert!(fields.get("remote_run").is_some());
        assert!(serde_json::to_string(&rec.public()).unwrap().find("\"env\"").is_none());
        let _ = std::fs::remove_file(files.claimed());
    }

    #[test]
    fn simulator_jobs_are_told_apart() {
        let cmd = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(drives_a_simulator(&cmd(&["xcodebuild", "-scheme", "App", "test"])));
        assert!(drives_a_simulator(&cmd(&["/usr/bin/xcodebuild", "test-without-building"])));
        assert!(drives_a_simulator(&cmd(&["sh", "-c", "cd app && xcodebuild test -scheme App"])));
        assert!(drives_a_simulator(&cmd(&[
            "xcodebuild",
            "build",
            "-destination",
            "platform=iOS Simulator,name=iPhone 16"
        ])));
        assert!(drives_a_simulator(&cmd(&["xcrun", "simctl", "boot", "iPhone 16"])));
        assert!(!drives_a_simulator(&cmd(&["xcodebuild", "-scheme", "App", "build"])));
        assert!(!drives_a_simulator(&cmd(&["cargo", "test"])));
        assert!(!drives_a_simulator(&cmd(&["npm", "test"])));
    }
}
