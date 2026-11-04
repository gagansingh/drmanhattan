//! How long a run will take, from the timing history, and how the machine's load stretches it.
//!
//! One rule serves both the queue (shortest expected first) and the dashboard (the progress
//! bar and the estimated start): the successful runs most like this one, narrowest first:
//!
//! 1. same category, label and variant, when there are at least 3 (the last 10);
//! 2. same category and label (the last 10);
//! 3. same category (the last 20).
//!
//! With none of those there is no estimate: unrelated jobs say nothing about this one.
//! The median of the matched runs is the typical duration (p50); their 90th percentile is p90.
//!
//! The same history decides a job's lane (`HistoryIndex::lane`): a command that has nearly
//! always ended within seconds here is quick, and starts without waiting for a slot.
//!
//! The rows are indexed by what they match on, each key keeping its own most recent runs,
//! so a build's estimate survives any number of small commands run since.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use serde::Serialize;

use crate::history::HistoryRow;
use crate::job::State;
use crate::time::human;

/// The load model needs this many successful runs that recorded their load ...
pub(crate) const LOAD_MODEL_MIN_RUNS: usize = 20;
/// ... spread over at least this much load per core.
const LOAD_MODEL_MIN_SPREAD: f64 = 0.5;
/// However loaded the machine, estimates are scaled by no less and no more than this.
const LOAD_FACTOR_RANGE: (f64, f64) = (0.6, 2.5);
/// How many of its most recent runs each key keeps: the most any rule reads.
const KEEP: usize = 20;
/// A run that ends within this is quick ...
pub(crate) const QUICK: Duration = Duration::from_secs(30);
/// ... and a command is judged on at least this many of its runs here.
const QUICK_MIN_RUNS: usize = 3;
/// Read-only tools that are quick until their runs here say otherwise, so they do not wait
/// behind a build the first few times they are used.
const QUICK_TOOLS: &[&str] = &["cat", "diff", "gh", "git", "grep", "head", "jq", "ls", "rg", "tail", "wc", "which"];

/// The median of a non-empty slice; for an even count, the mean of the two middle values.
pub(crate) fn median(values: &[u64]) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2
    }
}

/// Nearest-rank quantile of a non-empty slice.
pub(crate) fn quantile(values: &[u64], q: f64) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Estimate {
    /// Milliseconds.
    pub(crate) p50: Option<u64>,
    pub(crate) p90: Option<u64>,
    /// Which rule matched: "label+variant", "label" or "category".
    pub(crate) basis: Option<&'static str>,
    /// The matched durations, in milliseconds.
    pub(crate) samples: Vec<u64>,
}

/// One key's most recent runs, in milliseconds, oldest first.
#[derive(Default)]
struct Runs {
    /// Successful runs: what estimates read.
    ok: VecDeque<u64>,
    /// Runs that say how long the command takes, whether it succeeded or not: what the lane
    /// reads. A run cut short (cancelled, timed out, abandoned) counts only once it has
    /// outlasted `QUICK`: a lower bound, it then says the command is not quick.
    timed: VecDeque<u64>,
}

impl Runs {
    fn add(&mut self, row: &HistoryRow) {
        let millis = row.millis();
        let push = |runs: &mut VecDeque<u64>| {
            runs.push_back(millis);
            if runs.len() > KEEP {
                runs.pop_front();
            }
        };
        if row.outcome == State::Succeeded {
            push(&mut self.ok);
        }
        if matches!(row.outcome, State::Succeeded | State::Failed) || millis > QUICK.as_millis() as u64 {
            push(&mut self.timed);
        }
    }
}

fn last(runs: &VecDeque<u64>, n: usize) -> Vec<u64> {
    runs.iter().skip(runs.len().saturating_sub(n)).copied().collect()
}

/// Which way a waiting job goes: a quick one starts without waiting for a slot.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Lane {
    pub(crate) quick: bool,
    /// Why, in words, for `status` and the dashboard.
    pub(crate) reason: String,
}

/// Every timing row, indexed by the keys the rules match on.
#[derive(Default)]
pub(crate) struct HistoryIndex {
    variants: HashMap<(Option<String>, String, String, String), Runs>,
    labels: HashMap<(Option<String>, String, String), Runs>,
    categories: HashMap<(Option<String>, String), Runs>,
}

