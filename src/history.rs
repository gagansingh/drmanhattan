//! The timing history: one line in `history.jsonl` per finished run, which the estimates
//! learn from. The daemon reads the newest rows once at start-up and keeps them in memory.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::fsutil;
use crate::job::State;
use crate::paths::Paths;
use crate::time::Timestamp;

/// How many recent rows are kept in memory.
pub(crate) const IN_MEMORY: usize = 2000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct HistoryRow {
    pub(crate) finished_at: Timestamp,
    pub(crate) category: String,
    pub(crate) label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) variant: String,
    pub(crate) seconds: u64,
    /// The same in milliseconds; rows from before 0.5.1 have only `seconds`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) millis: Option<u64>,
    pub(crate) exit_status: i32,
    pub(crate) outcome: State,
    /// The run's average load, for the load model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) load: Option<f64>,
    /// The worker it ran on; none for this machine. Each machine's estimates come from its
    /// own runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) machine: Option<String>,
}

impl HistoryRow {
    /// How long it ran, in milliseconds.
    pub(crate) fn millis(&self) -> u64 {
        self.millis.unwrap_or(self.seconds * 1000)
    }
}

pub(crate) fn append(paths: &Paths, row: &HistoryRow) -> Result<()> {
    fsutil::ensure_private_dir(&paths.data)?;
    let line = serde_json::to_string(row).map_err(|e| crate::error::Error::Invalid(e.to_string()))?;
    fsutil::append_line(&paths.history(), &line)
}

/// The newest `last` rows that parse.
pub(crate) fn read_recent(paths: &Paths, last: usize) -> Vec<HistoryRow> {
    let path = paths.history();
    if !fsutil::is_own_file(&path) {
        return Vec::new();
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(last)..].iter().filter_map(|l| serde_json::from_str(l).ok()).collect()
}
