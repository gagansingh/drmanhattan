//! Running a job on a worker: hand it over, then follow it there until it ends.
//!
//! The run holds a slot of the worker's on this machine from admission on. Its runner
//! dispatches it (`workers::dispatch`); if that fails the run goes back into the local
//! queue, to run here. Once it has started there, `drmanhattan follow <job> --from <bytes>`
//! on the worker streams its log into the local one, so the dashboard, `follow`, the error
//! line and the activity work as for a local run. A dropped connection is retried every
//! five seconds, from the bytes already received; a worker silent for three minutes fails
//! the run. A cancel or timeout here cancels the job there, and the run ends once the
//! worker says it has. The run's record names the job on the worker, so a daemon that
//! restarts meanwhile picks it up again.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::supervise::{follow_log, Outcome, Watch};
use super::{open_log, Completion, Phase};
use crate::activity::{Activity, Patterns, OWN_LINE_PREFIX};
use crate::job::{Record, RemoteRun, State};
use crate::joblog;
use crate::log::{self, Tone};
use crate::queue::{self, Admitted, RunEvent, Shared};
use crate::settings::load_config;
use crate::time::{human, Timestamp};
use crate::workers::{self, pool, WorkerConfig};

const RECONNECT_EVERY: Duration = Duration::from_secs(5);
/// A worker that has not answered for this long has lost the run.
const LOST_AFTER: Duration = Duration::from_secs(180);
const ASK_LIMIT: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, PartialEq)]
enum Stopping {
    No,
    Cancelled,
    TimedOut,
}

pub(super) fn run(shared: &Shared, admitted: Admitted) {
    let Admitted { record: rec, events, exit_sender, activity, default_patterns } = admitted;
    let patterns = match &rec.job.activity {
        Some(config) => Patterns::compile(config).map_or(default_patterns, Arc::new),
        None => default_patterns,
    };
    let mut completion = Completion {
        shared,
        run_id: &rec.run_id,
        phase: Phase::Run,
        process_group: None,
        group_guard: None,
        finished: false,
    };
    let (paths, settings, typicals) = {
        let q = queue::lock(shared);
        (q.paths.clone(), q.settings.clone(), q.typicals.clone())
    };
    let name = rec.machine.clone().unwrap_or_default();
    let worker = load_config(&paths).ok().and_then(|c| c.workers.into_iter().find(|w| w.name == name));
    let Some(worker) = worker else {
        return requeue(&mut completion, &format!("worker {name} is no longer in config.json"), true);
    };
    let log = match open_log(&rec, true) {
        Ok(log) => log,
        Err(e) => return completion.finish(State::Failed, 126, Some(e.to_string()), Duration::ZERO),
    };
    let say = |text: &str| {
        let _ = (&log).write_all(format!("{OWN_LINE_PREFIX}{text}\n").as_bytes());
    };

    let remote = match rec.remote_run.clone() {
        Some(remote) => {
            log::event(Tone::Quiet, "reattached", &rec.job.label, Some(&rec.run_id), &[format!("on {name}")]);
            remote
        }
        None => {
            say(&format!(
                "{} sent to {name} (slot {}) at {} · run {}",
                rec.job.label,
                rec.slot.unwrap_or(0),
                Timestamp::now().local_time(),
                rec.run_id
            ));
            let cancel_asked = std::cell::Cell::new(false);
            let cancelled = || {
                if matches!(events.try_recv(), Ok(RunEvent::Cancel)) {
                    cancel_asked.set(true);
                }
                cancel_asked.get()
            };
            let lock = pool().dispatch_lock(&name);
            let dispatched = {
                let _one_at_a_time = crate::sync::lock(&lock);
                let on_submit = |job: &str| {
                    let remote = RemoteRun {
                        worker: name.clone(),
                        job: job.to_string(),
                        run_id: None,
                        log_base: joblog::output_len(&rec.job.log),
                    };
                    set_remote(shared, &rec, remote);
                };
                workers::dispatch(&worker, &rec, &paths, &say, &cancelled, &on_submit)
            };
            match dispatched {
                Ok(started) => {
                    let mut remote = current_remote(shared, &rec).unwrap_or(RemoteRun {
                        worker: name.clone(),
                        job: started.job.clone(),
                        run_id: None,
                        log_base: joblog::output_len(&rec.job.log),
                    });
                    remote.run_id = started.run_id;
                    set_remote(shared, &rec, remote.clone());
                    log::event(Tone::Start, "dispatched", &rec.job.label, Some(&rec.run_id), &[format!("to {name}")]);
                    remote
                }
                Err(_) if cancel_asked.get() => {
                    say("cancelled before it started");
                    return completion.finish(State::Cancelled, 130, None, Duration::ZERO);
                }
                Err(failure) => {
                    let next = if failure.worker_level { "back in the queue" } else { "running here" };
                    say(&format!("worker {name} unavailable: {}; {next}", failure.reason));
                    if failure.worker_level {
                        pool().unavailable(&paths, &name, &failure.reason);
                    }
                    drop(log);
                    let reason = format!("worker {name} unavailable: {}", failure.reason);
                    return requeue(&mut completion, &reason, !failure.worker_level);
                }
            }
        }
    };
    drop(log);

    let watch = Watch {
        rec: &rec,
        events: &events,
        activity: &activity,
        patterns: &patterns,
        typicals: &typicals,
        paths: &paths,
        settings: &settings,
        log_from: remote.log_base,
        on_ready: &|| {},
    };
    let mut outcome = follow(&worker, &rec, &remote, &watch, &exit_sender);
    if outcome.state == State::Failed && outcome.failure_reason.as_deref().is_some_and(|r| r.starts_with("worker lost"))
    {
        pool().unavailable(&paths, &name, "it stopped answering while running a job");
    }
    if outcome.state == State::Succeeded {
        if let Err(why) = copy_back(&worker, &rec) {
            (outcome.state, outcome.exit, outcome.failure_reason) = (State::Failed, 1, Some(why));
        }
    }
    completion.complete(&rec, &outcome);
}

