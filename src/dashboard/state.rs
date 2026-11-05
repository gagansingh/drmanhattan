//! `/api/state`: the whole dashboard as one JSON document, built on every poll.
//!
//! Finished runs come a page at a time, so the document stays small however many runs are
//! kept: `/api/state?filter=failed&search=cargo&from=<run id>` lists the `PAGE` runs that
//! match, starting at that run (at the newest, without `from`). A page that starts at a run
//! holds still as newer runs finish. `open=<run id>` adds that run, for the log pane.
//!
//! The queue lock is held to pick out the page's records (a filter and a search over the
//! kept runs, which allocate nothing for an ASCII search) and copy them and the estimates
//! (`Snapshot::take`); reading the other tools' session files, the settings and the machine
//! readings, and building the document, all happen after it is released.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::activity::{Activity, Typicals};
use crate::dashboard::run_view::{Eta, RunView};
use crate::dashboard::{http, Ctx};
use crate::estimate::{Estimate, LoadModel};
use crate::job::{Record, RunId, State};
use crate::process;
use crate::queue::{self, rank};
use crate::settings::{effective_capacity, load_config, read_manual_order, MAX_SLOTS};
use crate::sources::Sessions;
use crate::sysinfo;
use crate::time::Timestamp;
use crate::workers;

/// Finished runs on a page.
const PAGE: usize = 20;
/// The newest runs to finish, whatever the page shows, so the page can say what finished.
const LATEST: usize = 20;
/// Estimates are scaled for load only when that changes them by at least this much.
const MIN_LOAD_EFFECT: f64 = 0.05;
const DAY_SECONDS: u64 = 86_400;

#[derive(Serialize)]
pub(super) struct StateDoc {
    now: f64,
    capacity: u32,
    /// The highest number of slots the + button goes to.
    capacity_max: u32,
    /// Set when the daemon's `DRMANHATTAN_SLOTS` decides the slots, so − and + cannot.
    capacity_fixed: Option<String>,
    /// Since when the queue is paused (`drmanhattan pause`), as epoch seconds.
    queue_paused_since: Option<f64>,
    /// Paused with `--after-current`: the running jobs finish rather than being stopped.
    queue_pause_after_current: bool,
    /// Whether any waiter is placed by the manual order.
    manual_order: bool,
    slots: Vec<SlotView>,
    running: Vec<RunView>,
    /// Services running or starting: they take no slot, so they are listed apart.
    services: Vec<RunView>,
    queued: Vec<RunView>,
    /// A page of finished runs: those the query asks for, newest first.
    done: Vec<RunView>,
    finished: FinishedView,
    /// The run the query's `open` names, wherever it is.
    open: Option<RunView>,
    #[serde(flatten)]
    machine: sysinfo::Snapshot,
    load: Option<[f64; 3]>,
    ncpu: usize,
    inbox: PathBuf,
    version: &'static str,
    /// A newer release, when the daemon's daily check found one.
    update: Option<crate::update::Available>,
    auto_update: bool,
    /// The installed version, when the daemon has yet to restart into it.
    installed: Option<String>,
    /// What auto-update is doing, in words, for the footer's tooltip.
    auto_update_state: String,
    /// The update that installed this version, in the last week.
    updated: Option<crate::update::Updated>,
    /// Whether nothing runs or waits, so the page may offer to update now.
    idle: bool,
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

/// Which finished runs the page asks for, from the query string.
#[derive(Default)]
pub(super) struct Query {
    filter: Filter,
    /// Lowercase; matched against each run's name, category, folder, command and worker.
    search: String,
    /// The run the page starts at; None for the newest.
    from: Option<RunId>,
    open: Option<RunId>,
}

impl Query {
    pub(super) fn parse(url: &str) -> Query {
        let id = |key| http::query_param(url, key).and_then(|id| id.parse().ok());
        Query {
            filter: http::query_param(url, "filter").map_or(Filter::All, |f| Filter::parse(&f)),
            search: http::query_param(url, "search").unwrap_or_default().trim().to_lowercase(),
            from: id("from"),
            open: id("open"),
        }
    }

