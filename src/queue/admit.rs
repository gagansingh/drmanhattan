//! Admission: moving waiting runs into free slots.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::activity::{Activity, Patterns};
use crate::job::{Record, State};
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
        for next in order {
            if self.count(State::Running) >= capacity as usize {
                break;
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

    /// The lowest slot number no running run holds.
    fn free_slot(&self) -> u32 {
        let used: HashSet<u32> = self.records(State::Running).filter_map(|r| r.slot).collect();
        (1..=MAX_SLOTS).find(|n| !used.contains(n)).unwrap_or(MAX_SLOTS)
    }
}
