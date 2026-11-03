//! `drmanhattan submit`: writes a job file into the inbox from command-line flags.
//!
//! A convenience only: the daemon never sees `submit`, it just finds the file. With
//! `--wait`, the command then follows `<job>.log` until `<job>.exit` appears and exits with
//! that code, so `manhattan submit --wait -- make test` behaves like `make test` run through
//! the queue. Ctrl-C while waiting cancels the job; a second Ctrl-C stops waiting.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::cli::args::{self, Spec};
use crate::cli::source;
use crate::daemon;
use crate::error::{invalid, IoContext, Result};
use crate::fsutil;
use crate::job::{
    is_job_name, CommandSpec, JobFiles, JobSpec, Ready, RemoteMode, RunId, SourceSpec, SyncSpec, MAX_NAME,
};
use crate::joblog;
use crate::log;
use crate::naming;
use crate::paths::{self, Paths};
use crate::signals;

/// What `submit` says when there is no daemon to run the job. Plain enough to be passed on
/// as it is, by a person or by a coding agent.
const NOT_RUNNING_NOTHING_QUEUED: &str =
    "drmanhattan isn't running, so nothing was queued; start it with `drmanhattan`";

const POLL: Duration = Duration::from_millis(200);
/// How long `submit` gives a daemon that is still starting.
const STARTING_GRACE: Duration = Duration::from_secs(2);

