//! `drmanhattan status`: the slots in use and the waiters in admission order.
//!
//! Read from the state directory, so it works whether or not the daemon is running, and
//! ranked with the daemon's own rules, so the order shown is the order runs will start.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::cli::args::{self, Spec};
use crate::daemon;
use crate::error::Result;
use crate::job::{PrepareState, Record, RunId, Source, State};
use crate::paths::Paths;
use crate::queue::{rank, read_records};
use crate::settings::{effective_capacity, read_manual_order};
use crate::time::{human, Timestamp};

const STATUS: Spec = Spec {
    usage: "\
usage: drmanhattan status [<run>] [--json] [--inbox DIR]
       drmanhattan status [--session ID] [--limit N] [--json]

Shows the queue: whether the daemon is running, the slots in use, and the waiting jobs in
the order they will start. Text output is one `key=value` line per item (values with
spaces are quoted); --json prints the same as one JSON document.

--session ID shows only the runs one session or thread submitted (its source's session,
as `submit` records it), and adds its newest finished runs, at most N (default 10). The
first line, and each waiter's position, still describe the whole queue. --limit N without
--session adds the whole queue's newest N finished runs.

With <run> (a run id, its short id, or a job's name), shows that run instead: one line
run_id=… state=… job=… exit=…, or with --json its whole record as .status.json has it,
plus `state` (also `submitted` for a job file not yet picked up) and `exit` once it is over.
",
    values: &["inbox", "session", "limit"],
    switches: &["json"],
    positional: 1,
};

/// Finished runs `--session` lists when `--limit` does not say.
const DEFAULT_FINISHED: usize = 10;

#[derive(Serialize)]
struct StatusDoc<'a> {
    /// `stopped`, `idle` (nothing running), `available` (a slot is free) or `busy`.
    state: &'static str,
    daemon: Option<DaemonDoc>,
    active: usize,
    capacity: u32,
    /// Quick runs here now, and how many may run at once; they take no slot.
    quick: usize,
    quick_capacity: usize,
    /// The running daemon's version, or this command's when none runs.
    version: String,
    /// This command's version, when the daemon still runs another: it restarts into it.
    #[serde(skip_serializing_if = "Option::is_none")]
    installed: Option<&'static str>,
    auto_update: bool,
    /// What auto-update is doing, in words.
    auto_update_state: String,
    /// An update is holding new starts until the running jobs finish, then restarts.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    update_hold: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<crate::update::Available>,
    /// The session `--session` asked for; running and waiters then hold only its runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<&'a str>,
    running: Vec<RunningDoc<'a>>,
    waiters: Vec<WaiterDoc<'a>>,
    /// With `--session` or `--limit`: the newest finished runs (the session's), newest first.
    #[serde(skip_serializing_if = "Option::is_none")]
    finished: Option<Vec<FinishedDoc<'a>>>,
    /// The workers in config.json (see `drmanhattan workers`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    workers: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct DaemonDoc {
    pid: u32,
    url: Option<String>,
}

#[derive(Serialize)]
struct RunningDoc<'a> {
    slot: Option<u32>,
    /// The worker it runs on; none for this machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    machine: Option<&'a str>,
    /// A service: it takes no slot, and `ready` says whether it is ready yet.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    service: bool,
    /// A quick run: it takes no slot either; `slot` is only its `DRMANHATTAN_SLOT`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    quick: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ready: Option<bool>,
    run_id: &'a RunId,
    label: &'a str,
    category: &'a str,
    cwd: &'a PathBuf,
    started_at: Option<Timestamp>,
    elapsed_seconds: Option<u64>,
    /// The same, to the millisecond, for the text lines.
    #[serde(skip)]
    elapsed: Option<std::time::Duration>,
    expected_seconds: Option<u64>,
    log: &'a PathBuf,
    job_file: &'a PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'a Source>,
}

