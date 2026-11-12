//! Admission: moving waiting runs into free slots.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::activity::{Activity, Patterns};
use crate::job::{PrepareState, Record, RemoteMode, RunId, State};
use crate::log::{self, Tone};
use crate::process;
use crate::queue::{rank, Live, Queue, RunEvent};
use crate::settings::{Hold, ManualOrder, MAX_SLOTS};
use crate::time::{human, Timestamp};
use crate::workers;

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
    /// Admits waiters, in rank order, into free slots: this machine's while fewer than
    /// `capacity` run here, and the workers' while fewer than each one's capacity run there.
    /// A quick waiter starts beside the slots while fewer than `max_quick` quick runs do, and
    /// takes a free slot when the quick lane is full. A light waiter does the same with the
    /// light lane, which also takes nothing while the machine is short of memory
    /// (`LightGuard`). With `DRMANHATTAN_BUSY_CPU` set, a slot here past the first takes
    /// nothing while the CPU is busy (`BusyGuard`). The caller starts a runner thread for
    /// each. Lowering a capacity stops admissions there until enough runs finish; raising it
    /// admits more on this pass.
    pub(crate) fn admit(&mut self, capacity: u32, workers: &[workers::Slots], manual: &ManualOrder) -> Vec<Admitted> {
        let now = Timestamp::now();
        let order = rank(self.records(State::Queued), manual, now);
        let mut admitted = Vec::new();
        let light_closed = self.light_lane_closed();
        let cpu = self.settings.busy_guard.and_then(|g| crate::sysinfo::cpu_busy_over(g.window));
        // How busy the CPU was, if that kept a waiter out of a free slot on this pass.
        let mut held_at = None;
        // A group runs one job at a time.
        let mut busy_groups: HashSet<String> =
            self.records(State::Running).filter_map(|r| r.job.group.clone()).collect();
        for next in order {
            let Some(waiter) = self.runs.get(&next.run_id).map(|e| &e.rec) else { continue };
            if !waiter.is_ready() {
                continue; // still preparing
            }
            if waiter.job.group.as_ref().is_some_and(|g| busy_groups.contains(g)) {
                continue; // another of its group is running
            }
            // A service takes none of the build slots, only one of its own few; so does a
            // quick run, while the quick lane has room.
            let service = waiter.job.service;
            let quick =
                waiter.quick && self.records(State::Running).filter(|r| r.quick).count() < self.settings.max_quick;
            let light = !quick
                && waiter.light_lane
                && !light_closed
                && self.records(State::Running).filter(|r| r.light_lane).count() < self.settings.max_light;
            let beside = quick || light;
            let machine = if service {
                let running = self.records(State::Running).filter(|r| r.job.service).count();
                (running < self.settings.max_services).then_some(None)
            } else if beside {
                Some(None)
            } else {
                let hold = self.busy_hold(cpu, now);
                let placed = self.place(waiter, capacity, workers, hold.is_some());
                if let (None, Some(Hold::Busy(pct))) = (&placed, hold) {
                    if self.place(waiter, capacity, workers, false) == Some(None) {
                        held_at = Some(pct);
                    }
                }
                placed
            };
            let Some(machine) = machine else { continue };
            busy_groups.extend(waiter.job.group.clone());
            let slot = match (service, beside) {
                (true, _) => None,
                // Above any slot there can be, so `DRMANHATTAN_SLOT` stays unique among the
                // runs here whatever the capacity becomes.
                (false, true) => Some(self.free_number(None, MAX_SLOTS + 1)),
                (false, false) => Some(self.free_slot(machine.as_deref())),
            };
            let default_patterns = self.default_patterns.clone();
            let Some(entry) = self.runs.get_mut(&next.run_id) else { continue };
            let (control, events) = mpsc::channel();
            let activity = Arc::new(Mutex::new(Activity::default()));
            entry.live = Some(Live { control: control.clone(), activity: activity.clone(), cancel_requested: false });
            let rec = &mut entry.rec;
            rec.state = State::Running;
            rec.slot = slot;
            rec.quick = quick;
            rec.light_lane = light;
            let on = machine.as_ref().map(|m| format!(" on {m}")).unwrap_or_default();
            rec.machine = machine;
            rec.started_at = Some(now);
            rec.load_at_start = process::load_now().map(|l| (l * 10.0).round() / 10.0);
            let waited = format!("waited {}", human(rec.queued_at.until(now)));
            log::event(
                Tone::Start,
                "started",
                &rec.job.label,
                Some(&rec.run_id),
                &[
                    match slot {
                        None => "a service".to_string(),
                        Some(_) if quick => "quick".to_string(),
                        Some(_) if light => "light".to_string(),
                        Some(s) => format!("slot {s}{on}"),
                    },
                    waited,
                ],
            );
            let record = rec.clone();
            self.persist(&next.run_id);
            admitted.push(Admitted { record, events, exit_sender: control, activity, default_patterns });
        }
        self.note_slots_held(held_at);
        admitted
    }

    /// Why a slot here past the first takes nothing now, if it does: the newest run in a
    /// slot here has not had the guard's window to show what it uses, or the CPU was busy
    /// over it. None without `DRMANHATTAN_BUSY_CPU`, and while no slot here is taken.
    fn busy_hold(&self, cpu: Option<u32>, now: Timestamp) -> Option<Hold> {
        let guard = self.settings.busy_guard?;
        let newest = self
            .records(State::Running)
            .filter(|r| r.machine.is_none() && r.holds_slot())
            .filter_map(|r| r.started_at)
            .max();
        guard.holds(newest.map(|at| at.until(now)), cpu)
    }

    /// Says in the log when a busy CPU begins to keep waiters out of free slots. Their
    /// `started` lines say when it ends.
    fn note_slots_held(&mut self, held_at: Option<u32>) {
        let was_held = std::mem::replace(&mut self.slots_held, held_at.is_some());
        let (Some(pct), false, Some(guard)) = (held_at, was_held, self.settings.busy_guard) else { return };
        let detail = format!(
            "the CPU was {pct}% busy over the last {} (DRMANHATTAN_BUSY_CPU={}): slots past the first wait",
            human(guard.window),
            guard.cpu
        );
        log::event(Tone::Quiet, "slots held", "", None, &[detail]);
    }

    /// Whether the light lane takes nothing on this pass: a light run is waiting and the
    /// machine is short of memory. Says so in the log when that begins and when it ends.
    fn light_lane_closed(&mut self) -> bool {
        let guard = self.settings.light_guard;
        if !self.records(State::Queued).any(|r| r.light_lane) {
            return false; // nothing to hold: no reading taken
        }
        let pressure = crate::sysinfo::memory_pressure();
        let closed = guard.closes_at(pressure);
        if closed != self.light_closed {
            self.light_closed = closed;
            let pressure = pressure.unwrap_or("unknown");
            let detail = if closed {
                format!("memory pressure is {pressure}: light jobs wait for a slot")
            } else {
                format!("memory pressure is {pressure}: light jobs start beside the slots again")
            };
            log::event(Tone::Quiet, if closed { "light closed" } else { "light open" }, "", None, &[detail]);
        }
        closed
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
            entry.live = Some(Live { control: control.clone(), activity: activity.clone(), cancel_requested: false });
            let record = entry.rec.clone();
            log::event(Tone::Quiet, "preparing", &record.job.label, Some(&record.run_id), &[]);
            self.persist(&next.run_id);
            started.push(Admitted { record, events, exit_sender: control, activity, default_patterns });
        }
        started
    }

    /// Where a ready waiter can run now: Some(None) here, Some(Some(worker)) on a worker, None
    /// nowhere yet. A job runs here unless its `remote` says otherwise: `allowed` goes to a
    /// worker only while every slot here is busy, `prefer` to a free worker first. A worker
    /// must have a free slot, every tag the job `requires`, and not be out of admission after
    /// a failure; a run that a worker could not take stays here. With `held`, only the first
    /// slot here takes a job (`busy_hold`); the workers' slots are their own.
    fn place(&self, rec: &Record, capacity: u32, workers: &[workers::Slots], held: bool) -> Option<Option<String>> {
        let running_on = |m: Option<&str>| {
            self.records(State::Running).filter(|r| r.machine.as_deref() == m && r.holds_slot()).count()
        };
        // A worker's dispatcher may grant more of its slots than it would use itself.
        let local_capacity = capacity.max(rec.job.slots_hint.unwrap_or(0));
        let local_capacity = if held { local_capacity.min(1) } else { local_capacity };
        let here = (running_on(None) < local_capacity as usize).then_some(None);
        if rec.job.remote == RemoteMode::Never || rec.remote_fallback.is_some() {
            return here;
        }
        let worker = workers
            .iter()
            .find(|w| {
                !w.unavailable
                    && running_on(Some(&w.name)) < w.capacity as usize
                    && rec.job.requires.iter().all(|t| w.tags.contains(t))
            })
            .map(|w| Some(w.name.clone()));
        match rec.job.remote {
            RemoteMode::Prefer => worker.or(here),
            _ => here.or(worker),
        }
    }

    /// The lowest slot number no running run on `machine` (None: this one) holds.
    fn free_slot(&self, machine: Option<&str>) -> u32 {
        self.free_number(machine, 1).min(MAX_SLOTS)
    }

    /// The lowest number from `from` up that no running run on `machine` holds, quick runs
    /// included, so every run there has its own `DRMANHATTAN_SLOT`.
    fn free_number(&self, machine: Option<&str>, from: u32) -> u32 {
        let used: HashSet<u32> = self
            .records(State::Running)
            .filter(|r| r.machine.as_deref() == machine && !r.job.service)
            .filter_map(|r| r.slot)
            .collect();
        (from..).find(|n| !used.contains(n)).unwrap_or(from)
    }

    /// Runs on a worker that were running when the daemon last stopped: their runners pick
    /// up following them where they are. Called once, after recovery.
    pub(crate) fn reattach(&mut self) -> Vec<Admitted> {
        let ids: Vec<RunId> =
            self.records(State::Running).filter(|r| r.remote_run.is_some()).map(|r| r.run_id.clone()).collect();
        let mut started = Vec::new();
        for run_id in ids {
            let default_patterns = self.default_patterns.clone();
            let Some(entry) = self.runs.get_mut(&run_id) else { continue };
            let (control, events) = mpsc::channel();
            let activity = Arc::new(Mutex::new(Activity::default()));
            entry.live = Some(Live { control: control.clone(), activity: activity.clone(), cancel_requested: false });
            started.push(Admitted {
                record: entry.rec.clone(),
                events,
                exit_sender: control,
                activity,
                default_patterns,
            });
        }
        started
    }
}
