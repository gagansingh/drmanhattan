//! The daemon: one process that watches the inbox, schedules runs and serves the dashboard.
//!
//! `serve` sets up, recovers what the previous daemon left and then runs the scheduler
//! loop: one `Scheduler::pass` per wake-up, woken by a file-system event in the inbox or the
//! poll tick, until a signal ends it (`shutdown`).

mod lock;
mod pass;
mod setup;
mod shutdown;
mod stop;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::Watcher;

pub(crate) use lock::{running, slots_env, DaemonInfo, RunningDaemon, ShellEnvReload};
pub(crate) use setup::open_in_browser;
pub(crate) use stop::stop;

use crate::activity::Patterns;
use crate::error::{Error, Result};
use crate::fsutil;
use crate::job::{classify, InboxFile, State};
use crate::log;
use crate::naming::tilde_path;
use crate::paths::Paths;
use crate::queue::{self, prune_files, Queue, Shared, HEARTBEAT_EVERY};
use crate::runner;
use crate::settings::{load_config, standing_capacity, Settings};
use crate::signals;
use crate::sysinfo;
use crate::time::{count, human, Timestamp};
use crate::workers;
use pass::Scheduler;

const PRUNE_EVERY: Duration = Duration::from_secs(3600);

pub(crate) struct ServeOptions {
    pub(crate) inbox: PathBuf,
    pub(crate) bind: String,
    pub(crate) port: u16,
    /// Whether the user chose the port: a busy default port falls back to a free one, a
    /// busy chosen port is an error.
    pub(crate) port_explicit: bool,
    pub(crate) dashboard: bool,
    pub(crate) open_browser: bool,
    pub(crate) session_links: bool,
}

pub(crate) fn serve(paths: Paths, settings: Settings, opts: ServeOptions) -> Result<()> {
    // First, before anything is published: a `drmanhattan stop` or Ctrl-C that arrives the
    // moment dashboard.json appears must reach our handler, not the default action, which
    // would end the process without recording anything.
    shutdown::install_signal_handlers();
    setup::prepare_dirs(&paths)?;
    let Some(_lock) = lock::take(&paths)? else {
        // Not an error: someone started it twice. Say where the running one is.
        let running = lock::running(&paths);
        println!("{}", setup::already_running_message(running.as_ref(), &opts.inbox));
        if let (true, Some(url)) = (opts.open_browser, running.and_then(|r| r.info).and_then(|i| i.url)) {
            open_in_browser(&url);
        }
        return Ok(());
    };
    // Bad settings are reported now, not as a warning on every pass.
    standing_capacity(&paths, settings.slots_env.as_deref())?;
    let patterns = Patterns::compile(&load_config(&paths)?.activity.unwrap_or_default())
        .map_err(|e| Error::Invalid(format!("{} has invalid activity patterns: {e}", paths.config().display())))?;
    let inbox = setup::prepare_inbox(&opts.inbox)?;
    crate::settings::remove_timed_overrides(&paths);

    let mut queue = Queue::new(paths.clone(), settings.clone(), inbox.clone(), patterns);
    let recovery = queue.recover();
    let live = queue.prune_memory();
    prune_files(&paths, &inbox, &live, &settings);
    sysinfo::start_sampler();
    // Before any job starts, so every one gets the same base environment.
    let shell_env = crate::shell_env::init(settings.shell_env, settings.shell_env_timeout);
    let reattached = queue.reattach();
    let shared: Shared = Arc::new(Mutex::new(queue));
    for admitted in reattached {
        start_runner(&shared, admitted);
    }
    workers::start_poller(paths.clone(), settings.worker_poll);
    crate::update::start_checker(paths.clone());
    crate::update::start_auto_updater(paths.clone());
    crate::update::announce(&paths);

    let mut info = DaemonInfo {
        pid: std::process::id(),
        inbox: inbox.clone(),
        started_at: Timestamp::now(),
        url: None,
        slots_env: settings.slots_env.clone(),
        max_quick: Some(settings.max_quick),
        shell_env: Some(shell_env.summary.clone()),
        shell_env_read_at: shell_env.read_at,
        shell_env_reload: None,
        version: Some(env!("CARGO_PKG_VERSION").to_string()),
        protocol: Some(crate::workers::PROTOCOL),
    };
    if opts.dashboard {
        info.url = Some(setup::start_dashboard(&shared, &opts)?);
    }
    fsutil::write_json(&paths.daemon_info(), &info)?;
    let info = Arc::new(Mutex::new(info));
    let (slots, _) = standing_capacity(&paths, settings.slots_env.as_deref())?;
    // Two lines: where to look and what it does. The state and data directories are left to
    // `drmanhattan paths`; recovery is mentioned only when it did something.
    let mut lines = vec![
        format!(
            "drmanhattan {} running · dashboard {}",
            env!("CARGO_PKG_VERSION"),
            crate::sync::lock(&info).url.as_deref().unwrap_or("off")
        ),
        format!("watching {} · {}", tilde_path(&inbox), count(slots as usize, "slot", "slots")),
    ];
    if !recovery.is_quiet() {
        lines.push(format!("recovered: {recovery}"));
    }
    log::banner(&lines);

    // A watcher wakes the loop when a job or cancel file appears; the tick is the fallback
    // that also admits waiters as runs finish.
    let (wake_tx, wake) = mpsc::channel::<()>();
    let _watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok_and(|ev| ev.paths.iter().any(|p| wakes_scheduler(p))) {
            let _ = wake_tx.send(());
        }
    })
    .and_then(|mut w| w.watch(&inbox, notify::RecursiveMode::NonRecursive).map(|()| w));
    if let Err(e) = &_watcher {
        log::problem(format_args!("cannot watch the inbox ({e}); checking it every {:?} instead", settings.poll));
    }

    run_loop(&shared, Scheduler::new(paths.clone(), inbox, settings.slots_env.clone()), &settings, &wake, &info);
    let queued = queue::lock(&shared).count(State::Queued);
    let staying = if queued > 0 {
        count(queued, "waiting job runs", "waiting jobs run") + " on the next start"
    } else {
        String::new()
    };
    log::notice(log::Tone::Quiet, "stopped", staying);
    let _ = std::fs::remove_file(paths.daemon_info());
    Ok(())
}

