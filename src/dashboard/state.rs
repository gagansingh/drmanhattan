//! `/api/state`: the whole dashboard as one JSON document, built on every poll.
//!
//! The queue lock is held only to copy out the records and estimates (`Snapshot::take`);
//! reading the other tools' session files, the settings and the machine readings, and
//! building the document, all happen after it is released.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::activity::{Activity, Typicals};
use crate::dashboard::run_view::{Eta, RunView};
use crate::dashboard::Ctx;
use crate::estimate::{Estimate, LoadModel};
use crate::job::{Record, State};
use crate::process;
use crate::queue::{self, rank};
use crate::settings::{
    effective_capacity, load_config, read_manual_order, read_override, read_worker_override, standing_capacity,
    worker_capacity,
};
use crate::sources::Sessions;
use crate::sysinfo;
use crate::time::Timestamp;
use crate::workers;

/// Finished runs listed under "recent".
const RECENT_SHOWN: usize = 40;
/// Estimates are scaled for load only when that changes them by at least this much.
const MIN_LOAD_EFFECT: f64 = 0.05;
const DAY_SECONDS: u64 = 86_400;

#[derive(Serialize)]
pub(super) struct StateDoc {
    now: f64,
    capacity: u32,
    capacity_default: u32,
    /// The ceiling of the + button.
    capacity_ui_max: u32,
    capacity_override: Option<OverrideView>,
    /// Whether any waiter is placed by the manual order.
    manual_order: bool,
    slots: Vec<SlotView>,
    running: Vec<RunView>,
    /// Services running or starting: they take no slot, so they are listed apart.
    services: Vec<RunView>,
    queued: Vec<RunView>,
    done: Vec<RunView>,
    #[serde(flatten)]
    machine: sysinfo::Snapshot,
    load: Option<[f64; 3]>,
    ncpu: usize,
    inbox: PathBuf,
    /// The home directory, which the page writes as `~` in commands.
    home: PathBuf,
    state_dir: PathBuf,
    history_count: usize,
    metrics_24h: Metrics,
    load_model: ModelView,
    /// With workers configured: this machine, then each worker, with its slots and
    /// readings. Empty without workers, when the page shows one machine.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    machines: Vec<MachineView>,
}

/// One machine's bar on the dashboard.
#[derive(Serialize)]
struct MachineView {
    name: String,
    local: bool,
    ssh: Option<String>,
    online: Option<bool>,
    capacity: u32,
    capacity_default: u32,
    capacity_override: Option<OverrideView>,
    paused: bool,
    /// Runs on it now.
    running: usize,
    /// Its readings, while fresh: `machine-stats` for a worker.
    stats: Option<serde_json::Value>,
    last_seen: Option<f64>,
    /// Why it is not taking work, while that lasts.
    reason: Option<String>,
    reason_until: Option<f64>,
}

#[derive(Serialize)]
struct OverrideView {
    slots: u32,
    until: f64,
}

#[derive(Serialize)]
struct SlotView {
    slot: u32,
    run_id: Option<crate::job::RunId>,
}

#[derive(Serialize)]
struct ModelView {
    #[serde(flatten)]
    model: LoadModel,
    factor: f64,
}

/// The last day: runs that ended, how long they waited against how long they ran, failures.
#[derive(Serialize, Default, Debug, PartialEq)]
struct Metrics {
    runs: usize,
    /// Of those, how many started at all.
    ran: usize,
    wait_seconds: u64,
    run_seconds: u64,
    waited_longer: usize,
    failures: usize,
}

/// What a poll copies out under the queue lock. Running runs' activity is shared with their
/// runners, which hold its lock while they read their logs, so it is read after the queue
/// lock is released, never under it.
struct Snapshot {
    running: Vec<(Record, Estimate, Arc<Mutex<Activity>>)>,
    queued: Vec<(Record, Estimate)>,
    /// The newest finished runs, as many as the page lists.
    recent: Vec<(Record, Estimate)>,
    /// Runs that finished in the last day, for the metrics.
    last_day: Vec<Record>,
    history_count: usize,
    model: LoadModel,
    typicals: Arc<Mutex<Typicals>>,
}

impl Snapshot {
    fn take(ctx: &Ctx) -> Snapshot {
        let q = queue::lock(&ctx.shared);
        let with_estimate = |r: &Record| (r.clone(), q.estimate(&r.job));
        let running = q
            .runs
            .values()
            .filter(|e| e.rec.state == State::Running)
            .map(|e| {
                let activity = e.live.as_ref().map(|l| l.activity.clone()).unwrap_or_default();
                (e.rec.clone(), q.estimate(&e.rec.job), activity)
            })
            .collect();
        let mut finished: Vec<&Record> = q.runs.values().map(|e| &e.rec).filter(|r| r.state.is_terminal()).collect();
        finished.sort_by_key(|r| std::cmp::Reverse(r.last_event()));
        let in_last_day = |r: &&&Record| r.finished_at.is_some_and(|f| f.elapsed().as_secs() <= DAY_SECONDS);
        Snapshot {
            running,
            queued: q.records(State::Queued).map(with_estimate).collect(),
            recent: finished.iter().take(RECENT_SHOWN).map(|r| with_estimate(r)).collect(),
            last_day: finished.iter().filter(in_last_day).map(|r| (*r).clone()).collect(),
            history_count: q.history.len(),
            model: q.model.clone(),
            typicals: q.typicals.clone(),
        }
    }
}

