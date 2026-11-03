//! Handing a job to a worker, up to the moment it starts there.
//!
//! 1. Reach and check: its drmanhattan's version, its acknowledgement and daemon, that it
//!    holds fewer of its own slots than this machine may use, its free disk after
//!    housekeeping (`__dispatch-probe`), and the worker's `check`.
//! 2. Mirror: git metadata with `sync_git`, then each `sync` path (`mirror`).
//! 3. Submit: the job file, written into its inbox (`__dispatch-submit`).
//! 4. Wait, up to two minutes, for it to start there.
//!
//! Any failure returns the job to the local queue. A failure of the worker's (it did not
//! answer, another version, too little disk, its `check`) also keeps it out of admission for
//! a while; a failure of the job's own (a path that cannot be mirrored) does not.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::fsutil;
use crate::job::{CommandSpec, Job, JobSpec, Record, SourceSpec, SyncSpec};
use crate::paths::Paths;
use crate::settings::worker_capacity;
use crate::workers::host::{hostname, MachineStats, ProbeRequest, PROTOCOL};
use crate::workers::{mirror, ssh, WorkerConfig};

/// How long a dispatched job may take to start on the worker.
pub(crate) const START_WAIT: Duration = Duration::from_secs(120);
/// The worker's own timeout is the job's plus this, in case this machine is gone.
const BACKSTOP: u64 = 120;
const PROBE_LIMIT: Duration = Duration::from_secs(60);

/// Why a dispatch did not happen.
pub(crate) struct Failure {
    pub(crate) reason: String,
    /// The worker's fault, not the job's: keep it out of admission for a while.
    pub(crate) worker_level: bool,
}

fn worker_fails(reason: impl Into<String>) -> Failure {
    Failure { reason: reason.into(), worker_level: true }
}

fn job_fails(reason: impl Into<String>) -> Failure {
    Failure { reason: reason.into(), worker_level: false }
}

/// A job started on a worker: its name in the worker's inbox and the worker's run id.
pub(crate) struct Started {
    pub(crate) job: String,
    pub(crate) run_id: Option<String>,
}

/// The name a dispatched job has in the worker's inbox.
pub(crate) fn remote_name(rec: &Record) -> String {
    format!("dispatch-{}", rec.run_id)
}

/// Checks that a machine's `machine-stats` make it a usable worker; the reason if not.
pub(crate) fn unusable(stats: &MachineStats) -> Option<String> {
    let ours = env!("CARGO_PKG_VERSION");
    if stats.protocol != PROTOCOL || stats.version != ours {
        return Some(format!("it runs drmanhattan {}, this machine {ours}; they must match", stats.version));
    }
    if !stats.acknowledged {
        return Some("its drmanhattan has not been started and confirmed there yet".into());
    }
    if !stats.daemon {
        return Some("its drmanhattan is not running".into());
    }
    None
}

/// Hands `rec` to `worker` and waits for it to start there. `cancelled` is asked between
/// steps; `say` writes a line into the job's log; `on_submit` is told the job's name on the
/// worker just before it is written there, so the run's record can say where it went.
pub(crate) fn dispatch(
    worker: &WorkerConfig,
    rec: &Record,
    paths: &Paths,
    say: &dyn Fn(&str),
    cancelled: &dyn Fn() -> bool,
    on_submit: &dyn Fn(&str),
) -> Result<Started, Failure> {
    let job = &rec.job;
    let entries = mirror::entries(job).map_err(job_fails)?;
    let me = hostname();

    // 1. Reach and check, after housekeeping.
    let synced_file = paths.worker_synced(&worker.name);
    let mut synced: Vec<PathBuf> = std::fs::read_to_string(&synced_file)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect();
    let prune: Vec<PathBuf> = synced.iter().filter(|p| !p.exists()).cloned().collect();
    let request = ProbeRequest {
        dispatcher: me.clone(),
        mark: entries.iter().map(|e| e.path.clone()).collect(),
        prune: prune.clone(),
        mirror_days: worker.mirror_days(),
    };
    let request = serde_json::to_string(&request).unwrap_or_default();
    let out = ssh::run(worker, &ssh::drmanhattan(worker, &["__dispatch-probe"]), Some(&request), PROBE_LIMIT);
    if out.code != 0 {
        return Err(worker_fails(format!("unreachable or refused: {}", out.first_line())));
    }
    let stats: MachineStats = serde_json::from_str(out.stdout.trim())
        .map_err(|_| worker_fails(format!("its drmanhattan did not answer the probe: {}", out.first_line())))?;
    synced.retain(|p| !prune.contains(p));
    if !stats.pruned.is_empty() {
        say(&format!("removed {} old mirror(s) on {}", stats.pruned.len(), worker.name));
    }
    if let Some(why) = unusable(&stats) {
        return Err(worker_fails(why));
    }
    let granted = worker_capacity(paths, worker);
    if stats.running >= granted as usize {
        return Err(worker_fails(format!("busy: {} job(s) running there", stats.running)));
    }
    if let Some(free) = stats.free_gb.filter(|f| *f < worker.min_free_gb()) {
        return Err(worker_fails(format!("{free} GB free, under the {} GB it must keep", worker.min_free_gb())));
    }
    if let Some(check) = &worker.check {
        let out = ssh::run(worker, &["/bin/sh".into(), "-c".into(), check.clone()], None, PROBE_LIMIT);
        if out.code != 0 {
            return Err(worker_fails(format!("its check failed: {}", out.first_line())));
        }
    }
    if cancelled() {
        return Err(job_fails("cancelled"));
    }

    // 2. Mirror.
    let commons: Vec<PathBuf> = if job.sync_git {
        let mut c: Vec<PathBuf> = entries.iter().filter_map(|e| mirror::git_common_dir(&e.path)).collect();
        c.sort();
        c.dedup();
        c
    } else {
        Vec::new()
    };
    let mut parents: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
    parents.extend(commons.iter().cloned());
    mirror::make_parents(worker, &parents).map_err(worker_fails)?;
    for common in &commons {
        let checkout = entries.iter().find(|e| mirror::git_common_dir(&e.path).as_ref() == Some(common));
        mirror::mirror_git(worker, checkout.map_or(common, |e| &e.path)).map_err(job_fails)?;
        if cancelled() {
            return Err(job_fails("cancelled"));
        }
    }
    for entry in &entries {
        say(&format!("mirroring {} to {}", entry.path.display(), worker.name));
        mirror::mirror(worker, entry).map_err(job_fails)?;
        if cancelled() {
            return Err(job_fails("cancelled"));
        }
    }
    for entry in &entries {
        if !synced.contains(&entry.path) {
            synced.push(entry.path.clone());
        }
    }
    let list: String = synced.iter().map(|p| format!("{}\n", p.display())).collect();
    let _ = synced_file.parent().map(fsutil::ensure_private_dir);
    let _ = fsutil::write_atomic(&synced_file, &list);

    // 3. Submit.
    let name = remote_name(rec);
    let spec = remote_spec(worker, rec, &me, granted);
    let text = serde_json::to_string(&spec).unwrap_or_default();
    on_submit(&name);
    let out =
        ssh::run(worker, &ssh::drmanhattan(worker, &["__dispatch-submit", "--name", &name]), Some(&text), PROBE_LIMIT);
    if out.code != 0 {
        return Err(worker_fails(format!("it did not take the job: {}", out.first_line())));
    }

    // 4. Wait for it to start.
    let limit = START_WAIT.as_secs().to_string();
    let wait = ssh::drmanhattan(worker, &["wait", &name, "--until", "started", "--timeout", &limit]);
    let out = ssh::run(worker, &wait, None, START_WAIT + Duration::from_secs(30));
    let field =
        |key: &str| out.stdout.split_whitespace().find_map(|w| w.strip_prefix(&format!("{key}="))).map(String::from);
    match out.code {
        0 => Ok(Started { job: name, run_id: field("run_id") }),
        124 | 255 => {
            let _ = ssh::run(worker, &ssh::drmanhattan(worker, &["cancel", &name]), None, PROBE_LIMIT);
            Err(worker_fails(format!("the job did not start there within {}s", START_WAIT.as_secs())))
        }
        code => Err(job_fails(format!(
            "the job ended there before it started ({}, exit {code})",
            field("state").unwrap_or_else(|| "unknown".into())
        ))),
    }
}

