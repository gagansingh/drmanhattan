//! The order a person set by dragging queued runs on the dashboard: run ids, one per line,
//! first goes first. Ids that no longer name a waiting run are simply never matched.

use std::collections::HashMap;

use crate::error::Result;
use crate::fsutil;
use crate::job::RunId;
use crate::paths::Paths;

/// Run id -> position in the manual order.
pub(crate) type ManualOrder = HashMap<RunId, usize>;

pub(crate) fn read_manual_order(paths: &Paths) -> ManualOrder {
    let path = paths.manual_order();
    let mut order = ManualOrder::new();
    if !fsutil::is_own_file(&path) {
        return order;
    }
    for id in std::fs::read_to_string(&path).unwrap_or_default().lines().filter_map(|l| l.trim().parse::<RunId>().ok())
    {
        let next = order.len();
        order.entry(id).or_insert(next);
    }
    order
}

pub(crate) fn write_manual_order(paths: &Paths, run_ids: &[RunId]) -> Result<()> {
    if run_ids.is_empty() {
        return fsutil::remove_if_present(&paths.manual_order());
    }
    let text: String = run_ids.iter().map(|r| format!("{r}\n")).collect();
    fsutil::write_atomic(&paths.manual_order(), &text)
}
