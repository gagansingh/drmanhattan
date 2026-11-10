//! Commands about one run: `wait`, `follow`, `cancel` and `status <run>`.
//!
//! A run is named by its run id, its short id (the last part, as the daemon's log shows it
//! after `#`), or its job's name. Each reads the run's record when there is one, and the
//! files beside its job otherwise, so they work whether or not the daemon is running, and
//! on a job file the daemon has not picked up yet.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cli::args::{self, Args, Spec};
use crate::daemon;
use crate::error::{invalid, Result};
use crate::fsutil;
use crate::job::{is_job_name, JobFiles, Record, RunId};
use crate::joblog;
use crate::paths::{self, Paths};
use crate::queue::{read_record, read_records};

const POLL: Duration = Duration::from_millis(200);
/// `wait --timeout` ran out.
const WAIT_TIMED_OUT: i32 = 124;

/// The run a command is about.
struct Target {
    files: JobFiles,
    /// None for a job file not yet picked up; learnt once it is.
    run_id: Option<RunId>,
}

/// Where a run is now.
struct Seen {
    /// A run state, `submitted` (a job file not yet picked up) or `rejected`.
    state: String,
    /// Set once it is over.
    exit: Option<i32>,
    started: bool,
    /// The run's record as `.status.json` shows it, when there is one.
    doc: Value,
}

impl Seen {
    fn line(&self, target: &Target) -> String {
        let mut parts = vec![format!("state={}", self.state)];
        if let Some(id) = &target.run_id {
            parts.insert(0, format!("run_id={id}"));
        }
        parts.push(format!("job={}", target.files.name()));
        parts.extend(self.exit.map(|e| format!("exit={e}")));
        parts.join(" ")
    }
}

fn inbox(a: &Args, paths: &Paths) -> PathBuf {
    if let Some(dir) = a.get("inbox") {
        return PathBuf::from(dir);
    }
    daemon::running(paths).and_then(|d| d.info).map_or_else(paths::default_inbox, |i| i.inbox)
}

/// The run `what` names. A run id or short id must name a known run; a job name may also
/// name a job file waiting in the inbox.
fn find(paths: &Paths, inbox: &std::path::Path, what: &str) -> Result<Target> {
    let records = read_records(paths);
    let target = |r: &Record| Target { files: r.job.files(), run_id: Some(r.run_id.clone()) };
    if let Some(r) = what.parse::<RunId>().ok().and_then(|id| records.iter().find(|r| r.run_id == id)) {
        return Ok(target(r));
    }
    let short = what.trim_start_matches('#');
    let by_short: Vec<&Record> =
        records.iter().filter(|r| r.run_id.as_str().rsplit('-').next() == Some(short)).collect();
    match by_short.as_slice() {
        [r] => return Ok(target(r)),
        [_, _, ..] => return invalid(format!("{what} names more than one run; give the whole run id")),
        [] => {}
    }
    if !is_job_name(what) {
        return invalid(format!("no run or job is named {what}"));
    }
    let files = JobFiles::new(inbox, what);
    // A job file waiting to be picked up is newer than any run by its name.
    if files.job().exists() {
        return Ok(Target { files, run_id: None });
    }
    let newest = records.iter().filter(|r| r.job.files().name() == what).max_by_key(|r| (r.queued_at, &r.run_id));
    if let Some(r) = newest {
        return Ok(target(r));
    }
    if files.status().exists() || files.exit().exists() {
        return Ok(Target { files, run_id: None });
    }
    invalid(format!("no run or job is named {what} (in {})", inbox.display()))
}

/// Where the run is now: its record, else the files beside its job.
fn look(paths: &Paths, target: &mut Target) -> Seen {
    let status: Option<Value> = fsutil::read_json(&target.files.status());
    if target.run_id.is_none() {
        // Picked up since: the status file names the run.
        target.run_id = status.as_ref().and_then(|s| s["run_id"].as_str()).and_then(|id| id.parse().ok());
    }
    if let Some(id) = &target.run_id {
        let record = read_record(paths, id).or_else(|| {
            // Pruned from the state directory: the status file, if it is still this run's.
            let s = status.as_ref().filter(|s| s["run_id"].as_str() == Some(id.as_str()))?;
            serde_json::from_value(s.clone()).ok()
        });
        if let Some(r) = record {
            return Seen {
                state: r.state.to_string(),
                exit: r.state.is_terminal().then(|| r.exit_code()),
                started: r.started_at.is_some(),
                doc: serde_json::to_value(r.public()).unwrap_or_default(),
            };
        }
    }
    if let Some(s) = status {
        // A job file that never became a run: rejected, or lost by a previous daemon.
        let exit = std::fs::read_to_string(target.files.exit()).ok().and_then(|t| t.trim().parse().ok());
        let state = s["state"].as_str().unwrap_or("unknown").to_string();
        return Seen { state, exit, started: false, doc: s };
    }
    let state = if target.files.job().exists() { "submitted" } else { "unknown" };
    Seen { state: state.into(), exit: None, started: false, doc: Value::Null }
}

