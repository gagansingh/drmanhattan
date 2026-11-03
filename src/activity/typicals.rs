//! The usual duration of each `::start`/`::done` command, learned across runs: the median
//! of its last ten completions. Completions are appended to `steps.jsonl`, which seeds the
//! daemon's memory at start-up.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::estimate::median;
use crate::fsutil;
use crate::paths::Paths;
use crate::time::Timestamp;

const KEEP_PER_LABEL: usize = 10;
/// How many of the newest lines are read at start-up.
const READ_AT_START: usize = 5000;

/// One finished command, as `steps.jsonl` records it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct StepRow {
    pub(crate) finished_at: Timestamp,
    pub(crate) label: String,
    pub(crate) seconds: u64,
    /// The same in milliseconds; rows from before 0.5.1 have only `seconds`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) millis: Option<u64>,
}

impl StepRow {
    pub(crate) fn of(label: String, took: Duration, finished_at: Timestamp) -> StepRow {
        StepRow { finished_at, label, seconds: took.as_secs(), millis: Some(took.as_millis() as u64) }
    }
}

#[derive(Default, Debug)]
pub(crate) struct Typicals {
    /// Milliseconds, per label.
    durations: HashMap<String, Vec<u64>>,
}

impl Typicals {
    pub(crate) fn load(paths: &Paths) -> Typicals {
        let mut t = Typicals::default();
        let text = if fsutil::is_own_file(&paths.steps()) {
            std::fs::read_to_string(paths.steps()).unwrap_or_default()
        } else {
            String::new()
        };
        let lines: Vec<&str> = text.lines().collect();
        for row in
            lines[lines.len().saturating_sub(READ_AT_START)..].iter().filter_map(|l| serde_json::from_str(l).ok())
        {
            t.learn(&row);
        }
        t
    }

    pub(crate) fn learn(&mut self, row: &StepRow) {
        let list = self.durations.entry(row.label.clone()).or_default();
        list.push(row.millis.unwrap_or(row.seconds * 1000));
        if list.len() > KEEP_PER_LABEL {
            list.remove(0);
        }
    }

    pub(crate) fn typical(&self, label: &str) -> Option<Duration> {
        self.durations.get(label).filter(|v| !v.is_empty()).map(|v| Duration::from_millis(median(v)))
    }
}

/// Appends finished commands to `steps.jsonl` in one write; best effort.
pub(crate) fn append(paths: &Paths, rows: &[StepRow]) {
    let lines: Vec<String> = rows.iter().filter_map(|row| serde_json::to_string(row).ok()).collect();
    if !lines.is_empty() {
        let _ = fsutil::append_line(&paths.steps(), &lines.join("\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_typical_is_the_median_of_the_last_ten() {
        let mut t = Typicals::default();
        for seconds in [100, 100, 100, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
            t.learn(&StepRow::of("cc".into(), Duration::from_secs(seconds), Timestamp::now()));
        }
        assert_eq!(t.typical("cc"), Some(Duration::from_millis(5500)));
        assert_eq!(t.typical("ld"), None);
    }
}
