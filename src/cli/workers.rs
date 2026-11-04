//! `drmanhattan workers`: the configured workers and what this machine last saw of them,
//! and `workers add`, which checks a machine and adds it.

use std::time::Duration;

use serde::Serialize;

use crate::cli::args::{self, Spec};
use crate::error::{invalid, Error, Result};
use crate::job::State;
use crate::paths::Paths;
use crate::queue::read_records;
use crate::settings::{load_config, save_config, MAX_SLOTS};
use crate::time::Timestamp;
use crate::workers::{self, read_live, Live, WorkerConfig};

const WORKERS: Spec = Spec {
    usage: "\
usage: drmanhattan workers [--json]
       drmanhattan workers add <ssh-alias> [--name NAME] [--slots N] [--tag TAG]...

With no arguments, lists the workers in config.json: whether each answered, how many of
its slots this machine may use and how many it uses, and why it is not taking work, if it
is not. `--json` prints the same as one document.

`add` checks that the machine answers over SSH with its key alone, that it runs the same
drmanhattan, confirmed and running there, then adds it to config.json (or updates it).
",
    values: &["name", "slots", "tag"],
    switches: &["json"],
    positional: 2,
};

#[derive(Serialize)]
struct WorkerDoc {
    name: String,
    ssh: String,
    /// `free`, `busy`, `paused` or `unavailable`.
    state: &'static str,
    online: Option<bool>,
    capacity: u32,
    slots: u32,
    running: usize,
    tags: Vec<String>,
    reason: Option<String>,
    unavailable_until: Option<Timestamp>,
    last_seen: Option<Timestamp>,
}

pub(crate) fn docs(paths: &Paths) -> Result<Vec<serde_json::Value>> {
    let config = load_config(paths)?;
    let records = read_records(paths);
    Ok(config.workers.iter().map(|w| serde_json::to_value(doc(paths, w, &records)).unwrap_or_default()).collect())
}

fn doc(paths: &Paths, w: &WorkerConfig, records: &[crate::job::Record]) -> WorkerDoc {
    let live: Live = read_live(paths, &w.name);
    let capacity = w.slots;
    let running = records.iter().filter(|r| r.state == State::Running && r.machine.as_deref() == Some(&w.name)).count();
    let unavailable = live.unavailable().map(String::from);
    let state = match () {
        _ if capacity == 0 => "paused",
        _ if unavailable.is_some() => "unavailable",
        _ if running >= capacity as usize => "busy",
        _ => "free",
    };
    WorkerDoc {
        name: w.name.clone(),
        ssh: w.ssh.clone(),
        state,
        online: live.online,
        capacity,
        slots: w.slots,
        running,
        tags: w.tags.clone(),
        reason: unavailable,
        unavailable_until: live.unavailable_until.filter(|u| *u > Timestamp::now()),
        last_seen: live.last_seen,
    }
}

pub(super) fn workers(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&WORKERS, rest)? else { return Ok(0) };
    let paths = Paths::from_env();
    match a.positional.first().map(String::as_str) {
        None => list(&paths, a.has("json")),
        Some("add") => match a.positional.get(1) {
            Some(alias) => add(
                &paths,
                alias,
                a.get("name"),
                a.parsed("slots", &format!("a number from 0 to {MAX_SLOTS}"))?,
                a.all("tag"),
            ),
            None => invalid("workers add needs the worker's ssh alias (see --help)"),
        },
        Some(other) => invalid(format!("unknown workers command: {other} (see --help)")),
    }
}

fn list(paths: &Paths, json: bool) -> Result<i32> {
    let config = load_config(paths)?;
    let records = read_records(paths);
    let docs: Vec<WorkerDoc> = config.workers.iter().map(|w| doc(paths, w, &records)).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&docs).map_err(|e| Error::Invalid(e.to_string()))?);
        return Ok(0);
    }
    if docs.is_empty() {
        println!("no workers; add one with `drmanhattan workers add <ssh-alias>`");
    }
    for d in docs {
        let online = match d.online {
            Some(true) => "online",
            Some(false) => "offline",
            None => "not-asked-yet",
        };
        let mut line = format!(
            "worker={} ssh={} state={} {online} running={} capacity={}",
            d.name, d.ssh, d.state, d.running, d.capacity
        );
        if !d.tags.is_empty() {
            line += &format!(" tags={}", d.tags.join(","));
        }
        if let Some(r) = d.reason {
            line += &format!(" reason={}", crate::naming::shell_words(&[r]));
        }
        println!("{line}");
    }
    Ok(0)
}

fn add(paths: &Paths, alias: &str, name: Option<&str>, slots: Option<u32>, tags: &[String]) -> Result<i32> {
    let mut config = load_config(paths)?;
    let existing = config.workers.iter().position(|w| w.ssh == alias || name.is_some_and(|n| w.name == n));
    let mut worker = existing.map(|i| config.workers[i].clone()).unwrap_or(WorkerConfig {
        name: String::new(),
        ssh: alias.to_string(),
        slots: 1,
        tags: Vec::new(),
        check: None,
        args_path_map: Default::default(),
        min_free_gb: None,
        mirror_days: None,
        drmanhattan: None,
    });
    worker.ssh = alias.to_string();
    if let Some(n) = name {
        worker.name = n.to_string();
    }
    if let Some(s) = slots {
        worker.slots = s;
    }
    if !tags.is_empty() {
        worker.tags = tags.to_vec();
    }
    workers::validate(std::slice::from_mut(&mut worker))?;
    let out =
        workers::ssh_run(&worker, &workers::remote_argv(&worker, &["machine-stats"]), None, Duration::from_secs(30));
    if out.code != 0 {
        return invalid(format!(
            "{alias} did not answer: {} (it needs key-based ssh, and drmanhattan on its non-interactive PATH)",
            out.first_line()
        ));
    }
    let stats = serde_json::from_str(out.stdout.trim()).map_err(|_| {
        Error::Invalid(format!("{alias}'s drmanhattan did not give its readings: {}", out.first_line()))
    })?;
    if let Some(why) = workers::unusable(&stats) {
        return invalid(format!("{alias} cannot be a worker yet: {why}"));
    }
    match existing {
        Some(i) => config.workers[i] = worker.clone(),
        None => config.workers.push(worker.clone()),
    }
    workers::validate(&mut config.workers)?;
    save_config(paths, &config)?;
    let verb = if existing.is_some() { "updated" } else { "added" };
    println!("{verb} worker {} (ssh {}, {} slot(s) for this machine)", worker.name, worker.ssh, worker.slots);
    Ok(0)
}
