//! Start-up: picking up what the previous daemon left.
//!
//! - Queued runs go back in the queue; one that was running its `prepare` command runs it
//!   again.
//! - Runs that were running are recorded abandoned (nothing is watching them any more). Their
//!   processes were normally ended when the daemon died, by the guard beside each run
//!   (`runner::guard`); if a guard died too, what is left is terminated here.
//! - Finished runs stay for the dashboard's recent list; one whose `.exit` never got
//!   written (the daemon died in between) gets it now, so no poller waits forever.
//! - A job file that was claimed but that no record accounts for, and that has no `.exit`
//!   (the daemon died just after claiming it, or the state directory was cleared, as a
//!   reboot clears `/tmp`), is queued again, unless its log shows it had started, in which
//!   case it is recorded abandoned. Records name the claimed file by its identity (device
//!   and inode), so a job that reused an old run's name is never mistaken for that run.
//! - Temp files stranded by a crash are removed.

use std::fmt;
use std::fs;
use std::time::Duration;

use crate::fsutil;
use crate::job::{FileStatus, JobFiles, PrepareState, Record, State};
use crate::log;
use crate::process;
use crate::queue::persist::{read_records, write_results};
use crate::queue::{Entry, Queue};
use crate::time::{count, Timestamp};

/// Temp files older than this are strays. Younger ones may be a `submit` writing right now.
const STALE_TEMP: Duration = Duration::from_secs(600);
/// `ps` reports ages in whole seconds, and a process starts a moment after its run does.
const AGE_SLACK: Duration = Duration::from_secs(5);
/// How often the running daemon rewrites its heartbeat file. The last heartbeat bounds when
/// a killed daemon died: no later than this after it.
pub(crate) const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);

/// What recovery did, for the start-up log line.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct Recovery {
    pub(crate) requeued: usize,
    pub(crate) abandoned: usize,
    pub(crate) finished: usize,
    pub(crate) exits_rewritten: usize,
    pub(crate) reclaimed: usize,
    pub(crate) temp_removed: usize,
    pub(crate) reattached: usize,
}

impl Recovery {
    /// Nothing was picked up or repaired: finished runs reloaded do not count.
    pub(crate) fn is_quiet(&self) -> bool {
        self.parts().is_empty()
    }

    fn parts(&self) -> Vec<String> {
        [
            (self.requeued, "waiting job queued again", "waiting jobs queued again"),
            (self.abandoned, "running job abandoned", "running jobs abandoned"),
            (self.reattached, "job on a worker followed again", "jobs on workers followed again"),
            (self.reclaimed, "lost job file recovered", "lost job files recovered"),
            (self.exits_rewritten, "missing .exit written", "missing .exit files written"),
            (self.temp_removed, "stray temp file removed", "stray temp files removed"),
        ]
        .into_iter()
        .filter(|(n, _, _)| *n > 0)
        .map(|(n, one, many)| count(n, one, many))
        .collect()
    }
}

impl fmt::Display for Recovery {
    /// What recovery did, for the start-up summary: `2 waiting jobs queued again · 1 running
    /// job abandoned`, or `nothing` when it did nothing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts = self.parts();
        f.write_str(if parts.is_empty() { "nothing" } else { "" })?;
        f.write_str(&parts.join(" · "))
    }
}

/// What became of the processes a run left behind.
#[derive(Debug, PartialEq)]
enum Leftovers {
    None,
    Terminated,
    /// The process group id now belongs to processes that are not the run's.
    NotOurs,
}

impl Queue {
    pub(crate) fn recover(&mut self) -> Recovery {
        let mut done = Recovery { temp_removed: self.remove_stale_temp_files(), ..Recovery::default() };
        let mut records = read_records(&self.paths);
        records.sort_by_key(|r| r.queued_at);
        let died_by = self.daemon_died_by();
        for mut rec in records {
            match rec.state {
                State::Queued => {
                    done.requeued += 1;
                    rec.daemon_pid = std::process::id();
                    rec.slot = None;
                    self.restart_prepare(&mut rec, died_by);
                    write_results(&self.paths, &rec);
                }
                // On a worker: its runner follows it there again (`reattach`).
                State::Running if rec.remote_run.is_some() => {
                    done.reattached += 1;
                    rec.daemon_pid = std::process::id();
                    write_results(&self.paths, &rec);
                }
                // Still being handed to a worker, which never got it: it waits again.
                State::Running if rec.machine.is_some() => {
                    done.requeued += 1;
                    rec.state = State::Queued;
                    (rec.slot, rec.started_at, rec.machine) = (None, None, None);
                    rec.daemon_pid = std::process::id();
                    write_results(&self.paths, &rec);
                }
                State::Running => {
                    done.abandoned += 1;
                    self.abandon(&mut rec, died_by);
                    write_results(&self.paths, &rec);
                }
                _ => done.finished += 1,
            }
            self.runs.insert(rec.run_id.clone(), Entry { rec, live: None });
        }
        done.exits_rewritten = self.rewrite_missing_exits();
        done.reclaimed = self.reclaim_orphaned_job_files();
        done
    }

    /// A run that was preparing when the daemon stopped prepares again: what its command
    /// left behind is ended, as for a running run.
    fn restart_prepare(&self, rec: &mut Record, died_by: Option<Timestamp>) {
        let Some(prepare) = rec.prepare_status.as_mut().filter(|p| p.state == PrepareState::Running) else { return };
        if let (Some(pgid), Some(started)) = (rec.process_group, prepare.started_at) {
            self.end_leftovers(pgid, started, died_by, &rec.job.label);
        }
        prepare.state = PrepareState::Pending;
        prepare.started_at = None;
        rec.process_group = None;
    }