/// Copies the job's `sync_back` paths back from the worker, before the run is marked
/// finished, with a line in its log for each. The first that fails fails the run: what
/// waits for it would find the products missing.
fn copy_back(worker: &WorkerConfig, rec: &Record) -> Result<(), String> {
    let log = open_log(rec, true).ok();
    let say = |text: &str| {
        if let Some(mut log) = log.as_ref() {
            let _ = log.write_all(format!("{OWN_LINE_PREFIX}{text}\n").as_bytes());
        }
    };
    for path in &rec.job.sync_back {
        let started = Instant::now();
        if let Err(why) = workers::copy_back(worker, path) {
            say(&why);
            return Err(why);
        }
        say(&format!("copied back {path} from {} in {}", worker.name, human(started.elapsed())));
    }
    Ok(())
}

/// Follows the job on the worker until it ends there, or the worker is lost.
fn follow(
    worker: &WorkerConfig,
    rec: &Record,
    remote: &RemoteRun,
    w: &Watch,
    exit_sender: &std::sync::mpsc::Sender<RunEvent>,
) -> Outcome {
    let started = Instant::now();
    let timeout_at = rec.job.timeout.and_then(|t| started.checked_add(Duration::from_secs(t)));
    let received = Arc::new(AtomicU64::new(joblog::output_len(&rec.job.log).saturating_sub(remote.log_base)));
    let last_contact = Arc::new(Mutex::new(Instant::now()));
    let mut activity = Activity::from_offset(remote.log_base);
    let mut stopping = Stopping::No;
    let ask = |args: &[&str]| workers::ssh_run(worker, &workers::remote_argv(worker, args), None, ASK_LIMIT);
    let ended = |state: State, exit: i32, reason: Option<String>, activity: &Activity| Outcome {
        state,
        exit,
        duration: started.elapsed(),
        load_samples: Vec::new(),
        failure_reason: reason,
        error_line: None,
        notes: activity.notes().to_vec(),
    };
    loop {
        let from = received.load(Ordering::SeqCst).to_string();
        let mut command =
            workers::ssh_command(worker, &workers::remote_argv(worker, &["follow", &remote.job, "--from", &from]));
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Ok(mut child) = command.spawn() {
            let mut out = child.stdout.take();
            let (received, contact, sender, path) =
                (received.clone(), last_contact.clone(), exit_sender.clone(), rec.job.log.clone());
            std::thread::spawn(move || {
                let file = OpenOptions::new().append(true).custom_flags(libc::O_NOFOLLOW).open(&path);
                let mut buf = vec![0u8; 64 * 1024];
                while let (Some(pipe), Ok(mut file)) = (out.as_mut(), file.as_ref()) {
                    match pipe.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let _ = file.write_all(&buf[..n]);
                            received.fetch_add(n as u64, Ordering::SeqCst);
                            *crate::sync::lock(&contact) = Instant::now();
                        }
                    }
                }
                let _ = sender.send(RunEvent::Exited(child.wait().ok()));
            });
            // Until this follow ends: honour cancels and the timeout, and read the log.
            let mut next_scan = Instant::now();
            loop {
                let now = Instant::now();
                if now >= next_scan {
                    next_scan = now + TICK;
                    joblog::keep_bounded(&rec.job.log, w.settings.max_log_bytes);
                    follow_log(w, &mut activity);
                }
                if stopping == Stopping::No && timeout_at.is_some_and(|t| now >= t) {
                    stopping = Stopping::TimedOut;
                    let _ = ask(&["cancel", &remote.job]);
                }
                match w.events.recv_timeout(TICK) {
                    Ok(RunEvent::Exited(_)) | Err(RecvTimeoutError::Disconnected) => break,
                    Ok(RunEvent::Cancel) if stopping == Stopping::No => {
                        stopping = Stopping::Cancelled;
                        let _ = ask(&["cancel", &remote.job]);
                    }
                    Ok(RunEvent::Cancel) | Err(RecvTimeoutError::Timeout) => {}
                }
            }
        }
        follow_log(w, &mut activity);
        // The follow ended: the job is over there, or the connection dropped.
        let status = ask(&["status", &remote.job, "--json"]);
        if status.code == 0 {
            *crate::sync::lock(&last_contact) = Instant::now();
            let doc: serde_json::Value = serde_json::from_str(status.stdout.trim()).unwrap_or_default();
            if let Some(exit) = doc["exit"].as_i64() {
                if received.load(Ordering::SeqCst) < joblog_len_reported(&doc) {
                    continue; // more of its log to fetch first
                }
                let (state, reason) = outcome_of(&doc, stopping, &worker.name);
                let exit = if state == State::TimedOut { 124 } else { exit as i32 };
                return ended(state, exit, reason, &activity);
            }
        } else if status.stderr.contains("no run or job is named") {
            let reason = format!("worker {} has no record of its job {}", worker.name, remote.job);
            return ended(State::Failed, 1, Some(reason), &activity);
        }
        let silent = crate::sync::lock(&last_contact).elapsed();
        if silent >= LOST_AFTER {
            let reason = format!("worker lost: no answer from {} for {}", worker.name, human(silent));
            return ended(State::Failed, 1, Some(reason), &activity);
        }
        std::thread::sleep(RECONNECT_EVERY);
    }
}

