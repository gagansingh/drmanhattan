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
use crate::job::{PrepareState, Record, RunId, State};
use crate::paths::Paths;
use crate::queue::{rank, read_records};
use crate::settings::{effective_capacity, read_manual_order, read_override, Override};
use crate::time::{format_elapsed, Timestamp};

const STATUS: Spec = Spec {
    usage: "\
usage: drmanhattan status [<run>] [--json] [--inbox DIR]

Shows the queue: whether the daemon is running, the slots in use, and the waiting jobs in
the order they will start. Text output is one `key=value` line per item (values with
spaces are quoted); --json prints the same as one JSON document.

With <run> (a run id, its short id, or a job's name), shows that run instead: one line
run_id=… state=… job=… exit=…, or with --json its whole record as .status.json has it,
plus `state` (also `submitted` for a job file not yet picked up) and `exit` once it is over.
",
    values: &["inbox"],
    switches: &["json"],
    positional: 1,
};

#[derive(Serialize)]
struct StatusDoc<'a> {
    /// `stopped`, `idle` (nothing running), `available` (a slot is free) or `busy`.
    state: &'static str,
    daemon: Option<DaemonDoc>,
    active: usize,
    capacity: u32,
    version: &'static str,
    auto_update: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<crate::update::Available>,
    #[serde(rename = "override")]
    capacity_override: Option<Override>,
    running: Vec<RunningDoc<'a>>,
    waiters: Vec<WaiterDoc<'a>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    ready: Option<bool>,
    run_id: &'a RunId,
    label: &'a str,
    category: &'a str,
    cwd: &'a PathBuf,
    started_at: Option<Timestamp>,
    elapsed_seconds: Option<u64>,
    expected_seconds: Option<u64>,
    log: &'a PathBuf,
    job_file: &'a PathBuf,
}

#[derive(Serialize)]
struct WaiterDoc<'a> {
    position: usize,
    run_id: &'a RunId,
    label: &'a str,
    category: &'a str,
    waited_seconds: u64,
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
    job_file: &'a PathBuf,
}

pub(super) fn status(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&STATUS, rest)? else { return Ok(0) };
    if let Some(run) = a.positional.first() {
        return super::runs::status_of(&a, run);
    }
    let paths = Paths::from_env();
    let records = read_records(&paths);
    let doc = build(&paths, &records)?;
    if a.has("json") {
        println!("{}", serde_json::to_string_pretty(&doc).map_err(|e| crate::error::Error::Invalid(e.to_string()))?);
    } else {
        print_text(&doc);
    }
    Ok(0)
}

fn build<'a>(paths: &Paths, records: &'a [Record]) -> Result<StatusDoc<'a>> {
    let now = Timestamp::now();
    let daemon = daemon::running(paths);
    let capacity = effective_capacity(paths, daemon::slots_env(daemon.as_ref()).as_deref())?;
    let mut running: Vec<&Record> = records.iter().filter(|r| r.state == State::Running).collect();
    running.sort_by_key(|r| r.slot.unwrap_or(0));
    let by_id: HashMap<&RunId, &Record> = records.iter().map(|r| (&r.run_id, r)).collect();
    let ranked = rank(records.iter().filter(|r| r.state == State::Queued), &read_manual_order(paths), now);
    let here = running.iter().filter(|r| r.machine.is_none() && !r.job.service).count();
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
        .map(|(i, (r, w))| WaiterDoc {
            position: i + 1,
            run_id: &r.run_id,
            label: &r.job.label,
            category: &r.job.category,
            waited_seconds: r.queued_at.until(now).as_secs(),
            expected_seconds: r.expected_seconds,
            skip_after_seconds: r.skip_after_seconds,
            priority: r.job.priority,
            overdue: w.overdue,
            manual: w.manual,
            prepare: r.prepare_status.as_ref().map(|p| p.state),
            group: r.job.group.as_deref(),
            job_file: &r.job.job_file,
        })
        .collect();
    Ok(StatusDoc {
        state,
        daemon: daemon.map(|d| DaemonDoc { pid: d.pid, url: d.info.and_then(|i| i.url) }),
        active: here,
        workers: super::workers::docs(paths)?,
        capacity,
        version: crate::update::current(),
        auto_update: crate::settings::load_config(paths).is_ok_and(|c| c.auto_update),
        update: crate::update::available(paths),
        capacity_override: read_override(paths),
        running: running
            .into_iter()
            .map(|r| RunningDoc {
                slot: r.slot,
                machine: r.machine.as_deref(),
                service: r.job.service,
                ready: r.job.service.then_some(r.ready_at.is_some()),
                run_id: &r.run_id,
                label: &r.job.label,
                category: &r.job.category,
                cwd: &r.job.cwd,
                started_at: r.started_at,
                elapsed_seconds: r.started_at.map(|s| s.until(now).as_secs()),
                expected_seconds: r.expected_seconds,
                log: &r.job.log,
                job_file: &r.job.job_file,
            })
            .collect(),
        waiters,
    })
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
        kv("version", doc.version),
        kv("auto_update", if doc.auto_update { "on" } else { "off" }),
    ];
    if let Some(o) = &doc.capacity_override {
        head.extend([kv("override", o.slots), kv("override_until", o.until)]);
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
        let mut line = vec![kv("slot", r.slot.unwrap_or(0))];
        line.extend(r.machine.map(|m| kv("machine", m)));
        if r.service {
            line.push(kv("service", if r.ready == Some(true) { "ready" } else { "starting" }));
        }
        line.extend([kv("run_id", r.run_id), kv("label", r.label), kv("category", r.category)]);
        line.extend(r.elapsed_seconds.map(|s| kv("elapsed", format_elapsed(std::time::Duration::from_secs(s)))));
        line.extend(r.expected_seconds.map(|e| kv("expected", format!("{e}s"))));
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
            kv("waited", format!("{}s", w.waited_seconds)),
            kv("expected", w.expected_seconds.map_or_else(|| "unknown".into(), |e| format!("{e}s"))),
            kv("priority", yes(w.priority)),
            kv("overdue", yes(w.overdue)),
            kv("manual", yes(w.manual)),
        ];
        let mut line = line.to_vec();
        line.extend(w.prepare.map(|p| kv("prepare", p.as_str())));
        line.extend(w.group.map(|g| kv("group", g)));
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