    /// The latest the previous daemon can have died: its last heartbeat, plus the interval.
    fn daemon_died_by(&self) -> Option<Timestamp> {
        let text = fs::read_to_string(self.paths.heartbeat()).ok()?;
        Some(text.trim().parse::<Timestamp>().ok()?.plus(HEARTBEAT_EVERY))
    }

    fn abandon(&self, rec: &mut Record, died_by: Option<Timestamp>) {
        let leftovers = match (rec.process_group, rec.started_at) {
            (Some(pgid), Some(started)) => self.end_leftovers(pgid, started, died_by, &rec.job.label),
            _ => Leftovers::None,
        };
        let what = match leftovers {
            Leftovers::None => "the job's processes have ended",
            Leftovers::Terminated => "the processes it left behind were terminated",
            Leftovers::NotOurs => "its process group id is now used by other processes, which were left alone",
        };
        rec.state = State::Abandoned;
        rec.failure_reason = Some(format!("the daemon stopped while this job was running; {what}"));
        rec.finished_at = Some(Timestamp::now());
        rec.process_group = None;
    }

    /// Terminates what is left of a run's process group, but only when it is the run's and
    /// not a stranger that was given the recycled id. A process id is not reused while a
    /// group by that id exists, so:
    ///
    /// - a living leader (pid = group id) is the run's if it is as old as the run;
    /// - with the leader gone, the members are the run's if they all started after the run
    ///   did, and the oldest started before the previous daemon died: a stranger's group can
    ///   only appear once the run's own group had gone, which, while the daemon lived, its
    ///   runner would have seen and recorded.
    ///
    /// Without a heartbeat to say when the daemon died, a leaderless group is left alone.
    fn end_leftovers(&self, pgid: i32, started: Timestamp, died_by: Option<Timestamp>, label: &str) -> Leftovers {
        if !process::group_alive(pgid) {
            return Leftovers::None;
        }
        let run_age = started.elapsed();
        let ours = if process::process_alive(pgid) {
            process::process_age(pgid).is_some_and(|age| age + AGE_SLACK >= run_age)
        } else {
            let ages = process::group_member_ages(pgid);
            let since_death = died_by.map(Timestamp::elapsed);
            let oldest = ages.iter().max();
            let after_run = ages.iter().all(|age| *age <= run_age + AGE_SLACK);
            let before_death = oldest.zip(since_death).is_some_and(|(oldest, since)| *oldest + AGE_SLACK >= since);
            after_run && before_death
        };
        if !ours {
            log::problem(format_args!("left process group {pgid} alone: it is no longer the one {label} started"));
            return Leftovers::NotOurs;
        }
        log::notice(log::Tone::Warning, "cleanup", format_args!("ending process group {pgid}, left behind by {label}"));
        process::terminate_group(pgid, self.settings.term_grace);
        Leftovers::Terminated
    }

    /// A finished run whose `.exit` is missing gets its results written again, provided the
    /// claimed job file beside it is still that run's (a newer job may have reused the name)
    /// and no newer job by that name waits in the inbox.
    fn rewrite_missing_exits(&self) -> usize {
        let mut rewritten = 0;
        for rec in self.runs.values().map(|e| &e.rec).filter(|r| r.state.is_terminal()) {
            let files = rec.job.files();
            if owns_claimed_file(rec) && !files.exit().exists() && !files.job().exists() {
                write_results(&self.paths, rec);
                rewritten += 1;
            }
        }
        rewritten
    }

    /// Claimed job files that no record accounts for and that have no `.exit`.
    fn reclaim_orphaned_job_files(&self) -> usize {
        let mut reclaimed = 0;
        for entry in fs::read_dir(&self.inbox).into_iter().flatten().flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Some(name) = file_name.strip_suffix(".json.claimed") else { continue };
            let files = JobFiles::new(&self.inbox, name);
            // The record must be this file's: same path (the inbox may have changed between
            // runs), and the file still the one it claimed (the name may have been reused).
            let accounted =
                self.runs.values().any(|e| e.rec.job.files().claimed() == files.claimed() && owns_claimed_file(&e.rec));
            if accounted || files.exit().exists() || !fsutil::is_own_file(&files.claimed()) {
                continue;
            }
            if files.log().exists() {
                let reason = "the daemon lost track of this job while it was running";
                let _ = fsutil::write_json(&files.status(), &FileStatus::abandoned(reason, &files.claimed()));
                let _ = fsutil::write_atomic(&files.exit(), "1\n");
                log::event(
                    log::Tone::Bad,
                    "abandoned",
                    name,
                    None,
                    &["it had started, but no record of it survived".into()],
                );
                reclaimed += 1;
            } else if !files.job().exists() && fs::rename(files.claimed(), files.job()).is_ok() {
                log::event(
                    log::Tone::Quiet,
                    "requeued",
                    name,
                    None,
                    &["it was claimed, but no record of it survived".into()],
                );
                reclaimed += 1;
            }
        }
        reclaimed
    }

    fn remove_stale_temp_files(&self) -> usize {
        let dirs = [self.paths.state.clone(), self.paths.runs(), self.paths.data.clone(), self.inbox.clone()];
        let mut removed = 0;
        for entry in dirs.iter().flat_map(fs::read_dir).flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stale = entry.metadata().ok().and_then(|m| fsutil::age(&m)).is_some_and(|age| age > STALE_TEMP);
            let stray = (name.starts_with(".tmp.") && stale) || name == ".drmanhattan-write-test";
            if stray && fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// Whether the claimed job file beside a run's record is still that run's own. A record
/// written before claims were recorded owns the file that bears its name.
fn owns_claimed_file(rec: &Record) -> bool {
    let claimed = rec.job.files().claimed();
    match rec.job.claim {
        Some(claim) => fsutil::file_id(&claimed) == Some(claim),
        None => claimed.exists(),
    }
}