const SUBMIT: Spec = Spec {
    usage: "\
usage: drmanhattan submit [options] -- <command> [args...]

Queues a command by writing a job file into the inbox.

options:
  --wait              wait for the job: print its log as it runs, exit with its status
  --label TEXT        its name on the dashboard (default: the program and its first arguments)
  --category TEXT     what it is grouped with for estimates (default: the program)
  --variant TEXT      a finer grouping for estimates, such as a cold or warm build
  --timeout SECS      seconds of running time allowed
  --priority          run ahead of everything not marked priority
  --cwd DIR           where it runs (default: here)
  --env KEY=VALUE     set an environment variable (repeatable)
  --name NAME         the job file's name (default: the label and a unique id)
  --inbox DIR         the inbox (default: the running daemon's)
  --source TOOL       who is submitting, as the dashboard shows it; detected when omitted
  --session ID        the submitting session or thread, for the dashboard's link
  --source-url URL    a link to open instead
  --prepare CMD       a shell command run while the job waits, outside any slot; the job
                      runs once it succeeds, and fails if it fails
  --group NAME        at most one job of the group runs and one waits; a newer waiting
                      job replaces an older one
  --remote MODE       never (the default), allowed (on a worker when every slot here is
                      busy) or prefer (on a free worker first)
  --requires TAG      a tag the worker must have (repeatable)
  --sync PATH         a path to mirror to the worker (repeatable; default: --cwd)
  --sync-git          mirror the git metadata of synced checkouts too
  --service           a server that keeps running: submit returns once it is ready (with
                      --ready-port or --ready-log) and it takes no build slot
  --ready-port N      the service is ready once 127.0.0.1:N accepts connections
  --ready-log RE      the service is ready once a line of its output matches RE
  --ready-timeout S   seconds it may take to be ready (default 120)
  --ttl S             seconds it may run before it is stopped (default 8 hours)
  --restart           replace the service of this name if it is already running
  --fail-pattern RE   fail the job when a line of its output matches RE, even if the
                      command exits 0 (repeatable)
  --tty               run it under a pseudo-terminal (120x40, TERM=xterm-256color), for a
                      command that needs one; stdin stays at EOF
",
    values: &[
        "inbox",
        "name",
        "cwd",
        "label",
        "category",
        "timeout",
        "variant",
        "env",
        "source",
        "source-url",
        "session",
        "fail-pattern",
        "prepare",
        "group",
        "remote",
        "requires",
        "sync",
        "ready-port",
        "ready-log",
        "ready-timeout",
        "ttl",
    ],
    switches: &["priority", "wait", "sync-git", "service", "restart", "tty"],
    positional: 0,
};

pub(super) fn submit(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&SUBMIT, rest)? else { return Ok(0) };
    if a.rest.is_empty() {
        return invalid("a command is required after -- (see --help)");
    }
    let paths = Paths::from_env();
    let daemon = running_soon(&paths, a.has("wait"));
    let inbox = match (a.get("inbox"), daemon.as_ref().and_then(|d| d.info.as_ref())) {
        (Some(dir), _) => PathBuf::from(dir),
        (None, Some(info)) => info.inbox.clone(),
        (None, None) => paths::default_inbox(),
    };
    let service = a.has("service");
    if service && a.has("wait") {
        return invalid("a service never finishes: drop --wait; submit --service returns once it is ready");
    }
    if !service && ["ready-port", "ready-log", "ready-timeout", "ttl", "restart"].iter().any(|f| a.has(f)) {
        return invalid("--ready-port, --ready-log, --ready-timeout, --ttl and --restart are for --service");
    }
    // Waiting on a job nothing will run would never end: say so instead.
    if daemon.is_none() && (a.has("wait") || service) {
        return invalid(NOT_RUNNING_NOTHING_QUEUED);
    }
    if !inbox.is_dir() {
        return invalid(match daemon {
            None => NOT_RUNNING_NOTHING_QUEUED.to_string(),
            Some(_) => format!("the inbox does not exist: {}", inbox.display()),
        });
    }
    let cwd = match a.get("cwd") {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir().context("read", Path::new("the current directory"))?,
    };
    let cwd = cwd.canonicalize().context("find the working directory", &cwd)?;
    let label = a.get("label").map_or_else(|| naming::default_label(&a.rest), String::from);
    // A service's name is its label's, so that submitting it again finds the running one.
    let name = a.get("name").map_or_else(
        || if service { naming::file_safe(&label) } else { format!("{}-{}", naming::file_safe(&label), RunId::new()) },
        String::from,
    );
    if !is_job_name(&name) {
        return invalid(format!(
            "--name {name:?} cannot name a job: use a plain file name of at most {MAX_NAME} bytes, not starting with a dot or ending in .status, .json, .log, .exit or .cancel"
        ));
    }
    let mut env = std::collections::BTreeMap::new();
    for item in a.all("env") {
        let (key, value) = item
            .split_once('=')
            .ok_or_else(|| crate::error::Error::Invalid(format!("--env expects KEY=VALUE, not {item:?}")))?;
        env.insert(key.to_string(), value.to_string());
    }
    let names: Vec<String> = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect();
    let lookup = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let source = source::detect(
        a.get("source").map(String::from),
        a.get("session").map(String::from),
        a.get("source-url").map(String::from),
        &lookup,
        &names,
    );
    let spec = JobSpec {
        command: CommandSpec::Argv(a.rest.clone()),
        cwd: Some(cwd.to_string_lossy().into_owned()),
        label: Some(label),
        category: a.get("category").map(String::from),
        timeout: a.parsed("timeout", "a number of seconds")?,
        priority: a.has("priority"),
        variant: a.get("variant").map(String::from),
        env,
        source: source.map(SourceSpec::Full),
        session: None,
        activity: None,
        fail_patterns: a.all("fail-pattern").to_vec(),
        prepare: a.get("prepare").map(|p| CommandSpec::Shell(p.to_string())),
        group: a.get("group").map(String::from),
        remote: match a.get("remote") {
            None | Some("never") => None,
            Some("allowed") => Some(RemoteMode::Allowed),
            Some("prefer") => Some(RemoteMode::Prefer),
            Some(other) => return invalid(format!("--remote must be never, allowed or prefer, not {other:?}")),
        },
        requires: a.all("requires").to_vec(),
        sync: a.all("sync").iter().map(|p| SyncSpec::Path(absolute(p))).collect(),
        sync_git: a.has("sync-git"),
        tty: a.has("tty"),
        env_forward: Vec::new(),
        dispatched_from: None,
        dispatch_id: None,
        slots_hint: None,
        service,
        ready: match (a.parsed::<u16>("ready-port", "a port number")?, a.get("ready-log")) {
            (Some(_), Some(_)) => return invalid("give --ready-port or --ready-log, not both"),
            (Some(port), None) => Some(Ready::Port(port)),
            (None, Some(re)) => Some(Ready::Log(re.to_string())),
            (None, None) => None,
        },
        ready_timeout: a.parsed("ready-timeout", "a number of seconds")?,
        ttl: a.parsed("ttl", "a number of seconds")?,
    };
    let files = JobFiles::new(&inbox, &name);
    if service && live(&files) {
        if !a.has("restart") {
            // The same service, already running or starting: use it rather than start another.
            return super::service::wait_ready(&files, true);
        }
        fsutil::write_atomic(&files.cancel(), "")?;
        log::note(format_args!("stopping the running {name} to start it again"));
        while !files.exit().exists() {
            std::thread::sleep(POLL);
        }
    }
    if piped_input_waiting() {
        log::warn("jobs get no stdin, so the input piped to submit is not passed on; give it as a file or an argument");
    }
    write_job(&files, &spec)?;
    let queued = format!(
        "{}queued {} as {name} in {}",
        log::NOTE_PREFIX,
        spec.label.as_deref().unwrap_or(&name),
        naming::tilde_path(&inbox)
    );
    println!("{}", log::mark_own_line(&queued, log::stdout_colour()));
    if daemon.is_none() {
        log::warn("drmanhattan isn't running; the job waits in the inbox until it starts (`drmanhattan`)");
    }
    if service {
        super::service::wait_ready(&files, false)
    } else if a.has("wait") {
        wait(&files)
    } else {
        Ok(0)
    }
}