#[derive(Serialize)]
struct WaiterDoc<'a> {
    position: usize,
    run_id: &'a RunId,
    label: &'a str,
    category: &'a str,
    waited_seconds: u64,
    #[serde(skip)]
    waited: std::time::Duration,
    expected_seconds: Option<u64>,
    skip_after_seconds: u64,
    priority: bool,
    overdue: bool,
    manual: bool,
    /// For a job with a `prepare` command: `pending`, `running` or `succeeded`. Only a waiter
    /// without one, or whose prepare has succeeded, can take a slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    prepare: Option<PrepareState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<&'a str>,
    /// Whether it starts without waiting for a slot, and why (see `HistoryIndex::lane`).
    quick: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    lane_reason: Option<&'a str>,
    job_file: &'a PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'a Source>,
}

#[derive(Serialize)]
struct FinishedDoc<'a> {
    run_id: &'a RunId,
    label: &'a str,
    category: &'a str,
    state: &'static str,
    exit: Option<i32>,
    cwd: &'a PathBuf,
    started_at: Option<Timestamp>,
    finished_at: Option<Timestamp>,
    /// From start to finish; none for a run that never started.
    duration_seconds: Option<u64>,
    #[serde(skip)]
    duration: Option<std::time::Duration>,
    /// For a failed run: the line of its log to read first.
    #[serde(skip_serializing_if = "Option::is_none")]
    error_line: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_reason: Option<&'a str>,
    log: &'a PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'a Source>,
}

/// What `--session` narrows the document to, and how many finished runs it adds
/// (`--limit`, also without `--session`).
struct SessionFilter<'a> {
    session: Option<&'a str>,
    limit: usize,
}

impl SessionFilter<'_> {
    fn has(&self, r: &Record) -> bool {
        self.session.map_or(true, |session| r.job.source.as_ref().and_then(|s| s.session.as_deref()) == Some(session))
    }
}

pub(super) fn status(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&STATUS, rest)? else { return Ok(0) };
    if let Some(run) = a.positional.first() {
        return super::runs::status_of(&a, run);
    }
    let limit: Option<usize> = a.parsed("limit", "a number of runs")?;
    let session = match a.get("session").map(str::trim) {
        Some("") => return crate::error::invalid("--session needs a session or thread id"),
        Some(session) => Some(SessionFilter { session: Some(session), limit: limit.unwrap_or(DEFAULT_FINISHED) }),
        None => limit.map(|limit| SessionFilter { session: None, limit }),
    };
    let paths = Paths::from_env();
    let records = read_records(&paths);
    let doc = build(&paths, &records, session.as_ref())?;
    if a.has("json") {
        println!("{}", serde_json::to_string_pretty(&doc).map_err(|e| crate::error::Error::Invalid(e.to_string()))?);
    } else {
        print_text(&doc);
    }
    Ok(0)
}

