//! The order the next free slot goes in.
//!
//! Waiting runs fall into four groups, served in this order:
//!
//! 1. priority jobs, by arrival;
//! 2. runs a person placed by dragging them on the dashboard, in that order;
//! 3. runs past their anti-starvation bound (`skip_after_seconds`), by arrival;
//! 4. everyone else, shortest expected first, runs with no estimate last.
//!
//! Ties go to the earlier arrival, then the smaller run id, so the order is total and the
//! same on every pass. `drmanhattan status` shows the same order the daemon uses.

use std::cmp::Ordering;
use std::time::Duration;

use crate::job::{Record, RunId};
use crate::settings::ManualOrder;
use crate::time::Timestamp;

/// One waiter's place in the queue.
#[derive(Debug, PartialEq)]
pub(crate) struct Ranked {
    pub(crate) run_id: RunId,
    pub(crate) overdue: bool,
    /// Placed by the manual order.
    pub(crate) manual: bool,
}

struct Key<'a> {
    group: u8,
    /// The group's own measure: manual position, or expected seconds.
    measure: f64,
    queued_at: Timestamp,
    run_id: &'a RunId,
}

impl Key<'_> {
    fn cmp(&self, other: &Key) -> Ordering {
        (self.group.cmp(&other.group))
            .then(self.measure.total_cmp(&other.measure))
            .then(self.queued_at.cmp(&other.queued_at))
            .then(self.run_id.cmp(other.run_id))
    }
}

pub(crate) fn rank<'a>(queued: impl Iterator<Item = &'a Record>, manual: &ManualOrder, now: Timestamp) -> Vec<Ranked> {
    let mut keyed: Vec<(Key, Ranked)> = queued
        .map(|r| {
            let overdue = r.queued_at.until(now) > Duration::from_secs(r.skip_after_seconds);
            let position = manual.get(&r.run_id).filter(|_| !r.job.priority);
            let (group, measure) = match (r.job.priority, position) {
                (true, _) => (0, 0.0),
                (false, Some(pos)) => (1, *pos as f64),
                (false, None) if overdue => (2, 0.0),
                (false, None) => (3, r.expected_seconds.map_or(f64::INFINITY, |e| e as f64)),
            };
            let key = Key { group, measure, queued_at: r.queued_at, run_id: &r.run_id };
            (key, Ranked { run_id: r.run_id.clone(), overdue, manual: position.is_some() })
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::job::{Job, JobFiles};

    fn queued(id: &str, waited: u64, expected: Option<u64>, priority: bool, now: Timestamp) -> Record {
        let files = JobFiles::new(Path::new("/in"), id);
        let job = Job {
            label: id.into(),
            category: "t".into(),
            command: vec!["true".into()],
            cwd: PathBuf::from("/"),
            job_file: files.claimed(),
            log: files.log(),
            variant: None,
            priority,
            source: None,
            timeout: None,
            activity: None,
            env: Default::default(),
            fail_patterns: Vec::new(),
            claim: None,
        };
        let mut r = Record::new(id.parse().unwrap(), job, expected, 600);
        r.queued_at = now.minus(Duration::from_secs(waited));
        r
    }

    fn order(ranked: Vec<Ranked>) -> Vec<String> {
        ranked.into_iter().map(|r| r.run_id.to_string()).collect()
    }

    #[test]
    fn ranking_follows_the_documented_order() {
        let now = Timestamp::now();
        let runs = [
            queued("1-1-a", 10, Some(100), false, now),
            queued("1-1-b", 9, Some(5), false, now),
            queued("1-1-c", 8, None, false, now),
            queued("1-1-d", 7, Some(500), true, now),
            queued("1-1-e", 700, Some(900), false, now), // past its 600 s bound
            queued("1-1-f", 6, Some(5), false, now),     // ties with b, arrived later
        ];
        let none = ManualOrder::new();
        assert_eq!(order(rank(runs.iter(), &none, now)), ["1-1-d", "1-1-e", "1-1-b", "1-1-f", "1-1-a", "1-1-c"]);
        let manual: ManualOrder = [("1-1-c".parse().unwrap(), 0), ("1-1-d".parse().unwrap(), 1)].into();
        let ranked = rank(runs.iter(), &manual, now);
        assert_eq!(ranked[0].run_id.as_str(), "1-1-d", "priority stays first; the manual order cannot move it");
        assert_eq!((ranked[1].run_id.as_str(), ranked[1].manual), ("1-1-c", true));
        assert!(ranked[2].overdue);
    }

    #[test]
    fn identical_keys_break_ties_by_run_id() {
        let now = Timestamp::now();
        let mut runs = [queued("1-1-b", 5, Some(3), false, now), queued("1-1-a", 5, Some(3), false, now)];
        runs[1].queued_at = runs[0].queued_at;
        assert_eq!(order(rank(runs.iter(), &ManualOrder::new(), now)), ["1-1-a", "1-1-b"]);
    }
}