impl HistoryIndex {
    pub(crate) fn of(rows: &[HistoryRow]) -> HistoryIndex {
        let mut index = HistoryIndex::default();
        for row in rows {
            index.add(row);
        }
        index
    }

    pub(crate) fn add(&mut self, row: &HistoryRow) {
        let (m, c, l) = (row.machine.clone(), row.category.clone(), row.label.clone());
        if !row.variant.is_empty() {
            self.variants.entry((m.clone(), c.clone(), l.clone(), row.variant.clone())).or_default().add(row);
        }
        self.labels.entry((m.clone(), c.clone(), l)).or_default().add(row);
        self.categories.entry((m, c)).or_default().add(row);
    }

    fn variant(&self, machine: Option<&str>, category: &str, label: &str, variant: &str) -> Option<&Runs> {
        if variant.is_empty() {
            return None;
        }
        self.variants.get(&(machine.map(String::from), category.into(), label.into(), variant.into()))
    }

    fn label(&self, machine: Option<&str>, category: &str, label: &str) -> Option<&Runs> {
        self.labels.get(&(machine.map(String::from), category.into(), label.into()))
    }

    fn category(&self, machine: Option<&str>, category: &str) -> Option<&Runs> {
        self.categories.get(&(machine.map(String::from), category.into()))
    }

    /// The estimate for a run with this category, label and variant (see the module
    /// comment), on `machine` (a worker's name; None for this machine), from that machine's
    /// runs only.
    pub(crate) fn estimate(&self, category: &str, label: &str, variant: &str, machine: Option<&str>) -> Estimate {
        let pick = |runs: Option<&Runs>, take: usize, min: usize| {
            runs.filter(|r| r.ok.len() >= min).map(|r| last(&r.ok, take))
        };
        let found = pick(self.variant(machine, category, label, variant), 10, 3)
            .map(|v| ("label+variant", v))
            .or_else(|| pick(self.label(machine, category, label), 10, 1).map(|v| ("label", v)))
            .or_else(|| pick(self.category(machine, category), 20, 1).map(|v| ("category", v)));
        match found {
            Some((basis, samples)) => Estimate {
                p50: Some(median(&samples)),
                p90: Some(quantile(&samples, 0.9)),
                basis: Some(basis),
                samples,
            },
            None => Estimate::default(),
        }
    }

    /// Whether a job with this category, label and variant is quick on this machine:
    ///
    /// 1. its own runs (label and variant, else label), when there are at least 3: quick if 9
    ///    in 10 of the last 10 took under 30 s;
    /// 2. else its category's, when there are at least 3: quick only if every one of the last
    ///    20 did, since a new label may be the category's heavy work (`cargo build` among
    ///    `cargo fmt`s);
    /// 3. else a read-only tool none of whose runs here took longer is quick;
    /// 4. anything else is new here, and waits for a slot.
    ///
    /// A shell script is named after its first program, so it is judged as that program only
    /// when it is plain commands and every other program it runs is a read-only tool
    /// (`git log | head`); a script that runs more (`git pull && cargo build`), or whose
    /// programs cannot be listed (`naming::script_shape`), waits for a slot.
    pub(crate) fn lane(&self, category: &str, label: &str, variant: &str, script: Result<&[String], &str>) -> Lane {
        match script {
            Err(why) => return Lane { quick: false, reason: format!("a script that {why}") },
            Ok(later) => {
                if let Some(other) = later.iter().find(|c| !QUICK_TOOLS.contains(&c.as_str())) {
                    return Lane { quick: false, reason: format!("a script that also runs {other}") };
                }
            }
        }
        let quick = QUICK.as_millis() as u64;
        let seconds = human(QUICK);
        let own = [self.variant(None, category, label, variant), self.label(None, category, label)]
            .into_iter()
            .flatten()
            .find(|r| r.timed.len() >= QUICK_MIN_RUNS);
        if let Some(runs) = own {
            let recent = last(&runs.timed, 10);
            let long = recent.iter().filter(|m| **m >= quick).count();
            let n = recent.len();
            return if quantile(&recent, 0.9) < quick {
                Lane { quick: true, reason: format!("{} of its last {n} runs here took under {seconds}", n - long) }
            } else {
                Lane { quick: false, reason: format!("{long} of its last {n} runs here took {seconds} or more") }
            };
        }
        let runs = self.category(None, category).map(|r| last(&r.timed, 20)).unwrap_or_default();
        let longest = runs.iter().copied().max().unwrap_or(0);
        if longest >= quick {
            let took = human(Duration::from_millis(longest));
            return Lane { quick: false, reason: format!("a recent {category} run here took {took}") };
        }
        if runs.len() >= QUICK_MIN_RUNS {
            return Lane { quick: true, reason: format!("every recent {category} run here took under {seconds}") };
        }
        if QUICK_TOOLS.contains(&category) {
            return Lane { quick: true, reason: format!("{category} usually takes seconds") };
        }
        Lane { quick: false, reason: format!("new here: fewer than {QUICK_MIN_RUNS} runs to go by") }
    }
}

