//! Running one admitted job, on a thread of its own.
//!
//! The command's stdout and stderr are the log file itself (two handles on one append-mode
//! file), so there is no copying thread and every line is written exactly once. The child
//! gets its own process group, which is what lets a cancel or timeout take down everything
//! it started, not just the shell at the top. A helper thread blocks in `wait()` and
//! reports the exit on the run's channel, where cancel requests also arrive; `supervise`
//! sleeps on that channel until the next thing it has to do.
//!
//! Whatever happens, the run ends recorded: a runner that panics is caught by `Guard`,
//! which ends the process group and records the run failed.

mod error_line;
mod fail_patterns;
mod guard;
mod remote;
mod supervise;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

pub(crate) use error_line::last_error_line;
use guard::GroupGuard;
pub(crate) use guard::{run as run_guard, SUBCOMMAND as GUARD_SUBCOMMAND};
use supervise::{supervise, Outcome, Watch};

use crate::activity::{Patterns, OWN_LINE_PREFIX};
use crate::fsutil;
use crate::history::HistoryRow;
use crate::job::{LoadStats, PrepareState, Record, RunId, State};
use crate::log::{self, Tone};
use crate::naming::{shell_words, tilde_path};
use crate::process;
use crate::queue::{self, Admitted, RunEvent, Shared};
use crate::time::{human, Timestamp};

/// Which of a job's commands a runner runs.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Its `prepare` command, while it is queued.
    Prepare,
    /// The job itself, in a slot.
    Run,
}

/// Runs `admitted` to completion on the calling thread.
pub(crate) fn run_job(shared: &Shared, admitted: Admitted) {
    execute(shared, admitted, Phase::Run);
}

/// Runs the `prepare` command of the queued run `admitted`. When it succeeds the run waits
/// for a slot as before; otherwise the run ends with it.
pub(crate) fn run_prepare(shared: &Shared, admitted: Admitted) {
    execute(shared, admitted, Phase::Prepare);
}

fn execute(shared: &Shared, admitted: Admitted, phase: Phase) {
    if phase == Phase::Run && admitted.record.machine.is_some() {
        return remote::run(shared, admitted);
    }
    let Admitted { record: rec, events, exit_sender, activity, default_patterns } = admitted;
    let argv = match phase {
        Phase::Run => rec.job.command.clone(),
        Phase::Prepare => rec.job.prepare.clone().unwrap_or_else(|| vec!["true".into()]),
    };
    // Compiled here rather than under the queue lock; they were validated when the job was
    // queued, so this falls back to the defaults only for a record from an older version.
    let patterns = match &rec.job.activity {
        Some(config) => Patterns::compile(config).map_or(default_patterns, Arc::new),
        None => default_patterns,
    };
    let mut completion =
        Completion { shared, run_id: &rec.run_id, phase, process_group: None, group_guard: None, finished: false };
    let (paths, settings) = {
        let q = queue::lock(shared);
        (q.paths.clone(), q.settings.clone())
    };
    // After a prepare command, or a worker that could not take it, the run's output follows
    // what is already in the log.
    let after_prepare = phase == Phase::Run && (rec.prepare_status.is_some() || rec.remote_fallback.is_some());
    let log = match open_log(&rec, after_prepare) {
        Ok(log) => log,
        Err(e) => return completion.finish(State::Failed, 126, Some(e.to_string()), Duration::ZERO),
    };
    let log_from = if after_prepare { crate::joblog::output_len(&rec.job.log) } else { 0 };
    let say = |text: &str| {
        let _ = (&log).write_all(format!("{OWN_LINE_PREFIX}{text}\n").as_bytes());
    };
    let (now, run_id) = (Timestamp::now().local_time(), &rec.run_id);
    say(&match phase {
        Phase::Run => format!("{} started in slot {} at {now} · run {run_id}", rec.job.label, rec.slot.unwrap_or(0)),
        Phase::Prepare => format!("preparing {} at {now} · run {run_id}", rec.job.label),
    });
    say(&format!("in {}: {}", tilde_path(&rec.job.cwd), shell_words(&argv)));
    let mut child = match spawn(&rec, &argv, &log, phase) {
        Ok(child) => child,
        Err(e) => {
            let code = if e.kind() == std::io::ErrorKind::NotFound { 127 } else { 126 };
            let what = if phase == Phase::Prepare { "its prepare command " } else { "" };
            let reason = format!("could not start {what}{}: {e}", argv[0]);
            say(&reason);
            say(&format!("failed, exit {code}"));
            return completion.finish(State::Failed, code, Some(reason), Duration::ZERO);
        }
    };
    let pgid = child.id() as i32;
    completion.process_group = Some(pgid);
    completion.group_guard = GroupGuard::start(pgid, settings.term_grace);
    {
        let mut q = queue::lock(shared);
        if let Some(e) = q.runs.get_mut(&rec.run_id) {
            e.rec.process_group = Some(pgid);
        }
        q.persist(&rec.run_id);
    }
    std::thread::spawn(move || {
        let status = child.wait().ok();
        let _ = exit_sender.send(RunEvent::Exited(status));
    });
    let typicals = queue::lock(shared).typicals.clone();
    let watch = Watch {
        rec: &rec,
        events: &events,
        activity: &activity,
        patterns: &patterns,
        typicals: &typicals,
        paths: &paths,
        settings: &settings,
        log_from,
    };
    let mut outcome = supervise(&watch, pgid, &say);
    completion.process_group = None; // supervise ended the group
    if let Some(group_guard) = completion.group_guard.take() {
        group_guard.stand_down();
    }
    if outcome.state == State::Succeeded && phase == Phase::Run {
        apply_fail_patterns(&rec, &mut outcome, &say);
    }
    let took = if matches!(outcome.state, State::Succeeded | State::Failed) { "in" } else { "after" };
    let (word, duration) = (outcome.state.as_str().replace('_', " "), human(outcome.duration));
    say(&match (phase, outcome.state) {
        (Phase::Prepare, State::Succeeded) => format!("prepared in {duration}; waiting for a slot"),
        (Phase::Prepare, _) => format!("prepare {word} {took} {duration}, exit {}", outcome.exit),
        (Phase::Run, _) => format!("{word} {took} {duration}, exit {}", outcome.exit),
    });
    drop(log);
    match phase {
        Phase::Run => completion.complete(&rec, &outcome),
        Phase::Prepare => completion.prepared(&rec, &outcome),
    }
}