pub(super) fn build(ctx: &Ctx) -> StateDoc {
    let now = Timestamp::now();
    let snap = Snapshot::take(ctx);
    let sessions = Sessions::read(ctx.session_links);
    let cpus = process::cpu_count();
    let factor = snap.model.factor(process::load_now(), cpus);
    let scaled = (factor - 1.0).abs() >= MIN_LOAD_EFFECT;
    // Only what is still to come scales with today's load.
    let ahead = |rec: &Record, est: &Estimate| {
        let est = if scaled { est.scaled(factor) } else { est.clone() };
        let mut view = RunView::new(rec, &est, &sessions, now);
        view.load_factor = (scaled && est.p50.is_some()).then(|| (factor * 100.0).round() / 100.0);
        view
    };

    let mut running: Vec<RunView> = snap
        .running
        .iter()
        .map(|(rec, est, activity)| {
            let mut view = ahead(rec, est);
            let activity = crate::sync::lock(activity).clone();
            for note in activity.notes() {
                if !view.notes.contains(note) {
                    view.notes.push(note.clone());
                }
            }
            view.activity = activity.view(&crate::sync::lock(&snap.typicals));
            view
        })
        .collect();
    running.sort_by(|a, b| a.timing.elapsed.cmp(&b.timing.elapsed).reverse());
    let (services, running): (Vec<RunView>, Vec<RunView>) = running.into_iter().partition(RunView::is_service);

    let busy_groups: std::collections::HashSet<&str> =
        snap.running.iter().filter_map(|(r, _, _)| r.job.group.as_deref()).collect();
    let manual = read_manual_order(&ctx.paths);
    let by_id: std::collections::HashMap<_, _> = snap.queued.iter().map(|(r, e)| (&r.run_id, (r, e))).collect();
    let mut queued: Vec<RunView> = rank(snap.queued.iter().map(|(r, _)| r), &manual, now)
        .into_iter()
        .filter_map(|ranked| {
            let (rec, est) = by_id.get(&ranked.run_id)?;
            let mut view = ahead(rec, est);
            (view.overdue, view.manual) = (Some(ranked.overdue), Some(ranked.manual));
            view.group_busy = rec.job.group.as_deref().map(|g| busy_groups.contains(g));
            Some(view)
        })
        .collect();

    let slots_env = ctx.settings.slots_env.as_deref();
    let standing = standing_capacity(&ctx.paths, slots_env).map_or(1, |(slots, _)| slots);
    let capacity = effective_capacity(&ctx.paths, slots_env).unwrap_or(1);
    estimate_starts(&running, &mut queued, capacity, now);
    let used: Vec<(u32, &crate::job::RunId)> =
        snap.running.iter().filter_map(|(r, _, _)| Some((r.slot?, &r.run_id))).collect();
    let top = used.iter().map(|(slot, _)| *slot).max().unwrap_or(0).max(capacity);
    let slots = (1..=top)
        .map(|n| SlotView { slot: n, run_id: used.iter().find(|(slot, _)| *slot == n).map(|(_, id)| (*id).clone()) })
        .collect();
    let done = snap.recent.iter().map(|(r, e)| RunView::new(r, e, &sessions, now)).collect();
    let machines = machines(ctx, &snap, capacity, standing);
    StateDoc {
        now: now.epoch_seconds(),
        capacity,
        capacity_default: standing,
        capacity_ui_max: ctx.settings.capacity_ceiling(standing),
        capacity_override: read_override(&ctx.paths)
            .map(|o| OverrideView { slots: o.slots, until: o.until.epoch_seconds() }),
        manual_order: queued.iter().any(|v| v.manual == Some(true)),
        slots,
        running,
        services,
        queued,
        done,
        machine: sysinfo::snapshot(),
        load: process::load_averages().map(|l| l.map(|x| (x * 10.0).round() / 10.0)),
        ncpu: cpus,
        inbox: ctx.inbox.clone(),
        home: crate::paths::home(),
        state_dir: ctx.paths.state.clone(),
        history_count: snap.history_count,
        metrics_24h: metrics(&snap.last_day, now),
        load_model: ModelView { model: snap.model, factor: (factor * 100.0).round() / 100.0 },
        machines,
    }
}

