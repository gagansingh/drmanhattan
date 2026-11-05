//! The `drmanhattan` (alias `manhattan`) command.

mod args;
mod commands;
mod runs;
mod service;
mod source;
mod status;
mod submit;
mod workers;

use crate::log;

const HELP: &str = "\
drmanhattan: a local job queue for heavy commands.

usage:
  drmanhattan [serve] [options]           watch the inbox, run jobs, serve the dashboard (the default)
  drmanhattan submit [options] -- <command> [args...]
                                          queue a command (--wait: run it through the queue and wait)
  drmanhattan status [<run>] [--json]     slots, running jobs, and waiters in admission order;
                                          with a run, that run's state and record
  drmanhattan wait <run> [--until started|finished]
                                          wait for a run to start or finish
  drmanhattan follow <run> [--from BYTES] print a run's log as it grows, until it finishes
  drmanhattan cancel <run>                cancel a queued or running run
  drmanhattan workers [add <ssh-alias>]   the machines that run jobs for this one, over SSH
  drmanhattan machine-stats               this machine's readings, as JSON, for a dispatcher
  drmanhattan update [--now]              install the latest release and restart the daemon
  drmanhattan reload-env                  read your login shell's environment again for later jobs
  drmanhattan stop                        stop the daemon (running jobs get a grace period)
  drmanhattan slots [N]                   show or set how many jobs may run at once
  drmanhattan sharing [on|off]            whether other machines may send this one jobs
  drmanhattan lan [on|off|new-key]        the dashboard on this network, for a phone
  drmanhattan paths                       where the inbox, state and data live
  drmanhattan --version | --help

`drmanhattan <command> --help` describes a command's options. Jobs are JSON files in the inbox;
docs/REFERENCE.md describes the format and the result files written beside them.
";

/// Runs the command line and returns the process exit status.
pub fn run_cli() -> i32 {
    // An internal error should read as one line, not a Rust backtrace.
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let message =
            payload.downcast_ref::<&str>().copied().or_else(|| payload.downcast_ref::<String>().map(String::as_str));
        let at = info.location().map(ToString::to_string).unwrap_or_default();
        log::error(format_args!("internal error: {} ({at})", message.unwrap_or("panic")));
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&args) {
        Ok(code) => code,
        Err(e) => {
            log::error(e);
            2
        }
    }
}

/// Picks the subcommand. No subcommand, or only options, means `serve`, so a bare
/// `drmanhattan` in a launchd or systemd unit is the daemon.
fn dispatch(args: &[String]) -> crate::error::Result<i32> {
    let (command, rest) = match args.first().map(String::as_str) {
        None => ("serve", args),
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            return Ok(0);
        }
        Some("-V" | "--version" | "version") => {
            println!("drmanhattan {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        Some(flag) if flag.starts_with('-') => ("serve", args),
        Some(command) => (command, &args[1..]),
    };
    match command {
        "serve" => commands::serve(rest),
        // Started by the daemon beside each job; not for people (see runner::guard).
        crate::runner::GUARD_SUBCOMMAND => Ok(crate::runner::run_guard(rest)),
        "submit" => submit::submit(rest),
        "status" => status::status(rest),
        "wait" => runs::wait(rest),
        "follow" => runs::follow(rest),
        "cancel" => runs::cancel(rest),
        "workers" => workers::workers(rest),
        "update" => commands::update(rest),
        "machine-stats" => crate::workers::machine_stats(rest),
        // What a dispatcher runs on a worker; not for people.
        "__dispatch-probe" => crate::workers::dispatch_probe(rest),
        "__dispatch-submit" => crate::workers::dispatch_submit(rest),
        "reload-env" => commands::reload_env(rest),
        "stop" => commands::stop(rest),
        "slots" => commands::slots(rest),
        "sharing" => commands::sharing(rest),
        "lan" => commands::lan(rest),
        "paths" => commands::paths(rest),
        other => crate::error::invalid(format!("unknown command: {other} (see --help)")),
    }
}