/// Fails a run that exited 0 when its log matches one of the job's `fail_patterns`: exit
/// 1, the matching line as its error line.
fn apply_fail_patterns(rec: &Record, outcome: &mut Outcome, say: &dyn Fn(&str)) {
    // Checked when the job was queued, so this only fails for a record from an older version.
    let Ok(patterns) = crate::job::compile_fail_patterns(&rec.job.fail_patterns) else { return };
    let Some((line, pattern)) = fail_patterns::first_match(&rec.job.log, &patterns) else { return };
    say(&format!("the command exited 0, but its output matched the fail pattern {pattern:?}: {line}"));
    outcome.state = State::Failed;
    outcome.exit = 1;
    outcome.failure_reason = Some(format!("its output matched the fail pattern {pattern:?}"));
    outcome.error_line = Some(line);
}

/// The log, created fresh: the claim removed any earlier one, and refusing to open an
/// existing path means a symlink planted there cannot redirect the output. A run after its
/// `prepare` command appends to the log that command wrote, still never through a symlink.
fn open_log(rec: &Record, append: bool) -> crate::error::Result<File> {
    use crate::error::IoContext;
    let path = &rec.job.log;
    let mut options = OpenOptions::new();
    options.append(true).mode(0o600).custom_flags(libc::O_NOFOLLOW);
    if append && fsutil::is_own_file(path) {
        return options.open(path).context("open the log", path);
    }
    fsutil::remove_if_present(path)?;
    options.create_new(true).open(path).context("create the log", path)
}

fn spawn(rec: &Record, argv: &[String], log: &File, phase: Phase) -> std::io::Result<std::process::Child> {
    let job = &rec.job;
    let mut command = Command::new(&argv[0]);
    match phase {
        Phase::Prepare => command.env("DRMANHATTAN_PREPARE", "1"),
        Phase::Run => command.env("DRMANHATTAN_SLOT", rec.slot.unwrap_or(0).to_string()),
    };
    command
        .args(&argv[1..])
        .current_dir(&job.cwd)
        .envs(&job.env)
        .env("DRMANHATTAN_RUN_ID", rec.run_id.as_str())
        .env("DRMANHATTAN_LOG", &job.log)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log.try_clone()?))
        .process_group(0)
        .spawn()
}

/// Makes sure a run is recorded as ended exactly once, even if its runner panics.
struct Completion<'a> {
    shared: &'a Shared,
    run_id: &'a RunId,
    phase: Phase,
    /// While the command may still be running: its process group, to end on a panic, and
    /// the guard that ends it if the daemon dies.
    process_group: Option<i32>,
    group_guard: Option<GroupGuard>,
    finished: bool,
}