/// The job as the worker gets it: the same command (with the worker's path map), working
/// directory, label, category and variant; the job's environment plus what `env_forward`
/// picks from this daemon's; a timeout with a backstop; and where it came from.
fn remote_spec(worker: &WorkerConfig, rec: &Record, me: &str, granted: u32) -> JobSpec {
    let job: &Job = &rec.job;
    JobSpec {
        command: CommandSpec::Argv(worker.map_args(&job.command)),
        cwd: Some(job.cwd.to_string_lossy().into_owned()),
        label: Some(job.label.clone()),
        category: Some(job.category.clone()),
        timeout: job.timeout.map(|t| (t + BACKSTOP) as f64),
        priority: job.priority,
        variant: job.variant.clone(),
        // The jobs' shell environment, when captured, is what `env_forward` picks from.
        env: forwarded_env(job, std::env::vars().chain(crate::shell_env::vars().iter().cloned())),
        source: job.source.clone().map(SourceSpec::Full),
        session: None,
        activity: job.activity.clone(),
        fail_patterns: job.fail_patterns.clone(),
        prepare: job.prepare.clone().map(CommandSpec::Argv),
        group: None,
        remote: None,
        requires: Vec::new(),
        sync: job.sync.iter().map(|s| SyncSpec::Path(s.path.clone())).collect(),
        sync_git: false,
        env_forward: Vec::new(),
        dispatched_from: Some(me.to_string()),
        dispatch_id: Some(rec.run_id.to_string()),
        slots_hint: Some(granted.max(1)),
        service: false,
        ready: None,
        ready_timeout: None,
        ttl: None,
        tty: job.tty,
    }
}

/// The job's `env`, plus the variables of `daemon_env` that `env_forward` names (`NAME`, or
/// `PREFIX*`). The job's own values win.
pub(crate) fn forwarded_env(job: &Job, daemon_env: impl Iterator<Item = (String, String)>) -> BTreeMap<String, String> {
    let wanted = |name: &str| {
        job.env_forward.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name == p,
        })
    };
    let mut env: BTreeMap<String, String> = daemon_env.filter(|(k, _)| wanted(k)).collect();
    env.extend(job.env.clone());
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobFiles;

    #[test]
    fn env_forward_adds_named_daemon_variables_under_the_jobs_own() {
        let inbox = std::env::temp_dir();
        let spec = r#"{"command": ["true"], "env": {"APP_MODE": "job"}, "env_forward": ["APP_*", "TOKEN"]}"#;
        let job = JobSpec::parse(spec).unwrap().resolve(&JobFiles::new(&inbox, "x"), &inbox).unwrap();
        let daemon = [("APP_MODE", "daemon"), ("APP_HOST", "h"), ("TOKEN", "t"), ("TOKENS", "no"), ("PATH", "/bin")];
        let env = forwarded_env(&job, daemon.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let got: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(got, [("APP_HOST", "h"), ("APP_MODE", "job"), ("TOKEN", "t")]);
    }
}
