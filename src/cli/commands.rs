//! The small commands: `serve`, `stop`, `slots` and `paths`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::acknowledge;
use crate::cli::args::{self, Spec};
use crate::daemon::{self, ServeOptions};
use crate::error::{invalid, Result};
use crate::log;
use crate::paths::{self, Paths};
use crate::process;
use crate::settings::{load_config, read_override, save_config, standing_capacity, Settings, MAX_SLOTS};

/// An uncommon port, so the dashboard does not collide with the usual development servers.
pub(crate) const DEFAULT_PORT: u16 = 8417;

const SERVE: Spec = Spec {
    usage: "\
usage: drmanhattan [serve] [options]

Watches the inbox, runs jobs and serves the dashboard, until Ctrl-C or `drmanhattan stop`.

options:
  --inbox DIR         the watched folder (default ~/.drmanhattan/inbox, or $DRMANHATTAN_INBOX)
  --port N            the dashboard's port (default 8417; a free port is used if that one is busy)
  --bind ADDR         the dashboard's address (default 127.0.0.1)
  --no-dashboard      run the queue without the web page
  --open              open the dashboard in the browser
  --no-session-links  do not read Claude Code or Codex session files for the dashboard's links
  --yes               confirm the one-time question asked before the first start
                      (for services, which have no terminal; DRMANHATTAN_ACCEPT=1 does the same)
",
    values: &["inbox", "port", "bind"],
    switches: &["no-dashboard", "open", "no-session-links", "yes"],
    positional: 0,
};

pub(super) fn serve(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&SERVE, rest)? else { return Ok(0) };
    let port: Option<u16> = a.parsed("port", "a port number")?;
    let opts = ServeOptions {
        inbox: a.get("inbox").map_or_else(paths::default_inbox, PathBuf::from),
        bind: a.get("bind").unwrap_or("127.0.0.1").to_string(),
        port: port.unwrap_or(DEFAULT_PORT),
        port_explicit: port.is_some(),
        dashboard: !a.has("no-dashboard"),
        open_browser: a.has("open"),
        session_links: !a.has("no-session-links"),
    };
    // From here on, what the daemon prints, its start-up errors included, is also kept in its
    // log file: a service has no terminal to show them.
    let paths = Paths::from_env();
    if crate::fsutil::ensure_private_dir(&paths.data).is_ok() {
        log::log_to_file(&paths.daemon_log());
    }
    if !crate::dashboard::is_loopback(&opts.bind) {
        log::warn("the dashboard is reachable beyond this machine: anyone who can reach it can read your jobs' logs, though they cannot change anything");
    }
    let settings = Settings::from_env()?;
    if !acknowledge::confirm(&paths, &opts.inbox, a.has("yes"))? {
        return Ok(0);
    }
    daemon::serve(paths, settings, opts)?;
    Ok(0)
}

const UPDATE: Spec = Spec {
    usage: "\
usage: drmanhattan update [--now]
       drmanhattan update --auto on|off

Installs the latest release over this one and restarts the daemon. It first waits until no
job is running or queued (services aside; they are restarted with it); --now does not wait,
and running jobs end when the daemon restarts. The archive's SHA-256 is checked before
anything is replaced; a failed download or checksum leaves this version in place.

--auto on lets the daemon install newer releases of this major version by itself, once the
queue has been idle for 5 minutes; --auto off (the default) only shows the notice.
",
    values: &["auto"],
    switches: &["now"],
    positional: 0,
};

pub(super) fn update(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&UPDATE, rest)? else { return Ok(0) };
    let on = match a.get("auto") {
        None => return crate::update::update(a.has("now")),
        Some("on") => true,
        Some("off") => false,
        Some(other) => return invalid(format!("--auto must be on or off, not {other:?}")),
    };
    let paths = Paths::from_env();
    let mut config = load_config(&paths)?;
    config.auto_update = on;
    save_config(&paths, &config)?;
    println!(
        "auto-update {}",
        if on { "on: newer releases of this major version install once the queue is idle" } else { "off" }
    );
    Ok(0)
}

const STOP: Spec = Spec {
    usage: "\
usage: drmanhattan stop [<name>]

With no name, stops the running daemon. Running jobs get the shutdown grace period
($DRMANHATTAN_SHUTDOWN_GRACE_SECONDS, default 30) to finish and are then cancelled;
services are stopped at once; queued jobs run on the next start.

With a name (a service's, or any run's: its run id, short id or job name), stops that one,
its whole process group, and waits until it has ended.
",
    values: &[],
    switches: &[],
    positional: 1,
};

pub(super) fn stop(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&STOP, rest)? else { return Ok(0) };
    if let Some(name) = a.positional.first() {
        super::runs::cancel(std::slice::from_ref(name))?;
        let code = super::runs::wait(std::slice::from_ref(name))?;
        return Ok(if code == 2 { 2 } else { 0 });
    }
    daemon::stop(&Paths::from_env(), &Settings::from_env()?)?;
    Ok(0)
}

const RELOAD_ENV: Spec = Spec {
    usage: "\
usage: drmanhattan reload-env

Asks the running daemon to read your login shell's environment again, as it did when it
started; jobs that start afterwards get it, running jobs and services keep theirs. If the
shell fails or does not answer, jobs keep the environment they had and the command says so.
",
    values: &[],
    switches: &[],
    positional: 0,
};