/// This machine and each worker, when there are workers.
fn machines(ctx: &Ctx, snap: &Snapshot, capacity: u32, standing: u32) -> Vec<MachineView> {
    let workers = load_config(&ctx.paths).map(|c| c.workers).unwrap_or_default();
    if workers.is_empty() {
        return Vec::new();
    }
    let running_on =
        |m: Option<&str>| snap.running.iter().filter(|(r, _, _)| r.machine.as_deref() == m && !r.job.service).count();
    let override_view = |o: crate::settings::Override| OverrideView { slots: o.slots, until: o.until.epoch_seconds() };
    let mut out = vec![MachineView {
        name: "local".into(),
        local: true,
        ssh: None,
        online: Some(true),
        capacity,
        capacity_default: standing,
        capacity_override: read_override(&ctx.paths).map(override_view),
        paused: false,
        running: running_on(None),
        stats: None,
        last_seen: None,
        reason: None,
        reason_until: None,
    }];
    for w in workers {
        let live = workers::pool().get(&w.name);
        let capacity = worker_capacity(&ctx.paths, &w);
        out.push(MachineView {
            local: false,
            ssh: Some(w.ssh.clone()),
            online: live.online,
            capacity,
            capacity_default: w.slots,
            capacity_override: read_worker_override(&ctx.paths, &w.name).map(override_view),
            paused: capacity == 0,
            running: running_on(Some(&w.name)),
            stats: live.fresh_stats().cloned(),
            last_seen: live.last_seen.map(Timestamp::epoch_seconds),
            reason: live.unavailable().map(String::from),
            reason_until: live.unavailable().and(live.unavailable_until).map(Timestamp::epoch_seconds),
            name: w.name,
        });
    }
    out
}

/// When each waiter should start, shown as its estimated start.
fn estimate_starts(running: &[RunView], queued: &mut [RunView], capacity: u32, now: Timestamp) {
    let now = now.epoch_seconds();
    let busy = running.iter().map(|r| r.timing.remaining_estimate.map(|left| now + left as f64));
    let mut free_at: Vec<Option<f64>> = busy.collect();
    free_at.resize(free_at.len().max(capacity as usize), Some(now));
    let durations: Vec<Option<u64>> = queued.iter().map(RunView::typical_seconds).collect();
    for (view, start) in queued.iter_mut().zip(start_times(free_at, &durations)) {
        let eta_start = start.unwrap_or(now);
        view.eta =
            Some(Eta { eta_start, eta_wait: (eta_start - now).max(0.0) as u64, eta_open_ended: start.is_none() });
    }
}

/// Simulates the queue: each waiter, in order, takes the slot that frees first and holds it
/// for its expected duration. `free_at` is when each slot frees, None when that is unknown
/// (a run with no estimate); a waiter with no estimate makes its slot's next time unknown.
/// A slot with an unknown time is used only when no slot has a known one, since a known
/// time is a bound: the waiter starts then at the latest. None means the start is unknown.
fn start_times(mut free_at: Vec<Option<f64>>, durations: &[Option<u64>]) -> Vec<Option<f64>> {
    let mut starts = Vec::with_capacity(durations.len());
    for duration in durations {
        let Some(slot) = (0..free_at.len()).min_by(|&a, &b| match (free_at[a], free_at[b]) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }) else {
            break;
        };
        let start = free_at[slot];
        starts.push(start);
        free_at[slot] = start.zip(*duration).map(|(s, d)| s + d as f64);
    }
    starts
}

fn metrics(finished: &[Record], now: Timestamp) -> Metrics {
    let mut m = Metrics::default();
    for r in finished {
        let Some(ended) = r.finished_at.filter(|f| f.until(now).as_secs() <= DAY_SECONDS) else { continue };
        m.runs += 1;
        m.failures += usize::from(r.state.is_failure());
        match r.started_at {
            Some(started) => {
                let (wait, ran) = (r.queued_at.until(started).as_secs(), started.until(ended).as_secs());
                m.ran += 1;
                m.wait_seconds += wait;
                m.run_seconds += ran;
                m.waited_longer += usize::from(wait > ran);
            }
            None => m.wait_seconds += r.queued_at.until(ended).as_secs(), // gave up while queued
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_slots_are_used_before_unknown_ones() {
        // Slot 1 runs a job with no estimate; slot 2 frees at 15.
        let starts = start_times(vec![None, Some(15.0)], &[Some(40), Some(60), None, Some(5)]);
        // The first waiter takes slot 2 at 15 and holds it to 55; the second follows at 55,
        // holding it to 115; the third starts at 115 but has no estimate, so after it every
        // slot's time is unknown.
        assert_eq!(starts, [Some(15.0), Some(55.0), Some(115.0), None]);
        assert_eq!(
            start_times(vec![Some(0.0), Some(0.0)], &[Some(10), Some(10), Some(10)]),
            [Some(0.0), Some(0.0), Some(10.0)]
        );
    }
}