impl Estimate {
    /// The estimate scaled for the current load.
    pub(crate) fn scaled(&self, factor: f64) -> Estimate {
        let f = |v: u64| (v as f64 * factor) as u64;
        Estimate {
            p50: self.p50.map(f),
            p90: self.p90.map(f),
            basis: self.basis,
            samples: self.samples.iter().map(|v| f(*v)).collect(),
        }
    }

    /// Expected milliseconds left after `elapsed` milliseconds. Past p50 it is the median of the past runs that
    /// outlasted `elapsed`; None once this run has outlasted every one of them.
    pub(crate) fn remaining(&self, elapsed: u64) -> Option<u64> {
        let p50 = self.p50?;
        if elapsed < p50 {
            return Some(p50 - elapsed);
        }
        let longer: Vec<u64> = self.samples.iter().copied().filter(|s| *s > elapsed).collect();
        (!longer.is_empty()).then(|| median(&longer) - elapsed)
    }
}

/// How much longer runs take under load: duration / the label's median = a + b × (load per
/// core), fitted over successful runs that recorded their load. Inactive until there are
/// enough runs at varied load. Refitted when a run finishes.
#[derive(Clone, Debug, Serialize, Default, PartialEq)]
pub(crate) struct LoadModel {
    pub(crate) active: bool,
    /// Runs the fit could use, and how many it needs.
    pub(crate) runs: usize,
    pub(crate) need: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) a: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) b: Option<f64>,
    /// The median load per core of the fitted runs: the load at which the factor is 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) x_ref: Option<f64>,
}

impl LoadModel {
    pub(crate) fn fit(history: &[HistoryRow], cpus: usize) -> LoadModel {
        let mut groups: HashMap<(&str, &str), Vec<(u64, f64)>> = HashMap::new();
        // This machine's load says nothing about a worker's runs.
        for r in history.iter().filter(|r| r.outcome == State::Succeeded && r.machine.is_none()) {
            if let Some(load) = r.load {
                groups.entry((&r.category, &r.label)).or_default().push((r.seconds, load));
            }
        }
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for runs in groups.values().filter(|runs| runs.len() >= 3) {
            let base = median(&runs.iter().map(|(s, _)| *s).collect::<Vec<_>>()) as f64;
            if base > 0.0 {
                for (seconds, load) in runs {
                    xs.push(load / cpus as f64);
                    ys.push(*seconds as f64 / base);
                }
            }
        }
        let mut model = LoadModel { runs: xs.len(), need: LOAD_MODEL_MIN_RUNS, ..LoadModel::default() };
        let spread = xs.iter().copied().fold(f64::MIN, f64::max) - xs.iter().copied().fold(f64::MAX, f64::min);
        if xs.len() < LOAD_MODEL_MIN_RUNS || spread < LOAD_MODEL_MIN_SPREAD {
            return model;
        }
        let n = xs.len() as f64;
        let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
        let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
        let sxy: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
        // More load never makes a run faster; a fit that says so is noise.
        let b = if sxx > 0.0 { (sxy / sxx).max(0.0) } else { 0.0 };
        let mut sorted = xs;
        sorted.sort_by(f64::total_cmp);
        model.active = true;
        model.a = Some(my - b * mx);
        model.b = Some(b);
        model.x_ref = Some(sorted[sorted.len() / 2]);
        model
    }