fn build<'a>(paths: &Paths, records: &'a [Record], filter: Option<&SessionFilter<'a>>) -> Result<StatusDoc<'a>> {
    let mine = |r: &Record| filter.map_or(true, |f| f.has(r));
    let now = Timestamp::now();
    let daemon = daemon::running(paths);
    let capacity = effective_capacity(paths, daemon::slots_env(daemon.as_ref()).as_deref())?;
    let mut running: Vec<&Record> = records.iter().filter(|r| r.state == State::Running).collect();
    running.sort_by_key(|r| r.slot.unwrap_or(0));
    let by_id: HashMap<&RunId, &Record> = records.iter().map(|r| (&r.run_id, r)).collect();
    let ranked = rank(records.iter().filter(|r| r.state == State::Queued), &read_manual_order(paths), now);
    let here = running.iter().filter(|r| r.machine.is_none() && r.holds_slot()).count();
    let quick = running.iter().filter(|r| r.quick).count();
    let state = match &daemon {
        None => "stopped",
        Some(_) if running.is_empty() => "idle",
        Some(_) if here < capacity as usize => "available",
        Some(_) => "busy",
    };
    let waiters = ranked
        .iter()
        .filter_map(|w| by_id.get(&w.run_id).map(|r| (*r, w)))
        .enumerate()
        .filter(|(_, (r, _))| mine(r))
        .map(|(i, (r, w))| WaiterDoc {
            position: i + 1,
            run_id: &r.run_id,
            label: &r.job.label,
            category: &r.job.category,
            waited_seconds: r.queued_at.until(now).as_secs(),
            waited: r.queued_at.until(now),
            expected_seconds: r.expected_seconds,
            skip_after_seconds: r.skip_after_seconds,
            priority: r.job.priority,
            overdue: w.overdue,
            manual: w.manual,
            prepare: r.prepare_status.as_ref().map(|p| p.state),
            group: r.job.group.as_deref(),
            quick: r.quick,
            lane_reason: r.lane_reason.as_deref(),
            job_file: &r.job.job_file,
            source: r.job.source.as_ref(),
        })
        .collect();
    let finished = filter.map(|f| {
        let mut done: Vec<&Record> = records.iter().filter(|r| r.state.is_terminal() && f.has(r)).collect();
        done.sort_by_key(|r| std::cmp::Reverse(r.finished_at.unwrap_or(r.queued_at)));
        done.truncate(f.limit);
        done.into_iter().map(finished_doc).collect()
    });
    let (version, installed) = versions(daemon.as_ref());
    let daemon_quick = daemon.as_ref().and_then(|d| d.info.as_ref()).and_then(|i| i.max_quick);
    Ok(StatusDoc {
        state,
        daemon: daemon.map(|d| DaemonDoc { pid: d.pid, url: d.info.and_then(|i| i.url) }),
        active: here,
        workers: super::workers::docs(paths)?,
        capacity,
        quick,
        // The daemon's own; with none running (or one from before 0.7.0), this command's.
        quick_capacity: daemon_quick
            .unwrap_or_else(|| crate::settings::Settings::from_env().map_or(0, |s| s.max_quick)),
        version,
        installed,
        auto_update: crate::settings::load_config(paths).is_ok_and(|c| c.auto_update),
        auto_update_state: crate::update::auto_update_summary(paths),
        update_hold: crate::update::held(paths),
        update: crate::update::available(paths),
        session: filter.and_then(|f| f.session),
        finished,
        running: running
            .into_iter()
            .filter(|r| mine(r))
            .map(|r| RunningDoc {
                slot: r.slot,
                machine: r.machine.as_deref(),
                service: r.job.service,
                quick: r.quick,
                ready: r.job.service.then_some(r.ready_at.is_some()),
                run_id: &r.run_id,
                label: &r.job.label,
                category: &r.job.category,
                cwd: &r.job.cwd,
                started_at: r.started_at,
                elapsed_seconds: r.started_at.map(|s| s.until(now).as_secs()),
                elapsed: r.started_at.map(|s| s.until(now)),
                expected_seconds: r.expected_seconds,
                log: &r.job.log,
                job_file: &r.job.job_file,
                source: r.job.source.as_ref(),
            })
            .collect(),
        waiters,
    })
}

fn finished_doc(r: &Record) -> FinishedDoc<'_> {
    let duration = r.started_at.zip(r.finished_at).map(|(start, end)| start.until(end));
    FinishedDoc {
        run_id: &r.run_id,
        label: &r.job.label,
        category: &r.job.category,
        state: r.state.as_str(),
        exit: r.exit_status,
        cwd: &r.job.cwd,
        started_at: r.started_at,
        finished_at: r.finished_at,
        duration_seconds: duration.map(|d| d.as_secs()),
        duration,
        error_line: r.error_line.as_deref(),
        failure_reason: r.failure_reason.as_deref(),
        log: &r.job.log,
        source: r.job.source.as_ref(),
    }
}

/// The running daemon's version, and this command's when the daemon has yet to restart into it.
fn versions(daemon: Option<&crate::daemon::RunningDaemon>) -> (String, Option<&'static str>) {
    let ours = crate::update::current();
    match crate::update::daemon_behind(daemon) {
        Some(running) => (running, Some(ours)),
        None => (ours.into(), None),
    }
}

/// `key=value`, quoting a value a shell or a `cut`-style reader would split.
fn kv(key: &str, value: impl std::fmt::Display) -> String {
    let value = value.to_string();
    if value.is_empty() || value.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"') {
        format!("{key}={}", crate::naming::shell_words(std::slice::from_ref(&value)))
    } else {
        format!("{key}={value}")
    }
}