/// Sends SIGUSR1 to the daemon holding the lock, then waits for it to record the outcome
/// in `dashboard.json`. A daemon that would not understand the signal is never sent it.
pub(super) fn reload_env(rest: &[String]) -> Result<i32> {
    if args::parse(&RELOAD_ENV, rest)?.is_none() {
        return Ok(0);
    }
    let paths = Paths::from_env();
    let settings = Settings::from_env()?;
    let Some(daemon) = daemon::running(&paths) else {
        return invalid("drmanhattan is not running");
    };
    // The daemon publishes its info once it has read the environment the first time.
    let Some(info) = daemon.info.as_ref() else {
        return invalid("drmanhattan is still starting; try again in a moment");
    };
    if info.shell_env.as_deref() == Some("off") {
        return invalid(
            "the daemon runs with DRMANHATTAN_SHELL_ENV=0, so jobs get its own environment; nothing to read",
        );
    }
    if info.shell_env_read_at.is_none() {
        let version = info.version.as_deref().unwrap_or("before 0.6.2");
        return invalid(format!(
            "the running daemon ({version}) predates reload-env; `drmanhattan update`, or `drmanhattan stop` and a new start, reads the environment afresh"
        ));
    }
    let before = info.shell_env_reload.clone();
    if !process::signal_process(daemon.pid as i32, libc::SIGUSR1) {
        return invalid(format!("could not signal drmanhattan (pid {})", daemon.pid));
    }
    let deadline = Instant::now() + settings.shell_env_timeout + settings.poll + Duration::from_secs(5);
    loop {
        let info = daemon::running(&paths).filter(|d| d.pid == daemon.pid).and_then(|d| d.info);
        // A record other than the one seen before signalling is this request's answer.
        if let Some(reload) =
            info.as_ref().and_then(|i| i.shell_env_reload.as_ref()).filter(|r| Some(*r) != before.as_ref())
        {
            return match &reload.error {
                None => {
                    let summary = info.as_ref().and_then(|i| i.shell_env.as_deref()).unwrap_or_default();
                    println!("read {summary} again; jobs that start from now get it");
                    Ok(0)
                }
                Some(why) => invalid(why),
            };
        }
        if Instant::now() >= deadline {
            return invalid("drmanhattan has not answered; see its log (`drmanhattan paths` shows where)");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

const SLOTS: Spec = Spec {
    usage: "\
usage: drmanhattan slots [N]

With no argument, shows how many jobs may run at once and where that number comes from.
With N (1 to 16), sets the standing count in config.json; the daemon picks it up at once.
$DRMANHATTAN_SLOTS, when set, overrides config.json; the dashboard's - and + buttons
override both for four hours.
",
    values: &[],
    switches: &[],
    positional: 1,
};

pub(super) fn slots(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&SLOTS, rest)? else { return Ok(0) };
    let paths = Paths::from_env();
    let Some(n) = a.positional.first() else {
        let slots_env = daemon::slots_env(daemon::running(&paths).as_ref());
        let (standing, source) = standing_capacity(&paths, slots_env.as_deref())?;
        let over = read_override(&paths);
        let mut line = format!("capacity={} standing={standing} source={source}", over.map_or(standing, |o| o.slots));
        if let Some(o) = over {
            line += &format!(" override={} until={}", o.slots, o.until);
        }
        println!("{line}");
        return Ok(0);
    };
    let n: u32 = match n.parse() {
        Ok(n) if (1..=MAX_SLOTS).contains(&n) => n,
        _ => return invalid(format!("slots must be a number from 1 to {MAX_SLOTS}, not {n:?}")),
    };
    let mut config = load_config(&paths)?;
    config.slots = Some(n);
    save_config(&paths, &config)?;
    let plural = if n == 1 { "" } else { "s" };
    let note = if daemon::slots_env(daemon::running(&paths).as_ref()).is_some() {
        " (DRMANHATTAN_SLOTS, set for the daemon, still overrides it)"
    } else {
        ""
    };
    println!("standing capacity set to {n} slot{plural}{note}");
    Ok(0)
}

const PATHS: Spec = Spec {
    usage: "\
usage: drmanhattan paths

Prints where the inbox, the state directory and the data directory are, and the running
daemon's pid and dashboard when there is one.
",
    values: &[],
    switches: &[],
    positional: 0,
};

pub(super) fn paths(rest: &[String]) -> Result<i32> {
    if args::parse(&PATHS, rest)?.is_none() {
        return Ok(0);
    }
    let paths = Paths::from_env();
    let daemon = daemon::running(&paths);
    let info = daemon.as_ref().and_then(|d| d.info.as_ref());
    let inbox = info.map_or_else(paths::default_inbox, |i| i.inbox.clone());
    println!(
        "inbox={}\nstate={}\ndata={}\nlog={}",
        inbox.display(),
        paths.state.display(),
        paths.data.display(),
        paths.daemon_log().display()
    );
    if let Some(d) = &daemon {
        let url = info.and_then(|i| i.url.as_deref()).map(|u| format!(" dashboard={u}")).unwrap_or_default();
        let read_at = info.and_then(|i| i.shell_env_read_at).map(|t| format!(" · read {}", t.local_clock()));
        let shell_env = info
            .and_then(|i| i.shell_env.as_deref())
            .map(|s| format!(" shell_env={:?}", format!("{s}{}", read_at.unwrap_or_default())))
            .unwrap_or_default();
        println!("daemon=pid {}{url}{shell_env}", d.pid);
    }
    Ok(0)
}
