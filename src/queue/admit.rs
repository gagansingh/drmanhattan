//! Admission: moving waiting runs into free slots.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::activity::{Activity, Patterns};
use crate::job::{PrepareState, Record, State};
use crate::log::{self, Tone};
use crate::process;
use crate::queue::{rank, Live, Queue, RunEvent};
use crate::settings::{ManualOrder, MAX_SLOTS};
use crate::time::{human, Timestamp};

/// A run just admitted: everything its runner thread needs.
pub(crate) struct Admitted {
    pub(crate) record: Record,
    pub(crate) events: Receiver<RunEvent>,
    /// For the thread that waits on the child, to report its exit on `events`.
    pub(crate) exit_sender: Sender<RunEvent>,
    pub(crate) activity: Arc<Mutex<Activity>>,
    /// The daemon's patterns; the runner uses the job's own when it gives some.
    pub(crate) default_patterns: Arc<Patterns>,
}

impl Queue {
    /// Admits waiters, in rank order, while fewer than `capacity` run. The caller starts a
    /// runner thread for each. Lowering the capacity stops admissions until enough runs
    /// finish; raising it admits more on this pass.
    pub(crate) fn admit(&mut self, capacity: u32, manual: &ManualOrder) -> Vec<Admitted> {
        let now = Timestamp::now();
        let order = rank(self.records(State::Queued), manual, now);
        let mut admitted = Vec::new();
        // A group runs one job at a time.
        let mut busy_groups: HashSet<String> =
            self.records(State::Running).filter_map(|r| r.job.group.clone()).collect();
        for next in order {
            if self.count(State::Running) >= capacity as usize {
                break;
            }
            let Some(waiter) = self.runs.get(&next.run_id).map(|e| &e.rec) else { continue };
            if !waiter.is_ready() {
                continue; // still preparing
            }
            if let Some(group) = &waiter.job.group {
                if !busy_groups.insert(group.clone()) {
                    continue; // another of its group is running
                }
            }
            let slot = self.free_slot();
            let default_patterns = self.default_patterns.clone();
            let Some(entry) = self.runs.get_mut(&next.run_id) else { continue };
            let (control, events) = mpsc::channel();
            let activity = Arc::new(Mutex::new(Activity::default()));
            entry.live = Some(Live { control: control.clone(), activity: activity.clone() });
            let rec = &mut entry.rec;
            rec.state = State::Running;
            rec.slot = Some(slot);
            rec.started_at = Some(now);
            rec.load_at_start = process::load_now().map(|l| (l * 10.0).round() / 10.0);
            let waited = format!("waited {}", human(rec.queued_at.until(now)));
            log::event(Tone::Start, "started", &rec.job.label, Some(&rec.run_id), &[format!("slot {slot}"), waited]);
            let record = rec.clone();
            self.persist(&next.run_id);
            admitted.push(Admitted { record, events, exit_sender: control, activity, default_patterns });
        }
        admitted
    }

    /// Starts the `prepare` commands of waiting runs, best-ranked first, while fewer than
    /// `limit` prepare at once. The caller starts a thread for each (`runner::run_prepare`).
    pub(crate) fn start_prepares(&mut self, limit: usize, manual: &ManualOrder) -> Vec<Admitted> {
        let preparing = |q: &Queue| {
            q.records(State::Queued)
                .filter(|r| r.prepare_status.as_ref().is_some_and(|p| p.state == PrepareState::Running))
                .count()
        };
        let mut started = Vec::new();
        for next in rank(self.records(State::Queued), manual, Timestamp::now()) {
            if preparing(self) >= limit {
                break;
            }
            let default_patterns = self.default_patterns.clone();
            let Some(entry) = self.runs.get_mut(&next.run_id) else { continue };
            let Some(prepare) = entry.rec.prepare_status.as_mut().filter(|p| p.state == PrepareState::Pending) else {
                continue;
            };
            prepare.state = PrepareState::Running;
            prepare.started_at = Some(Timestamp::now());
            let (control, events) = mpsc::channel();
            let activity = Arc::new(Mutex::new(Activity::default()));
            entry.live = Some(Live { control: control.clone(), activity: activity.clone() });
            let record = entry.rec.clone();
            log::event(Tone::Quiet, "preparing", &record.job.label, Some(&record.run_id), &[]);
            self.persist(&next.run_id);
            started.push(Admitted { record, events, exit_sender: control, activity, default_patterns });
        }
        started
    }

    /// The lowest slot number no running run holds.
    fn free_slot(&self) -> u32 {
        let used: HashSet<u32> = self.records(State::Running).filter_map(|r| r.slot).collect();
        (1..=MAX_SLOTS).find(|n| !used.contains(n)).unwrap_or(MAX_SLOTS)
    }
}