    fn matches(&self, r: &Record) -> bool {
        let search = self.search.as_str();
        let has = |text: &str| contains_folded(text, search);
        search.is_empty()
            || has(&r.job.label)
            || has(&r.job.category)
            || has(&r.job.cwd.to_string_lossy())
            || has(r.run_id.as_str())
            || r.job.command.iter().any(|word| has(word))
            || r.machine.as_deref().is_some_and(has)
    }
}

/// Whether `text` contains `needle` (already lowercase), ignoring case. An ASCII needle, the
/// usual case, is compared in place: this runs for every kept run under the queue lock.
fn contains_folded(text: &str, needle: &str) -> bool {
    if needle.is_ascii() {
        let needle = needle.as_bytes();
        text.len() >= needle.len() && text.as_bytes().windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
    } else {
        text.to_lowercase().contains(needle)
    }
}

/// The page's outcome filters.
#[derive(Default, Clone, Copy)]
enum Filter {
    #[default]
    All,
    Failed,
    Succeeded,
    Cancelled,
}

impl Filter {
    fn parse(text: &str) -> Filter {
        match text {
            "failed" => Filter::Failed,
            "succeeded" => Filter::Succeeded,
            "cancelled" => Filter::Cancelled,
            _ => Filter::All,
        }
    }

    fn keeps(self, state: State) -> bool {
        match self {
            Filter::All => true,
            Filter::Failed => state.is_failure(),
            Filter::Succeeded => state == State::Succeeded,
            Filter::Cancelled => state == State::Cancelled,
        }
    }
}

/// Where the page of finished runs is among all of them.
#[derive(Serialize, Default)]
struct FinishedView {
    /// The finished runs kept that match the search, by what each filter would show.
    counts: Counts,
    /// How many runs the filter shows are newer than this page: none on the newest page.
    newer: usize,
    /// The run the next older page starts at; None on the oldest page.
    older_from: Option<RunId>,
    /// The run the next newer page starts at; None when that page is the newest.
    newer_from: Option<RunId>,
    /// The newest runs to finish, whatever the query.
    latest: Vec<Outcome>,
}

#[derive(Serialize, Default)]
struct Counts {
    all: usize,
    failed: usize,
    succeeded: usize,
    cancelled: usize,
}

/// How a run ended, in brief.
#[derive(Serialize)]
struct Outcome {
    run_id: RunId,
    label: String,
    state: State,
    exit_status: Option<i32>,
}

/// The last day: runs that ended, how long they waited against how long they ran, failures.
#[derive(Serialize, Default, Debug, PartialEq)]
struct Metrics {
    runs: usize,
    /// Of those, how many started at all.
    ran: usize,
    wait_seconds: f64,
    run_seconds: f64,
    waited_longer: usize,
    failures: usize,
}

/// What a poll copies out under the queue lock. Running runs' activity is shared with their
/// runners, which hold its lock while they read their logs, so it is read after the queue
/// lock is released, never under it. Of the finished runs, only the page is copied.
struct Snapshot {
    running: Vec<(Record, Estimate, Arc<Mutex<Activity>>)>,
    queued: Vec<(Record, Estimate)>,
    page: Vec<(Record, Estimate)>,
    finished: FinishedView,
    open: Option<(Record, Estimate)>,
    metrics_24h: Metrics,
    history_count: usize,
    model: LoadModel,
    typicals: Arc<Mutex<Typicals>>,
    queue_paused_since: Option<Timestamp>,
    pause_after_current: bool,
}

impl Snapshot {
    fn take(ctx: &Ctx, query: &Query, now: Timestamp) -> Snapshot {
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
        finished.sort_by(|a, b| order(b).cmp(&order(a)));
        let (page, mut view) = page(&finished, query);
        view.latest = finished.iter().take(LATEST).map(|r| outcome(r)).collect();
        let mut snap = Snapshot {
            running,
            queued: q.records(State::Queued).map(with_estimate).collect(),
            page: page.into_iter().map(with_estimate).collect(),
            finished: view,
            open: query.open.as_ref().and_then(|id| q.runs.get(id)).map(|e| with_estimate(&e.rec)),
            metrics_24h: metrics(finished.iter().copied(), now),
            history_count: q.history.len(),
            model: q.model.clone(),
            typicals: q.typicals.clone(),
            queue_paused_since: q.queue_paused_since,
            pause_after_current: q.pause_after_current,
        };
        // The pause so far counts toward neither waiting nor running, as it will once resumed.
        if let Some(since) = snap.queue_paused_since {
            snap.running.iter_mut().for_each(|(r, _, _)| r.add_pause(since, now, snap.pause_after_current));
            let after_current = snap.pause_after_current;
            snap.queued.iter_mut().for_each(|(r, _)| r.add_pause(since, now, after_current));
        }
        snap
    }
}

/// The order of finished runs, oldest first: by when they ended, then by id.
fn order(r: &Record) -> (Timestamp, &RunId) {
    (r.last_event(), &r.run_id)
}

fn outcome(r: &Record) -> Outcome {
    Outcome { run_id: r.run_id.clone(), label: r.job.label.clone(), state: r.state, exit_status: r.exit_status }
}

/// The page the query asks for, from the finished runs newest first, and where it is. A
/// page starts at its `from` run, or where that run would be, so it holds still as runs
/// finish above it. A run no longer kept starts an empty page past the oldest, from which
/// the newer pages can still be reached.
fn page<'a>(finished: &[&'a Record], query: &Query) -> (Vec<&'a Record>, FinishedView) {
    let mut counts = Counts::default();
    let mut shown = Vec::new();
    for &r in finished.iter().filter(|r| query.matches(r)) {
        counts.all += 1;
        counts.failed += usize::from(Filter::Failed.keeps(r.state));
        counts.succeeded += usize::from(Filter::Succeeded.keeps(r.state));
        counts.cancelled += usize::from(Filter::Cancelled.keeps(r.state));
        if query.filter.keeps(r.state) {
            shown.push(r);
        }
    }
    let start = match &query.from {
        None => 0,
        Some(id) => match finished.iter().find(|r| &r.run_id == id) {
            Some(anchor) => shown.partition_point(|r| order(r) > order(anchor)),
            None => shown.len(),
        },
    };
    let end = shown.len().min(start + PAGE);
    let view = FinishedView {
        counts,
        newer: start,
        older_from: shown.get(end).map(|r| r.run_id.clone()),
        newer_from: start.checked_sub(PAGE).filter(|&n| n > 0).map(|n| shown[n].run_id.clone()),
        latest: Vec::new(),
    };
    (shown[start..end].to_vec(), view)
}

