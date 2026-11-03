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

use std::collections::HashMap;

use serde::Serialize;

use crate::history::HistoryRow;
use crate::job::State;

/// The load model needs this many successful runs that recorded their load ...
pub(crate) const LOAD_MODEL_MIN_RUNS: usize = 20;
/// ... spread over at least this much load per core.
const LOAD_MODEL_MIN_SPREAD: f64 = 0.5;
/// However loaded the machine, estimates are scaled by no less and no more than this.
const LOAD_FACTOR_RANGE: (f64, f64) = (0.6, 2.5);
/// Only the most recent rows count: the machine and the code change over time.
const RECENT_ROWS: usize = 500;

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

/// The estimate for a run with this category, label and variant (see the module comment),
/// on `machine` (a worker's name; None for this machine), from that machine's runs only.
pub(crate) fn estimate(
    history: &[HistoryRow],
    category: &str,
    label: &str,
    variant: &str,
    machine: Option<&str>,
) -> Estimate {
    let recent = &history[history.len().saturating_sub(RECENT_ROWS)..];
    let ok: Vec<&HistoryRow> =
        recent.iter().filter(|r| r.outcome == State::Succeeded && r.machine.as_deref() == machine).collect();
    let pick = |matches: &dyn Fn(&HistoryRow) -> bool, take: usize, min: usize| -> Option<Vec<u64>> {
        let durations: Vec<u64> = ok.iter().filter(|r| matches(r)).map(|r| r.millis()).collect();
        (durations.len() >= min).then(|| durations[durations.len().saturating_sub(take)..].to_vec())
    };
    let same_label = |r: &HistoryRow| r.category == category && r.label == label;
    let found = (!variant.is_empty())
        .then(|| pick(&|r| same_label(r) && r.variant == variant, 10, 3).map(|v| ("label+variant", v)))
        .flatten()
        .or_else(|| pick(&same_label, 10, 1).map(|v| ("label", v)))
        .or_else(|| pick(&|r| r.category == category, 20, 1).map(|v| ("category", v)));
    match found {
        Some((basis, samples)) => {
            Estimate { p50: Some(median(&samples)), p90: Some(quantile(&samples, 0.9)), basis: Some(basis), samples }
        }
        None => Estimate::default(),
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