/// How much output the worker's log holds, as far as its status says; 0 when unknown.
fn joblog_len_reported(doc: &serde_json::Value) -> u64 {
    doc["log_bytes"].as_u64().unwrap_or(0)
}

/// The run's end, from the worker's status of its job and what this machine asked for.
fn outcome_of(doc: &serde_json::Value, stopping: Stopping, worker: &str) -> (State, Option<String>) {
    let reported = doc["state"].as_str().unwrap_or("failed");
    let unasked = |state: State| {
        (state, Some(format!("the run was {} on {worker} without this machine asking", reported.replace('_', " "))))
    };
    match (reported, stopping) {
        ("succeeded", _) => (State::Succeeded, None),
        ("failed", _) => (State::Failed, doc["failure_reason"].as_str().map(String::from)),
        ("timed_out" | "cancelled", Stopping::TimedOut) => (State::TimedOut, None),
        ("cancelled", Stopping::Cancelled) => (State::Cancelled, None),
        ("timed_out", _) => unasked(State::TimedOut),
        ("cancelled", _) => unasked(State::Cancelled),
        ("abandoned", _) => (State::Abandoned, Some(format!("the daemon on {worker} stopped while it ran"))),
        (other, _) => (State::Failed, Some(format!("it ended {other} on {worker}"))),
    }
}

fn set_remote(shared: &Shared, rec: &Record, remote: RemoteRun) {
    let mut q = queue::lock(shared);
    if let Some(e) = q.runs.get_mut(&rec.run_id) {
        e.rec.remote_run = Some(remote);
    }
    q.persist(&rec.run_id);
}

fn current_remote(shared: &Shared, rec: &Record) -> Option<RemoteRun> {
    queue::lock(shared).runs.get(&rec.run_id).and_then(|e| e.rec.remote_run.clone())
}

/// Puts the run back in the queue: to run here only when `stays_here`, else here or on
/// a worker that is available, whichever frees first.
fn requeue(completion: &mut Completion, reason: &str, stays_here: bool) {
    completion.finished = true;
    let mut q = queue::lock(completion.shared);
    let Some(e) = q.runs.get_mut(completion.run_id) else { return };
    let rec = &mut e.rec;
    rec.state = State::Queued;
    (rec.slot, rec.started_at, rec.machine, rec.remote_run, rec.load_at_start) = (None, None, None, None, None);
    rec.remote_fallback = Some(reason.to_string());
    rec.stays_here = stays_here;
    e.live = None;
    let label = rec.job.label.clone();
    q.persist(completion.run_id);
    log::event(Tone::Warning, "back here", &label, Some(completion.run_id), &[reason.to_string()]);
}