impl Completion<'_> {
    /// Records a run that ran: its load, its error line, its timing history.
    fn complete(&mut self, rec: &Record, outcome: &Outcome) {
        let error_line = match &outcome.error_line {
            Some(line) => Some(line.clone()),
            None if outcome.state.is_failure() => last_error_line(&rec.job.log),
            None => None,
        };
        let load = LoadStats::of(&outcome.load_samples);
        {
            let mut q = queue::lock(self.shared);
            if let Some(e) = q.runs.get_mut(self.run_id) {
                e.rec.load = load;
                e.rec.error_line = error_line;
                for note in &outcome.notes {
                    if !e.rec.notes.contains(note) && e.rec.notes.len() < crate::activity::MAX_NOTES {
                        e.rec.notes.push(note.clone());
                    }
                }
            }
            q.record_history(HistoryRow {
                finished_at: Timestamp::now(),
                category: rec.job.category.clone(),
                label: rec.job.label.clone(),
                variant: rec.job.variant.clone().unwrap_or_default(),
                seconds: outcome.duration.as_secs(),
                exit_status: outcome.exit,
                outcome: outcome.state,
                load: load.map(|l| l.average),
                machine: rec.machine.clone(),
            });
        }
        self.finish(outcome.state, outcome.exit, outcome.failure_reason.clone(), outcome.duration);
    }

    /// Records a prepare command that ran: the run goes on waiting for a slot, or, when the
    /// command did not succeed, ends with it.
    fn prepared(&mut self, rec: &Record, outcome: &Outcome) {
        if outcome.state != State::Succeeded {
            let reason = match outcome.state {
                State::TimedOut => "its prepare command timed out".to_string(),
                State::Cancelled => "cancelled while its prepare command ran".to_string(),
                _ => format!("its prepare command failed, exit {}", outcome.exit),
            };
            let error_line = if outcome.state.is_failure() { last_error_line(&rec.job.log) } else { None };
            // Cancelled before it started, as far as the job is concerned.
            let exit = if outcome.state == State::Cancelled { 130 } else { outcome.exit };
            let replaced = {
                let mut q = queue::lock(self.shared);
                let e = q.runs.get_mut(self.run_id);
                e.is_some_and(|e| {
                    e.rec.error_line = error_line;
                    e.rec.notes = outcome.notes.clone();
                    e.rec.superseded_by.is_some()
                })
            };
            // A run replaced in its group already says so.
            let reason = (!replaced).then_some(reason);
            return self.finish(outcome.state, exit, reason, outcome.duration);
        }
        self.finished = true;
        let mut q = queue::lock(self.shared);
        let Some(e) = q.runs.get_mut(self.run_id) else { return };
        if let Some(p) = e.rec.prepare_status.as_mut() {
            p.state = PrepareState::Succeeded;
            p.finished_at = Some(Timestamp::now());
            p.exit_status = Some(0);
        }
        e.rec.notes = outcome.notes.clone();
        e.rec.process_group = None;
        e.live = None;
        q.persist(self.run_id);
        log::event(Tone::Quiet, "prepared", &rec.job.label, Some(self.run_id), &[human(outcome.duration)]);
    }

    fn finish(&mut self, state: State, exit: i32, reason: Option<String>, duration: Duration) {
        self.finished = true;
        let mut q = queue::lock(self.shared);
        let Some(e) = q.runs.get_mut(self.run_id) else { return };
        if self.phase == Phase::Prepare {
            if let Some(p) = e.rec.prepare_status.as_mut() {
                p.state = PrepareState::Failed;
                p.finished_at = Some(Timestamp::now());
                p.exit_status = Some(exit);
            }
        }
        e.rec.state = state;
        e.rec.exit_status = Some(exit);
        if reason.is_some() {
            e.rec.failure_reason = reason;
        }
        e.rec.finished_at = Some(Timestamp::now());
        e.rec.process_group = None;
        e.live = None;
        let label = e.rec.job.label.clone();
        let why =
            e.rec.error_line.clone().or_else(|| e.rec.failure_reason.clone()).filter(|_| state != State::Succeeded);
        q.persist(self.run_id);
        let tone = match state {
            State::Succeeded => Tone::Good,
            State::Cancelled => Tone::Quiet,
            _ => Tone::Bad,
        };
        let exit = if exit == 0 { String::new() } else { format!("exit {exit}") };
        let word = state.as_str().replace('_', " ");
        log::event(tone, &word, &label, Some(self.run_id), &[human(duration), exit, why.unwrap_or_default()]);
    }
}

impl Drop for Completion<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Some(pgid) = self.process_group {
            process::terminate_group(pgid, Duration::from_secs(2));
        }
        if let Some(group_guard) = self.group_guard.take() {
            group_guard.stand_down();
        }
        let reason = "drmanhattan hit an internal error while running this job (a bug; please report it)";
        log::notice(
            Tone::Bad,
            "error",
            format_args!("the runner of run {} panicked (a bug; please report it)", self.run_id),
        );
        self.finish(State::Failed, 1, Some(reason.into()), Duration::ZERO);
    }
}