/// A job by this name is queued or running.
fn live(files: &JobFiles) -> bool {
    files.job().exists() || (files.claimed().exists() && !files.exit().exists())
}

/// `path` as an absolute path, from the current directory.
fn absolute(path: &str) -> String {
    let p = Path::new(path);
    if p.is_absolute() || path.starts_with("~/") {
        return path.to_string();
    }
    std::env::current_dir().map_or_else(|_| path.to_string(), |d| d.join(p).to_string_lossy().into_owned())
}

/// Whether something has already been piped into `submit`: fd 0 is a pipe with bytes
/// waiting. A terminal or `/dev/null` is never a pipe, and a pipe nothing has written to
/// yet counts as empty, so this never blocks; it is a warning's worth of certainty, not more.
fn piped_input_waiting() -> bool {
    use std::fs::File;
    use std::mem::ManuallyDrop;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::io::FromRawFd;

    // SAFETY: fd 0 is open for the life of the process; ManuallyDrop keeps it from being closed.
    let stdin = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    if !stdin.metadata().is_ok_and(|m| m.file_type().is_fifo()) {
        return false;
    }
    let mut waiting: libc::c_int = 0;
    // SAFETY: FIONREAD writes the number of unread bytes into the int it is given.
    unsafe { libc::ioctl(0, libc::FIONREAD as _, &mut waiting) == 0 && waiting > 0 }
}

/// Writes the job file, refusing a name that is already queued or running. A finished
/// job's results under the same name are removed first, so `--wait` cannot mistake them
/// for this run's (the daemon would remove them when it claims the job anyway).
fn write_job(files: &JobFiles, spec: &JobSpec) -> Result<()> {
    let live = files.job().exists() || (files.claimed().exists() && !files.exit().exists());
    if live {
        return invalid(format!("a job named {} is already queued or running", files.name()));
    }
    for old in [files.exit(), files.status(), files.log()] {
        fsutil::remove_if_present(&old)?;
    }
    let text = serde_json::to_string_pretty(spec).map_err(|e| crate::error::Error::Invalid(e.to_string()))?;
    fsutil::write_atomic(&files.job(), &format!("{text}\n"))
}

/// The parts of `<job>.status.json` that `--wait` reports.
#[derive(Deserialize)]
struct Status {
    state: String,
    failure_reason: Option<String>,
}

/// Prints the log as it grows until `.exit` appears, and returns its code.
fn wait(files: &JobFiles) -> Result<i32> {
    signals::count(&[libc::SIGINT]);
    let mut echo = Echo::new();
    let mut cancel_sent = false;
    loop {
        let finished = files.exit().exists();
        echo.copy_new(&files.log());
        if finished {
            echo.finish();
            let code = std::fs::read_to_string(files.exit()).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(1);
            if let Some(Status { state, failure_reason: Some(reason) }) = fsutil::read_json(&files.status()) {
                log::note(format_args!("{}: {reason}", state.replace('_', " ")));
            }
            return Ok(code);
        }
        match signals::received() {
            0 => {}
            1 if !cancel_sent => {
                cancel_sent = true;
                fsutil::write_atomic(&files.cancel(), "")?;
                log::note("cancelling the job; press Ctrl-C again to stop waiting");
            }
            1 => {}
            _ => return Ok(130),
        }
        std::thread::sleep(POLL);
    }
}