pub(super) fn build(ctx: &Ctx, query: &Query) -> StateDoc {
    let now = Timestamp::now();
    let snap = Snapshot::take(ctx, query, now);
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
            view.paused = snap.queue_paused_since.is_some() && rec.stopped_by_pause(snap.pause_after_current);
            view.may_not_pause_cleanly = view.paused && rec.may_not_pause_cleanly();
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
    running.sort_by(|a, b| b.timing.elapsed.unwrap_or(0.0).total_cmp(&a.timing.elapsed.unwrap_or(0.0)));
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
            view.paused = snap.queue_paused_since.is_some();
            view.group_busy = rec.job.group.as_deref().map(|g| busy_groups.contains(g));
            Some(view)
        })
        .collect();

    let slots_env = ctx.settings.slots_env.as_deref();
    let capacity = effective_capacity(&ctx.paths, slots_env).unwrap_or(1);
    let quick_room = ctx.settings.max_quick.saturating_sub(running.iter().filter(|r| r.is_quick()).count());
    estimate_starts(&running, &mut queued, capacity, quick_room, now);
    // A quick run's number is not one of the slots.
    let used: Vec<(u32, &crate::job::RunId)> = snap
        .running
        .iter()
        .filter(|(r, _, _)| r.holds_slot())
        .filter_map(|(r, _, _)| Some((r.slot?, &r.run_id)))
        .collect();
    let top = used.iter().map(|(slot, _)| *slot).max().unwrap_or(0).max(capacity);
    let slots = (1..=top)
        .map(|n| SlotView { slot: n, run_id: used.iter().find(|(slot, _)| *slot == n).map(|(_, id)| (*id).clone()) })
        .collect();
    let done = snap.page.iter().map(|(r, e)| RunView::new(r, e, &sessions, now)).collect();
    let open = snap.open.as_ref().map(|(r, e)| RunView::new(r, e, &sessions, now));
    let machines = machines(ctx, &snap, capacity);
    StateDoc {
        now: now.epoch_seconds(),
        capacity,
        capacity_max: MAX_SLOTS,
        capacity_fixed: ctx.settings.slots_env.as_ref().map(|raw| format!("DRMANHATTAN_SLOTS={raw}")),
        queue_paused_since: snap.queue_paused_since.map(Timestamp::epoch_seconds),
        queue_pause_after_current: snap.pause_after_current,
        manual_order: queued.iter().any(|v| v.manual == Some(true)),
        slots,
        running,
        services,
        queued,
        done,
        finished: snap.finished,
        open,
        machine: sysinfo::snapshot(),
        load: process::load_averages().map(|l| l.map(|x| (x * 10.0).round() / 10.0)),
        ncpu: cpus,
        inbox: ctx.inbox.clone(),
        version: crate::update::current(),
        update: crate::update::available(&ctx.paths),
        auto_update: load_config(&ctx.paths).is_ok_and(|c| c.auto_update),
        auto_update_state: crate::update::auto_update_summary(&ctx.paths),
        installed: crate::update::installed_version(),
        updated: crate::update::updated(&ctx.paths),
        idle: crate::update::idle(&ctx.paths),
        home: crate::paths::home(),
        state_dir: ctx.paths.state.clone(),
        history_count: snap.history_count,
        metrics_24h: snap.metrics_24h,
        load_model: ModelView { model: snap.model, factor: (factor * 100.0).round() / 100.0 },
        machines,
    }
}