fn print_text(doc: &StatusDoc) {
    let mut head = vec![
        kv("state", doc.state),
        kv("active", doc.active),
        kv("capacity", doc.capacity),
        kv("quick", format!("{}/{}", doc.quick, doc.quick_capacity)),
        kv("version", &doc.version),
        kv("auto_update", if doc.auto_update { "on" } else { "off" }),
    ];
    if let Some(installed) = doc.installed {
        head.extend([kv("installed", installed), kv("running", &doc.version), kv("restart", "pending")]);
    }
    if doc.auto_update {
        head.push(kv("auto_update_state", &doc.auto_update_state));
    }
    if doc.update_hold {
        head.push(kv("hold", "update: new jobs wait until the running ones finish and it restarts"));
    }
    if let Some(d) = &doc.daemon {
        head.push(kv("daemon_pid", d.pid));
        head.extend(d.url.as_ref().map(|u| kv("dashboard", u)));
    }
    println!("{}", head.join(" "));
    if let Some(u) = &doc.update {
        println!("update available={} url={} run=`drmanhattan update`", u.version, u.url);
    }
    for r in &doc.running {
        let mut line = vec![if r.quick { kv("lane", "quick") } else { kv("slot", r.slot.unwrap_or(0)) }];
        line.extend(r.machine.map(|m| kv("machine", m)));
        if r.service {
            line.push(kv("service", if r.ready == Some(true) { "ready" } else { "starting" }));
        }
        line.extend([kv("run_id", r.run_id), kv("label", r.label), kv("category", r.category)]);
        line.extend(r.elapsed.map(|d| kv("elapsed", human(d))));
        line.extend(r.expected_seconds.map(|e| kv("expected", human(std::time::Duration::from_secs(e)))));
        line.push(kv("cwd", r.cwd.display()));
        println!("{}", line.join(" "));
    }
    for w in &doc.workers {
        let field = |k: &str| w[k].as_str().map(String::from).or_else(|| w[k].as_u64().map(|n| n.to_string()));
        let mut line = vec!["worker".to_string()];
        for k in ["name", "state", "running", "capacity", "reason"] {
            line.extend(field(k).map(|v| kv(k, v)));
        }
        println!("{}", line.join(" "));
    }
    println!("policy='priority first, then manual order, then overdue by arrival, then shortest expected first'");
    let yes = |b: bool| if b { "yes" } else { "no" };
    for w in &doc.waiters {
        let line = [
            "waiter".to_string(),
            kv("position", w.position),
            kv("run_id", w.run_id),
            kv("label", w.label),
            kv("category", w.category),
            kv("waited", human(w.waited)),
            kv(
                "expected",
                w.expected_seconds.map_or_else(|| "unknown".into(), |e| human(std::time::Duration::from_secs(e))),
            ),
            kv("priority", yes(w.priority)),
            kv("overdue", yes(w.overdue)),
            kv("manual", yes(w.manual)),
        ];
        let mut line = line.to_vec();
        line.extend(w.prepare.map(|p| kv("prepare", p.as_str())));
        line.extend(w.group.map(|g| kv("group", g)));
        line.push(kv("lane", if w.quick { "quick" } else { "slot" }));
        line.extend(w.lane_reason.map(|r| kv("reason", r)));
        println!("{}", line.join(" "));
    }
    for f in doc.finished.iter().flatten() {
        let mut line = vec!["finished".to_string(), kv("run_id", f.run_id), kv("state", f.state)];
        line.extend(f.exit.map(|e| kv("exit", e)));
        line.extend([kv("label", f.label), kv("category", f.category)]);
        line.extend(f.duration.map(|d| kv("duration", human(d))));
        line.extend(f.finished_at.map(|t| kv("ago", human(t.until(Timestamp::now())))));
        println!("{}", line.join(" "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_with_spaces_are_quoted() {
        assert_eq!(kv("label", "build"), "label=build");
        assert_eq!(kv("label", "cargo test"), "label='cargo test'");
        assert_eq!(kv("label", ""), "label=''");
    }
}
