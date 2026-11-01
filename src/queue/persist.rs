//! Writing a run's state where the outside world reads it.

use crate::fsutil;
use crate::job::{JobFiles, Record, RunId};
use crate::log;
use crate::paths::Paths;
use crate::queue::Queue;

impl Queue {
    /// Writes everything about one run after a state change (see `write_results`).
    pub(crate) fn persist(&self, run_id: &RunId) {
        if let Some(entry) = self.runs.get(run_id) {
            write_results(&self.paths, &entry.rec);
        }
    }
}

/// The record under `state/runs/`; `<job>.status.json` beside the job file (without the
/// job's environment, which may hold secrets); and once the run is over, `<job>.exit`,
/// written last so that its existence means "done", plus a copy in the archive. Each is a
/// temp file and a rename, so a reader never sees half a file. A failure is logged and
/// the next state change tries again.
pub(crate) fn write_results(paths: &Paths, rec: &Record) {
    let files = rec.job.files();
    let warn = |what: &dyn std::fmt::Display, e: crate::error::Error| {
        log::problem(format_args!("could not write {what}: {e}"))
    };
    if let Err(e) = fsutil::write_json(&paths.run_record(rec.run_id.as_str()), rec) {
        warn(&format_args!("the record of {}", rec.run_id), e);
    }
    if let Err(e) = fsutil::write_json(&files.status(), &rec.public()) {
        warn(&files.status().display(), e);
    }
    if rec.state.is_terminal() {
        if let Err(e) = fsutil::write_atomic(&files.exit(), &format!("{}\n", rec.exit_code())) {
            warn(&files.exit().display(), e);
        }
        let month = paths.archive().join(rec.last_event().month());
        let archived = fsutil::ensure_private_dir(&month)
            .and_then(|()| fsutil::write_json(&month.join(format!("{}.json", rec.run_id)), rec));
        if let Err(e) = archived {
            warn(&format_args!("the archived record of {}", rec.run_id), e);
        }
    }
}

/// Every run record under `state/runs/`. Hidden files (a record on its way to being
/// renamed into place) and records whose job file is not a job's are skipped.
pub(crate) fn read_records(paths: &Paths) -> Vec<Record> {
    let entries = std::fs::read_dir(paths.runs()).into_iter().flatten().flatten();
    let visible =
        entries.filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(".json") && !n.starts_with('.')));
    let records = visible.filter_map(|e| fsutil::read_json::<Record>(&e.path()));
    records.filter(|r| JobFiles::of(&r.job.job_file).is_some()).collect()
}
