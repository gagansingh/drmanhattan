//! Housekeeping: forgetting old runs and removing files past their retention.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::fsutil;
use crate::job::{classify, InboxFile, RunId};
use crate::paths::Paths;
use crate::queue::Queue;
use crate::settings::Settings;

/// `history.jsonl` past this many lines is cut back to the newest `HISTORY_KEEP`.
const HISTORY_MAX: usize = 30_000;
const HISTORY_KEEP: usize = 20_000;
const STEPS_MAX: usize = 20_000;
const STEPS_KEEP: usize = 10_000;

impl Queue {
    /// Forgets finished runs older than the retention window, and returns the names of the
    /// jobs still live, whose files `prune_files` must spare. Cheap: run under the lock.
    pub(crate) fn prune_memory(&mut self) -> HashSet<String> {
        let retention = self.settings.retention;
        let old: Vec<RunId> = self
            .runs
            .values()
            .filter(|e| e.rec.state.is_terminal() && e.rec.last_event().elapsed() > retention)
            .map(|e| e.rec.run_id.clone())
            .collect();
        for run_id in old {
            self.runs.remove(&run_id);
            let _ = fs::remove_file(self.paths.run_record(run_id.as_str()));
        }
        self.live_names()
    }
}

/// Removes what has outlived its window, outside the queue lock: the inbox files of
/// finished jobs past the retention (their logs may hold secrets), archived records past
/// the archive window, and the oldest lines of the history and step files.
pub(crate) fn prune_files(paths: &Paths, inbox: &Path, live: &HashSet<String>, settings: &Settings) {
    for entry in fs::read_dir(inbox).into_iter().flatten().flatten() {
        let file_name = entry.file_name();
        let Some((InboxFile::Result, name)) = file_name.to_str().and_then(classify) else { continue };
        let expired = entry.metadata().ok().and_then(|m| fsutil::age(&m)).is_some_and(|age| age > settings.retention);
        if expired && !live.contains(name) {
            let _ = fs::remove_file(entry.path());
        }
    }
    for month in fs::read_dir(paths.archive()).into_iter().flatten().flatten() {
        for record in fs::read_dir(month.path()).into_iter().flatten().flatten() {
            let age = record.metadata().ok().and_then(|m| fsutil::age(&m));
            if age.is_some_and(|age| age > settings.archive_retention) {
                let _ = fs::remove_file(record.path());
            }
        }
        let _ = fs::remove_dir(month.path()); // succeeds only once the month is empty
    }
    fsutil::trim_lines(&paths.history(), HISTORY_MAX, HISTORY_KEEP);
    fsutil::trim_lines(&paths.steps(), STEPS_MAX, STEPS_KEEP);
}
