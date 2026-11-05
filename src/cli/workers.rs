//! `drmanhattan workers`: the configured workers and what this machine last saw of them,
//! `workers add`, which checks a machine and adds it, and `workers remove`. Also
//! `drmanhattan route`, which sends jobs that may run on a worker to chosen workers only.

use std::time::Duration;

use serde::Serialize;

use crate::cli::args::{self, Spec};
use crate::error::{invalid, Error, Result};
use crate::job::State;
use crate::paths::Paths;
use crate::queue::read_records;
use crate::settings::{load_config, set_route, update_config, MAX_SLOTS};
use crate::time::Timestamp;
use crate::workers::{self, read_live, Live, WorkerConfig};

const WORKERS: Spec = Spec {
    usage: "\
usage: drmanhattan workers [--json]
       drmanhattan workers add <ssh-alias> [--name NAME] [--slots N] [--tag TAG]...
                               [--drmanhattan PATH]
       drmanhattan workers remove <name>

With no arguments, lists the workers in config.json: whether each answered, how many of
its slots this machine may use and how many it uses, and why it is not taking work, if it
is not. `--json` prints the same as one document.

`add` checks that the machine answers over SSH with its key alone, that it runs the same
drmanhattan, confirmed and running there, then adds it to config.json (or updates it).
--drmanhattan gives the command's absolute path there, when it is not on the PATH ssh
gives a command (install.sh's ~/.local/bin usually is not).

`remove` takes it out of config.json, and out of the route (`drmanhattan route`); with
nothing left on the route, the route is off and jobs run here again. Runs already on it
carry on there, followed to the end; its mirrors stay on it.
",
    values: &["name", "slots", "tag", "drmanhattan"],
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
                a.get("drmanhattan"),
            ),
            None => invalid("workers add needs the worker's ssh alias (see --help)"),
        },
        Some("remove") => match a.positional.get(1) {
            Some(name) => remove(&paths, name),
            None => invalid("workers remove needs the worker's name (see --help)"),
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

fn add(
    paths: &Paths,
    alias: &str,
    name: Option<&str>,
    slots: Option<u32>,
    tags: &[String],
    program: Option<&str>,
) -> Result<i32> {
    let config = load_config(paths)?;
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
    if let Some(p) = program {
        worker.drmanhattan = Some(p.to_string());
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
    // The probe may have taken a while: change the file as it is now, and only in what was
    // asked, so a change made meanwhile (a − or + on the dashboard) stays.
    let (existed, worker) = update_config(paths, |config| {
        let at = config.workers.iter().position(|w| w.ssh == alias || name.is_some_and(|n| w.name == n));
        let i = match at {
            Some(i) => {
                let w = &mut config.workers[i];
                w.ssh = alias.to_string();
                if let Some(n) = name {
                    w.name = n.to_string();
                }
                if let Some(s) = slots {
                    w.slots = s;
                }
                if !tags.is_empty() {
                    w.tags = tags.to_vec();
                }
                if let Some(p) = program {
                    w.drmanhattan = Some(p.to_string());
                }
                i
            }
            None => {
                config.workers.push(worker.clone());
                config.workers.len() - 1
            }
        };
        workers::validate(&mut config.workers)?;
        Ok((at.is_some(), config.workers[i].clone()))
    })?;
    let verb = if existed { "updated" } else { "added" };
    println!("{verb} worker {} (ssh {}, {} slot(s) for this machine)", worker.name, worker.ssh, worker.slots);
    Ok(0)
}

fn remove(paths: &Paths, name: &str) -> Result<i32> {
    let route = update_config(paths, |config| {
        let Some(at) = config.workers.iter().position(|w| w.name == name) else {
            return invalid(format!("no worker is named {name:?}"));
        };
        config.workers.remove(at);
        config.route.retain(|r| r != name);
        Ok(config.route.clone())
    })?;
    let _ = crate::fsutil::remove_if_present(&paths.worker_state(name));
    println!("removed worker {name}");
    let running =
        read_records(paths).iter().filter(|r| r.state == State::Running && r.machine.as_deref() == Some(name)).count();
    if running > 0 {
        println!("{running} run(s) on it carry on there until they end");
    }
    println!("{}", route_line(&route));
    Ok(0)
}

const ROUTE: Spec = Spec {
    usage: "\
usage: drmanhattan route [<worker>... | local]

With no argument, shows the route. With one or more workers, every job that may run on a
worker (`remote` allowed or prefer) or does not say (no `remote`) runs on those workers
only: it waits for a free slot on one of them, and does not take a slot here. It is kept
in config.json until changed. `local` turns it off: jobs run as they say again.

While a route is on, these still run here: jobs with `remote: never`, services, quick runs
(they start at once, and would spend longer being sent), jobs whose `requires` tags none of
the route's workers has, and runs a worker failed for a reason of the job's own. Jobs with
`remote: only` go to any worker.
",
    values: &[],
    switches: &[],
    positional: 64,
};

/// `drmanhattan route`: shows or sets the route.
pub(super) fn route(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&ROUTE, rest)? else { return Ok(0) };
    let paths = Paths::from_env();
    let route = match a.positional.as_slice() {
        [] => load_config(&paths)?.route,
        [off] if off == "local" || off == "off" => set_route(&paths, &[])?,
        names => set_route(&paths, names)?,
    };
    println!("{}", route_line(&route));
    Ok(0)
}

/// The route in one line, as `route` and `workers remove` print it.
fn route_line(route: &[String]) -> String {
    if route.is_empty() {
        "route=local: jobs run where they say, here by default".into()
    } else {
        format!("route={}: jobs that may run on a worker go only to {}", route.join(","), route.join(" or "))
    }
}