/// `drmanhattan reload-env` asked (SIGUSR1): reads the login shell's environment again for
/// the jobs that start afterwards, and records how it went in `dashboard.json`, where the
/// command that asked reads it. When the shell fails now, jobs keep the snapshot they had.
fn reload_shell_env(paths: &Paths, timeout: Duration, info: &Mutex<DaemonInfo>) {
    let result = crate::shell_env::reload(timeout);
    let mut info = crate::sync::lock(info);
    let error = match &result {
        Ok(env) => {
            log::notice(log::Tone::Quiet, "reloaded", format_args!("{}: {} variables", env.summary, env.vars.len()));
            info.shell_env = Some(env.summary.clone());
            info.shell_env_read_at = env.read_at;
            None
        }
        Err(why) => {
            let kept = info.shell_env_read_at.map(|t| format!(" read at {}", t.local_clock())).unwrap_or_default();
            let why = format!("{why}; jobs keep the environment{kept}");
            log::problem(&why);
            Some(why)
        }
    };
    info.shell_env_reload = Some(ShellEnvReload { at: Timestamp::now(), error });
    if let Err(e) = fsutil::write_json(&paths.daemon_info(), &*info) {
        log::problem(format_args!("cannot record the reload in {}: {e}", paths.daemon_info().display()));
    }
}

/// Only job and cancel files wake the scheduler: logs and status files change constantly
/// while jobs run.
fn wakes_scheduler(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    matches!(classify(name), Some((InboxFile::Job | InboxFile::Cancel, _)))
}

fn run_loop(
    shared: &Shared,
    mut scheduler: Scheduler,
    settings: &Settings,
    wake: &Receiver<()>,
    info: &Arc<Mutex<DaemonInfo>>,
) {
    let mut last_prune = Instant::now();
    let mut last_heartbeat: Option<Instant> = None;
    let mut stopping: Option<Instant> = None;
    let mut reloads_seen = 0;
    let mut reloading: Option<std::thread::JoinHandle<()>> = None;
    loop {
        let signals = shutdown::signals();
        // One reload at a time; a request that arrives during one runs after it.
        let reloads = signals::reloads_received();
        if reloads > reloads_seen && reloading.as_ref().map_or(true, |t| t.is_finished()) {
            let (paths, timeout, info) = (scheduler.paths.clone(), settings.shell_env_timeout, info.clone());
            let thread = std::thread::Builder::new().name("reload-env".into());
            match thread.spawn(move || reload_shell_env(&paths, timeout, &info)) {
                Ok(handle) => {
                    reloads_seen = reloads;
                    reloading = Some(handle);
                }
                // Not counted as seen: the next pass tries again.
                Err(e) => log::problem(format_args!("could not start a thread to re-read the environment: {e}")),
            }
        }
        if signals >= 2 {
            log::notice(log::Tone::Warning, "stopping", "now: cancelling the running jobs");
            shutdown::force_stop(shared, settings);
            return;
        }
        if signals == 1 && stopping.is_none() {
            stopping = Some(Instant::now());
            shutdown::announce(shared, settings);
            shutdown::stop_services(shared);
        }
        for admitted in scheduler.pass(shared, stopping.is_none()) {
            start_runner(shared, admitted);
        }
        if let Some(since) = stopping {
            // Runs on workers carry on there; the next start follows them again.
            let running = queue::lock(shared).records(State::Running).filter(|r| r.machine.is_none()).count();
            if running == 0 {
                return;
            }
            if since.elapsed() >= settings.shutdown_grace {
                let still = count(running, "job still running", "jobs still running");
                log::notice(
                    log::Tone::Warning,
                    "stopping",
                    format_args!("{still} after {}; cancelling", human(settings.shutdown_grace)),
                );
                shutdown::force_stop(shared, settings);
                return;
            }
        }
        if last_heartbeat.map_or(true, |t| t.elapsed() >= HEARTBEAT_EVERY) {
            last_heartbeat = Some(Instant::now());
            let _ = fsutil::write_atomic(&scheduler.paths.heartbeat(), &format!("{}\n", Timestamp::now()));
        }
        if last_prune.elapsed() >= PRUNE_EVERY {
            last_prune = Instant::now();
            let live = queue::lock(shared).prune_memory();
            prune_files(&scheduler.paths, &scheduler.inbox, &live, settings);
        }
        // Wait for a file event or the tick, then drain any burst of events.
        // Never longer than the heartbeat interval, which recovery relies on.
        let _ = wake.recv_timeout(settings.poll.min(HEARTBEAT_EVERY));
        while wake.try_recv().is_ok() {}
    }
}

fn start_runner(shared: &Shared, admitted: queue::Admitted) {
    let run_id = admitted.record.run_id.clone();
    let thread_shared = shared.clone();
    let preparing = admitted.record.state == State::Queued;
    let spawned = std::thread::Builder::new().name(format!("run-{run_id}")).spawn(move || {
        if preparing {
            runner::run_prepare(&thread_shared, admitted)
        } else {
            runner::run_job(&thread_shared, admitted)
        }
    });
    if let Err(e) = spawned {
        queue::lock(shared).fail_to_start(&run_id, &format!("could not start a thread to run it: {e}"));
    }
}
