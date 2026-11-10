//! Writing a run's state where the outside world reads it, and reading it back.

use crate::fsutil;
use crate::job::{JobFiles, Record, RunId};
use crate::log::{self, Tone};
use crate::paths::Paths;
use crate::queue::Queue;

impl Queue {
    /// Writes everything about one run after a state change (see `write_results`). A run
    /// whose results could not all be written is said so loudly, once, and written again
    /// on every heartbeat until they are (`retry_unwritten`): a run that has ended never
    /// changes state again, so without that nothing would ever correct what readers see.
    pub(crate) fn persist(&mut self, run_id: &RunId) {
        let Some(entry) = self.runs.get(run_id) else { return };
        let failures = write_results(&self.paths, &entry.rec);
        if failures.is_empty() {
            if self.unwritten.remove(run_id).is_some() {
                let state = entry.rec.state;
                log::notice(Tone::Good, "recorded", format_args!("run {run_id} is written down at last: {state}"));
            }
            return;
        }
        // Said once for each state that cannot be written; the end of a run above all.
        if self.unwritten.insert(run_id.clone(), entry.rec.state) != Some(entry.rec.state) {
            for failure in &failures {
                log::problem(failure);
            }
            log::notice(
                Tone::Bad,
                "unrecorded",
                format_args!(
                    "run {run_id} is {}, but that could not be written; `status` and `wait` may show it \
                     out of date until it is (is the disk full?). Trying again every few seconds",
                    entry.rec.state
                ),
            );
        }
    }

    /// Writes again the runs whose last write failed. Quiet until one succeeds.
    pub(crate) fn retry_unwritten(&mut self) {
        let ids: Vec<RunId> = self.unwritten.keys().cloned().collect();
        for run_id in ids {
            if self.runs.contains_key(&run_id) {
                self.persist(&run_id);
            } else {
                self.unwritten.remove(&run_id); // forgotten meanwhile
            }
        }
    }
}

/// The record under `state/runs/`; `<job>.status.json` beside the job file (without the
/// job's environment, which may hold secrets); and once the run is over, `<job>.exit`,
/// written last so that its existence means "done", plus a copy in the archive. Each is a
/// temp file and a rename, so a reader never sees half a file. Returns what could not be
/// written; the caller writes it all again later.
///
/// When the record of a run that has ended cannot be written (a full disk has no room for
/// the temp file), the old one is removed and the record written again in the room that
/// frees. Failing that, no record is better than one saying queued or running: readers then
/// go by `.status.json`, and a restarted daemon by the job's files.
pub(crate) fn write_results(paths: &Paths, rec: &Record) -> Vec<String> {
    let files = rec.job.files();
    let mut failures = Vec::new();
    let mut note =
        |what: &dyn std::fmt::Display, e: crate::error::Error| failures.push(format!("could not write {what}: {e}"));
    let record = paths.run_record(rec.run_id.as_str());
    if let Err(e) = fsutil::write_json(&record, rec) {
        let rescued = rec.state.is_terminal()
            && fsutil::remove_if_present(&record).is_ok()
            && fsutil::write_json(&record, rec).is_ok();
        if !rescued {
            note(&format_args!("the record of {}", rec.run_id), e);
        }
    }
    if let Err(e) = fsutil::write_json(&files.status(), &rec.public()) {
        note(&files.status().display(), e);
    }
    if rec.state.is_terminal() {
        if let Err(e) = fsutil::write_atomic(&files.exit(), &format!("{}\n", rec.exit_code())) {
            note(&files.exit().display(), e);
        }
        let month = paths.archive().join(rec.last_event().month());
        let archived = fsutil::ensure_private_dir(&month)
            .and_then(|()| fsutil::write_json(&month.join(format!("{}.json", rec.run_id)), rec));
        if let Err(e) = archived {
            note(&format_args!("the archived record of {}", rec.run_id), e);
        }
    }
    failures
}

/// Every run record under `state/runs/`, each as up to date as its files show it (see
/// `freshest`). Hidden files (a record on its way to being renamed into place) and records
/// whose job file is not a job's are skipped.
pub(crate) fn read_records(paths: &Paths) -> Vec<Record> {
    let entries = std::fs::read_dir(paths.runs()).into_iter().flatten().flatten();
    let visible =
        entries.filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(".json") && !n.starts_with('.')));
    let records = visible.filter_map(|e| fsutil::read_json::<Record>(&e.path()));
    records.filter(|r| JobFiles::of(&r.job.job_file).is_some()).map(freshest).collect()
}

/// A run's record, or None when it has none under `state/runs/` (see `freshest`).
pub(crate) fn read_record(paths: &Paths, run_id: &RunId) -> Option<Record> {
    fsutil::read_json(&paths.run_record(run_id.as_str())).map(freshest)
}

/// The record, unless the run's `.status.json` says it has ended and the record does not:
/// then the run ended but its record could not be written (a full disk), and the status
/// file, written just after it, is the one to believe. A run never goes back from ended.
fn freshest(rec: Record) -> Record {
    if rec.state.is_terminal() {
        return rec;
    }
    let status: Option<Record> = fsutil::read_json(&rec.job.files().status());
    match status {
        Some(mut ended) if ended.run_id == rec.run_id && ended.state.is_terminal() => {
            ended.job.env = rec.job.env; // the status file leaves it out
            ended
        }
        _ => rec,
    }
}