const WAIT: Spec = Spec {
    usage: "\
usage: drmanhattan wait <run> [--until started|finished] [--timeout SECS] [--inbox DIR]

Waits until the run starts or finishes (the default), then prints one line:
run_id=… state=… job=… exit=…

<run> is a run id, its short id (the last part) or the job's name.
Exit status: with --until finished, the run's own exit code. With --until started, 0 once
it has started, or its exit code if it ended without starting (cancelled, rejected or
replaced). 124 when --timeout ran out first.
",
    values: &["until", "timeout", "inbox"],
    switches: &[],
    positional: 1,
};

pub(super) fn wait(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&WAIT, rest)? else { return Ok(0) };
    let started = match a.get("until").unwrap_or("finished") {
        "started" => true,
        "finished" => false,
        other => return invalid(format!("--until must be started or finished, not {other:?}")),
    };
    let timeout: Option<f64> = a.parsed("timeout", "a number of seconds")?;
    let deadline = timeout.map(|t| Instant::now() + Duration::from_secs_f64(t.max(0.0)));
    let paths = Paths::from_env();
    let mut target = find(&paths, &inbox(&a, &paths), required(&a, "wait")?)?;
    loop {
        let seen = look(&paths, &mut target);
        let reached = seen.exit.is_some() || (started && seen.started);
        if reached {
            println!("{}", seen.line(&target));
            return Ok(match (started, seen.started) {
                (true, true) => 0,
                _ => seen.exit.unwrap_or(0),
            });
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            println!("{}", seen.line(&target));
            crate::log::error(format_args!("still {} after {}s", seen.state, timeout.unwrap_or(0.0)));
            return Ok(WAIT_TIMED_OUT);
        }
        std::thread::sleep(POLL);
    }
}

const FOLLOW: Spec = Spec {
    usage: "\
usage: drmanhattan follow <run> [--from BYTES] [--inbox DIR]

Prints the run's log as it grows, from its start or from BYTES bytes of output in, and
exits with the run's exit code once it has finished and all its output is printed. The
output is the log's bytes, unchanged, so a reader that loses its place can carry on with
--from set to the bytes it has.
",
    values: &["from", "inbox"],
    switches: &[],
    positional: 1,
};

pub(super) fn follow(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&FOLLOW, rest)? else { return Ok(0) };
    let mut offset: u64 = a.parsed("from", "a number of bytes")?.unwrap_or(0);
    let paths = Paths::from_env();
    let mut target = find(&paths, &inbox(&a, &paths), required(&a, "follow")?)?;
    let mut out = std::io::stdout().lock();
    loop {
        // Looked at before reading, so a run that ends meanwhile has its last output read.
        let seen = look(&paths, &mut target);
        while let Some(gained) = joblog::read_from(&target.files.log(), offset, 1024 * 1024) {
            if gained.bytes.is_empty() {
                break;
            }
            offset = gained.offset;
            if out.write_all(&gained.bytes).and_then(|()| out.flush()).is_err() {
                return Ok(141); // the reader went away
            }
        }
        if let Some(exit) = seen.exit {
            return Ok(exit);
        }
        std::thread::sleep(POLL);
    }
}

const CANCEL: Spec = Spec {
    usage: "\
usage: drmanhattan cancel <run> [--inbox DIR]

Cancels a queued or running run (as a <job>.cancel file does). A queued run ends at once;
a running one is stopped, its whole process group, and ends once that is gone. Prints the
run's state; a run already over is left as it is.
",
    values: &["inbox"],
    switches: &[],
    positional: 1,
};

pub(super) fn cancel(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&CANCEL, rest)? else { return Ok(0) };
    let paths = Paths::from_env();
    let mut target = find(&paths, &inbox(&a, &paths), required(&a, "cancel")?)?;
    let seen = look(&paths, &mut target);
    if seen.exit.is_none() {
        fsutil::write_atomic(&target.files.cancel(), "")?;
        println!("{} cancel=requested", seen.line(&target));
    } else {
        println!("{} cancel=already-over", seen.line(&target));
    }
    Ok(0)
}

/// `status <run>`: the run's record, as `.status.json` holds it.
pub(super) fn status_of(a: &Args, what: &str) -> Result<i32> {
    let paths = Paths::from_env();
    let mut target = find(&paths, &inbox(a, &paths), what)?;
    let seen = look(&paths, &mut target);
    if a.has("json") {
        let mut doc = match seen.doc {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        doc.insert("state".into(), Value::from(seen.state.as_str()));
        doc.entry("job").or_insert_with(|| Value::from(target.files.name()));
        doc.insert("exit".into(), seen.exit.map_or(Value::Null, Value::from));
        doc.insert("log_bytes".into(), Value::from(joblog::output_len(&target.files.log())));
        let text = serde_json::to_string_pretty(&doc).map_err(|e| crate::error::Error::Invalid(e.to_string()))?;
        println!("{text}");
    } else {
        println!("{}", seen.line(&target));
    }
    Ok(0)
}

fn required<'a>(a: &'a Args, command: &str) -> Result<&'a str> {
    match a.positional.first() {
        Some(what) => Ok(what),
        None => invalid(format!("{command} needs a run: its run id, short id or job name (see --help)")),
    }
}