/// This machine and each worker, when there are workers.
fn machines(ctx: &Ctx, snap: &Snapshot, capacity: u32) -> Vec<MachineView> {
    let workers = load_config(&ctx.paths).map(|c| c.workers).unwrap_or_default();
    if workers.is_empty() {
        return Vec::new();
    }
    let running_on =
        |m: Option<&str>| snap.running.iter().filter(|(r, _, _)| r.machine.as_deref() == m && r.holds_slot()).count();
    let mut out = vec![MachineView {
        name: "local".into(),
        local: true,
        ssh: None,
        online: Some(true),
        capacity,
        paused: false,
        running: running_on(None),
        stats: None,
        last_seen: None,
        reason: None,
        reason_until: None,
    }];
    for w in workers {
        let live = workers::pool().get(&w.name);
        let capacity = w.slots;
        out.push(MachineView {
            local: false,
            ssh: Some(w.ssh.clone()),
            online: live.online,
            capacity,
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

/// When each waiter should start, shown as its estimated start. A quick waiter that the
/// quick lane has room for, and whose group is free, starts now; any other is reckoned with
/// the slots, which it takes when the lane is full.
fn estimate_starts(running: &[RunView], queued: &mut [RunView], capacity: u32, quick_room: usize, now: Timestamp) {
    let now = now.epoch_seconds();
    let busy = running.iter().filter(|r| !r.is_quick()).map(|r| r.timing.remaining_estimate.map(|left| now + left));
    let mut free_at: Vec<Option<f64>> = busy.collect();
    free_at.resize(free_at.len().max(capacity as usize), Some(now));
    let mut room = quick_room;
    let (quick, slot): (Vec<&mut RunView>, Vec<&mut RunView>) = queued.iter_mut().partition(|v| {
        let starts_now = v.is_quick() && v.group_busy != Some(true) && room > 0;
        room -= usize::from(starts_now);
        starts_now
    });
    for view in quick {
        view.eta = Some(Eta { eta_start: now, eta_wait: 0, eta_open_ended: false });
    }
    let durations: Vec<Option<f64>> = slot.iter().map(|v| v.typical_seconds()).collect();
    for (view, start) in slot.into_iter().zip(start_times(free_at, &durations)) {
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
fn start_times(mut free_at: Vec<Option<f64>>, durations: &[Option<f64>]) -> Vec<Option<f64>> {
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
        free_at[slot] = start.zip(*duration).map(|(s, d)| s + d);
    }
    starts
}

fn metrics<'a>(finished: impl Iterator<Item = &'a Record>, now: Timestamp) -> Metrics {
    let mut m = Metrics::default();
    for r in finished {
        let Some(ended) = r.finished_at.filter(|f| f.until(now).as_secs() <= DAY_SECONDS) else { continue };
        m.runs += 1;
        m.failures += usize::from(r.state.is_failure());
        match r.started_at {
            Some(started) => {
                let (wait, ran) = (r.queued_at.until(started).as_secs_f64(), started.until(ended).as_secs_f64());
                m.ran += 1;
                m.wait_seconds += wait;
                m.run_seconds += ran;
                m.waited_longer += usize::from(wait > ran);
            }
            None => m.wait_seconds += r.queued_at.until(ended).as_secs_f64(), // gave up while queued
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
        let starts = start_times(vec![None, Some(15.0)], &[Some(40.0), Some(60.0), None, Some(5.0)]);
        // The first waiter takes slot 2 at 15 and holds it to 55; the second follows at 55,
        // holding it to 115; the third starts at 115 but has no estimate, so after it every
        // slot's time is unknown.
        assert_eq!(starts, [Some(15.0), Some(55.0), Some(115.0), None]);
        assert_eq!(
            start_times(vec![Some(0.0), Some(0.0)], &[Some(10.0), Some(10.0), Some(10.0)]),
            [Some(0.0), Some(0.0), Some(10.0)]
        );
    }
}

#[cfg(test)]
mod search_tests {
    #[test]
    fn search_ignores_case_without_allocating_for_ascii() {
        assert!(super::contains_folded("Cargo Build", "cargo b"));
        assert!(!super::contains_folded("cargo", "cargo build"));
        assert!(super::contains_folded("Größe", "größe"));
    }
}