    /// The factor estimates are multiplied by at this load (1 while the model is inactive).
    pub(crate) fn factor(&self, load_now: Option<f64>, cpus: usize) -> f64 {
        let (true, Some(a), Some(b), Some(x_ref), Some(load)) = (self.active, self.a, self.b, self.x_ref, load_now)
        else {
            return 1.0;
        };
        let reference = a + b * x_ref;
        if reference <= 0.0 {
            return 1.0;
        }
        ((a + b * load / cpus as f64) / reference).clamp(LOAD_FACTOR_RANGE.0, LOAD_FACTOR_RANGE.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Timestamp;

    fn row(category: &str, label: &str, variant: &str, seconds: u64, outcome: State) -> HistoryRow {
        HistoryRow {
            finished_at: Timestamp::now(),
            category: category.into(),
            label: label.into(),
            variant: variant.into(),
            seconds,
            exit_status: i32::from(outcome != State::Succeeded),
            outcome,
            load: None,
            machine: None,
            millis: None,
        }
    }

    fn estimate(h: &[HistoryRow], category: &str, label: &str, variant: &str, machine: Option<&str>) -> Estimate {
        HistoryIndex::of(h).estimate(category, label, variant, machine)
    }

    fn lane(h: &[HistoryRow], category: &str, label: &str) -> Lane {
        HistoryIndex::of(h).lane(category, label, "", Ok(&[]))
    }

    #[test]
    fn the_narrowest_matching_rule_wins() {
        let ok = State::Succeeded;
        let mut h = vec![row("make", "a", "", 10, ok); 5];
        h.extend(vec![row("make", "a", "cold", 30, ok); 3]);
        h.push(row("make", "b", "", 100, ok));
        h.push(row("make", "c", "", 50, State::Failed)); // failures never count
        let e = estimate(&h, "make", "a", "cold", None);
        assert_eq!((e.p50, e.basis), (Some(30_000), Some("label+variant")));
        let e = estimate(&h, "make", "a", "warm", None);
        assert_eq!((e.p50, e.basis), (Some(10_000), Some("label")));
        let e = estimate(&h, "make", "zzz", "", None);
        assert_eq!((e.p50, e.basis), (Some(10_000), Some("category")));
        assert_eq!(estimate(&h, "other", "zzz", "", None), Estimate::default(), "unrelated runs say nothing");
        assert_eq!(
            estimate(&h, "make", "a", "", Some("air")),
            Estimate::default(),
            "another machine's runs say nothing"
        );
        assert_eq!(estimate(&[], "make", "a", "", None), Estimate::default());
    }

    #[test]
    fn a_label_uses_its_last_ten_runs() {
        let mut h: Vec<HistoryRow> = (0..10).map(|_| row("t", "x", "", 1000, State::Succeeded)).collect();
        h.extend((0..10).map(|_| row("t", "x", "", 5, State::Succeeded)));
        assert_eq!(estimate(&h, "t", "x", "", None).p50, Some(5_000));
    }

    #[test]
    fn an_estimate_survives_any_number_of_other_runs() {
        let mut h = vec![row("swift", "swift build", "", 900, State::Succeeded)];
        h.extend((0..5000).map(|_| row("git", "git status", "", 0, State::Succeeded)));
        assert_eq!(estimate(&h, "swift", "swift build", "", None).p50, Some(900_000));
    }

    #[test]
    fn a_command_is_quick_when_nearly_all_its_runs_here_were() {
        let ok = State::Succeeded;
        // Its own runs decide: 9 of 10 under 30 s is quick, failures count, 2 of 10 is not.
        let mut h = vec![row("make", "make lint", "", 2, State::Failed); 9];
        h.push(row("make", "make lint", "", 45, ok));
        assert!(lane(&h, "make", "make lint").quick, "{:?}", lane(&h, "make", "make lint"));
        h.push(row("make", "make lint", "", 45, ok));
        let l = lane(&h, "make", "make lint");
        assert!(!l.quick && l.reason == "2 of its last 10 runs here took 30s or more", "{l:?}");
        // Fewer than 3 of its own: its category's, all of them under 30 s.
        let h: Vec<HistoryRow> = (0..5).map(|_| row("cargo", "cargo fmt", "", 1, ok)).collect();
        assert!(lane(&h, "cargo", "cargo build").quick);
        let mut h = h;
        h.push(row("cargo", "cargo test", "", 300, ok));
        let l = lane(&h, "cargo", "cargo build");
        assert!(!l.quick && l.reason.starts_with("a recent cargo run here took 5m"), "{l:?}");
        // New: a read-only tool is quick, anything else waits.
        assert!(lane(&[], "git", "git status").quick);
        let l = lane(&[], "swift", "swift build");
        assert!(!l.quick && l.reason.starts_with("new here"), "{l:?}");
        let slow_git = vec![row("git", "git clone", "", 120, ok)];
        assert!(!lane(&slow_git, "git", "git status").quick, "a tool's own runs overrule the list");
    }

    #[test]
    fn a_variant_with_runs_of_its_own_decides_for_itself() {
        let ok = State::Succeeded;
        let mut h: Vec<HistoryRow> = (0..5).map(|_| row("swift", "swift build", "warm", 10, ok)).collect();
        h.extend((0..3).map(|_| row("swift", "swift build", "cold", 600, ok)));
        let index = HistoryIndex::of(&h);
        assert!(index.lane("swift", "swift build", "warm", Ok(&[])).quick);
        assert!(!index.lane("swift", "swift build", "cold", Ok(&[])).quick);
        // Fewer than 3 of the variant's own: the label's, here 3 long runs of 8.
        let l = index.lane("swift", "swift build", "new", Ok(&[]));
        assert!(!l.quick && l.reason == "3 of its last 8 runs here took 30s or more", "{l:?}");
    }

    #[test]
    fn a_script_is_judged_as_its_first_program_only_when_the_rest_only_read() {
        let index = HistoryIndex::of(&[]);
        assert!(index.lane("git", "git", "", Ok(&["head".into()])).quick);
        let l = index.lane("git", "git", "", Ok(&["cargo".into()]));
        assert!(!l.quick && l.reason == "a script that also runs cargo", "{l:?}");
        let l = index.lane("git", "git", "", Err("uses for"));
        assert!(!l.quick && l.reason == "a script that uses for", "{l:?}");
    }

    #[test]
    fn a_run_cut_short_counts_only_once_it_ran_long() {
        let h = vec![row("make", "m", "", 1, State::Cancelled); 5];
        assert!(!lane(&h, "make", "m").quick, "short cancels say nothing: still new");
        let mut h: Vec<HistoryRow> = (0..3).map(|_| row("make", "m", "", 1, State::Succeeded)).collect();
        h.push(row("make", "m", "", 40, State::TimedOut));
        h.push(row("make", "m", "", 40, State::Cancelled));
        assert!(!lane(&h, "make", "m").quick, "two long cut-short runs of five");
        assert!(lane(&vec![row("x", "x", "", 1, State::Succeeded); 3], "x", "x").quick);
        let worker_runs: Vec<HistoryRow> = (0..5)
            .map(|_| HistoryRow { machine: Some("air".into()), ..row("make", "m", "", 1, State::Succeeded) })
            .collect();
        assert!(!lane(&worker_runs, "make", "m").quick, "a worker's runs say nothing about this machine");
    }

    #[test]
    fn quantiles_and_time_remaining() {
        assert_eq!(median(&[3, 1, 2]), 2);
        assert_eq!(median(&[4, 1, 3, 2]), 2);
        assert_eq!(quantile(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 0.9), 9);
        assert_eq!(quantile(&[7], 0.9), 7);
        let est = Estimate { p50: Some(10), p90: Some(20), basis: None, samples: vec![5, 10, 20, 40] };
        assert_eq!(est.remaining(4), Some(6));
        assert_eq!(est.remaining(15), Some(15), "past p50: the median of the longer runs (30) less elapsed");
        assert_eq!(est.remaining(50), None);
    }

    #[test]
    fn the_load_model_needs_enough_varied_runs() {
        let h: Vec<HistoryRow> = (0..30u64)
            .map(|i| HistoryRow { load: Some(i as f64), ..row("t", "x", "", 10 + i, State::Succeeded) })
            .collect();
        let m = LoadModel::fit(&h, 4);
        assert!(m.active && m.b.unwrap() > 0.0);
        assert!(m.factor(Some(28.0), 4) > m.factor(Some(1.0), 4));
        let flat = LoadModel::fit(&h[..5], 4);
        assert!(!flat.active);
        assert_eq!(flat.factor(Some(28.0), 4), 1.0);
    }
}