/// The running daemon, once it has said which inbox it reads. A daemon still starting is
/// given a moment to: it takes its lock before it writes that. With `absent_too`, so is
/// one not yet started (as by `drmanhattan &` just before).
fn running_soon(paths: &Paths, absent_too: bool) -> Option<daemon::RunningDaemon> {
    let until = std::time::Instant::now() + STARTING_GRACE;
    loop {
        let daemon = daemon::running(paths);
        let settled = match &daemon {
            Some(d) => d.info.is_some(),
            None => !absent_too,
        };
        if settled || std::time::Instant::now() >= until {
            return daemon;
        }
        std::thread::sleep(POLL);
    }
}

/// Copies a job's log to stdout as it grows. On a terminal, drmanhattan's own lines are set
/// apart from the command's output (`log::mark_own_line`); everything else is passed on as
/// soon as it is read, a progress bar's unfinished line included.
struct Echo {
    offset: u64,
    /// Read but not yet written: the start of a line that may be one of drmanhattan's.
    held: Vec<u8>,
    /// The start of the current line was written already, so it is the command's.
    mid_line: bool,
    colour: bool,
}

impl Echo {
    fn new() -> Echo {
        Echo { offset: 0, held: Vec::new(), mid_line: false, colour: log::stdout_colour() }
    }

    fn copy_new(&mut self, log: &Path) {
        let mut out = std::io::stdout().lock();
        while let Some(gained) = joblog::read_from(log, self.offset, 1024 * 1024) {
            if gained.bytes.is_empty() {
                break;
            }
            self.offset = gained.offset;
            if self.colour {
                self.held.extend_from_slice(&gained.bytes);
                self.write_lines(&mut out);
            } else {
                let _ = out.write_all(&gained.bytes);
            }
        }
        let _ = out.flush();
    }

    /// Writes what is held, up to a line start that may yet become one of drmanhattan's.
    fn write_lines(&mut self, out: &mut impl Write) {
        let prefix = log::NOTE_PREFIX.as_bytes();
        loop {
            let newline = self.held.iter().position(|b| *b == b'\n');
            if self.mid_line || !(self.held.starts_with(prefix) || prefix.starts_with(&self.held)) {
                // The command's: up to the end of the line, or all there is.
                let end = newline.map_or(self.held.len(), |i| i + 1);
                let _ = out.write_all(&self.held[..end]);
                self.mid_line = newline.is_none();
                self.held.drain(..end);
            } else if let Some(i) = newline {
                let line = String::from_utf8_lossy(&self.held[..i]).into_owned();
                let _ = writeln!(out, "{}", log::mark_own_line(&line, true));
                self.held.drain(..=i);
            } else {
                return; // perhaps the start of drmanhattan's line: wait for the rest
            }
            if self.held.is_empty() {
                return;
            }
        }
    }

    /// Writes out anything still held, once the job has finished.
    fn finish(&mut self) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&std::mem::take(&mut self.held));
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `submit --wait` writes for each read of the log, on a terminal.
    fn echo(reads: &[&str]) -> Vec<String> {
        let mut echo = Echo { offset: 0, held: Vec::new(), mid_line: false, colour: true };
        reads
            .iter()
            .map(|read| {
                let mut out = Vec::new();
                echo.held.extend_from_slice(read.as_bytes());
                echo.write_lines(&mut out);
                String::from_utf8(out).unwrap()
            })
            .collect()
    }

    #[test]
    fn drmanhattans_lines_are_set_apart_and_the_commands_pass_straight_through() {
        let own = |rest: &str| format!("{}\n", log::mark_own_line(&format!("{}{rest}", log::NOTE_PREFIX), true));
        assert_eq!(
            echo(&["drmanhattan · started\ncompiling 40%\r", "compiling 80%\rdone\n", "drmanhattan · succeeded\n"]),
            [own("started") + "compiling 40%\r", "compiling 80%\rdone\n".into(), own("succeeded")],
            "a progress line is not held back"
        );
        assert_eq!(
            echo(&["drman", "hattan · failed\n"]),
            ["".to_string(), own("failed")],
            "a split prefix is waited for"
        );
        assert_eq!(echo(&["say drmanhattan · hi\n"]), ["say drmanhattan · hi\n"], "only at the start of a line");
    }
}
