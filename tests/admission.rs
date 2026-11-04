//! Admission: capacity, slots, and the order waiting jobs start in.

mod common;

use std::fs;
use std::time::Duration;

use common::{epoch, wait_for, Daemon};
use serde_json::json;

const T: Duration = Duration::from_secs(30);

#[test]
fn capacity_one_serialises_runs() {
    let d = Daemon::start(&[]);
    d.submit("first", d.job("sleep 4"));
    d.wait_state("first", "running", T);
    d.submit("second", d.job("true"));
    d.wait_state("second", "queued", T);
    let st = d.status_json();
    assert_eq!(st["state"], "busy");
    assert_eq!(st["active"], 1);
    assert_eq!(st["capacity"], 1);
    assert_eq!(d.waiters(), vec!["second"]);
    assert_eq!(d.wait_exit("second", T), 0);
    let a = d.status_of("first").unwrap();
    let b = d.status_of("second").unwrap();
    assert!(epoch(&b["started_at"]) >= epoch(&a["finished_at"]) - 0.5);
    assert_eq!(b["slot"], 1);
}

#[test]
fn two_slots_run_in_parallel() {
    let d = Daemon::start(&[("DRMANHATTAN_SLOTS", "2")]);
    d.submit("a", d.job("sleep 1"));
    d.submit("b", d.job("sleep 1"));
    d.wait_state("a", "running", T);
    d.wait_state("b", "running", T);
    let st = d.status_json();
    assert_eq!(st["active"], 2);
    let mut slots: Vec<u64> = st["running"].as_array().unwrap().iter().map(|r| r["slot"].as_u64().unwrap()).collect();
    slots.sort();
    assert_eq!(slots, vec![1, 2]);
    d.wait_exit("a", T);
    d.wait_exit("b", T);
}

fn started(d: &Daemon, name: &str) -> f64 {
    epoch(&d.status_of(name).unwrap()["started_at"])
}

#[test]
fn shortest_expected_first() {
    let mut d = Daemon::start(&[]);
    d.seed_history(&[
        ("sleep", "long", 100),
        ("sleep", "long", 100),
        ("sleep", "long", 100),
        ("sleep", "short", 5),
        ("sleep", "short", 5),
        ("sleep", "short", 5),
    ]);
    d.submit("blocker", d.job("sleep 2"));
    d.wait_state("blocker", "running", T);
    d.submit("long", json!({"command": "sleep 0.2", "cwd": d.work, "label": "long", "category": "sleep"}));
    d.wait_state("long", "queued", T);
    d.submit("short", json!({"command": "sleep 0.2", "cwd": d.work, "label": "short", "category": "sleep"}));
    d.wait_state("short", "queued", T);
    let st = d.status_json();
    let waiters = st["waiters"].as_array().unwrap();
    assert_eq!(waiters.iter().map(|w| w["label"].as_str().unwrap()).collect::<Vec<_>>(), vec!["short", "long"]);
    assert_eq!(waiters[0]["expected_seconds"], 5);
    assert_eq!(waiters[1]["expected_seconds"], 100);
    d.wait_exit("long", T);
    d.wait_exit("short", T);
    assert!(started(&d, "short") < started(&d, "long"));
}

#[test]
fn priority_goes_ahead_of_shorter_waiters() {
    let mut d = Daemon::start(&[]);
    d.seed_history(&[
        ("sleep", "short", 5),
        ("sleep", "short", 5),
        ("sleep", "short", 5),
        ("sleep", "big", 500),
        ("sleep", "big", 500),
        ("sleep", "big", 500),
    ]);
    d.submit("blocker", d.job("sleep 2"));
    d.wait_state("blocker", "running", T);
    d.submit("short", json!({"command": "sleep 0.2", "cwd": d.work, "label": "short", "category": "sleep"}));
    d.wait_state("short", "queued", T);
    d.submit(
        "big",
        json!({"command": "sleep 0.2", "cwd": d.work, "label": "big", "category": "sleep", "priority": true}),
    );
    d.wait_state("big", "queued", T);
    assert_eq!(d.waiters(), vec!["big", "short"]);
    assert_eq!(d.status_json()["waiters"][0]["priority"], true);
    d.wait_exit("big", T);
    d.wait_exit("short", T);
    assert!(started(&d, "big") < started(&d, "short"));
}

#[test]
fn manual_order_puts_listed_waiters_first() {
    let mut d = Daemon::start(&[]);
    d.seed_history(&[
        ("sleep", "long", 100),
        ("sleep", "long", 100),
        ("sleep", "long", 100),
        ("sleep", "short", 5),
        ("sleep", "short", 5),
        ("sleep", "short", 5),
    ]);
    d.submit("blocker", d.job("sleep 2.5"));
    d.wait_state("blocker", "running", T);
    d.submit("long", json!({"command": "sleep 0.2", "cwd": d.work, "label": "long", "category": "sleep"}));
    d.submit("short", json!({"command": "sleep 0.2", "cwd": d.work, "label": "short", "category": "sleep"}));
    d.wait_state("long", "queued", T);
    d.wait_state("short", "queued", T);
    assert_eq!(d.waiters(), vec!["short", "long"]);
    d.write_manual_order(&[&d.run_id_of("long")]);
    let st = d.status_json();
    assert_eq!(st["waiters"][0]["label"], "long");
    assert_eq!(st["waiters"][0]["manual"], true);
    assert_eq!(st["waiters"][1]["manual"], false);
    // Whole seconds, under the names scripts have read since 0.2.
    assert!(st["waiters"][0]["waited_seconds"].is_u64() && st["running"][0]["elapsed_seconds"].is_u64(), "{st}");
    d.wait_exit("long", T);
    d.wait_exit("short", T);
    assert!(started(&d, "long") < started(&d, "short"));
}

#[test]
fn overdue_waiter_is_admitted_ahead_of_shorter_newcomers() {
    // bound = max(floor 0, 2 x expected) = 4 s for `long`, 2 s for `short`
    let mut d = Daemon::start(&[("DRMANHATTAN_MAX_SKIP_SECONDS", "0")]);
    d.seed_history(&[
        ("sleep", "long", 2),
        ("sleep", "long", 2),
        ("sleep", "long", 2),
        ("sleep", "short", 1),
        ("sleep", "short", 1),
        ("sleep", "short", 1),
    ]);
    // The blocker holds the only slot until both waiters have gone overdue, then is cancelled.
    d.submit("blocker", d.job("sleep 60"));
    d.wait_state("blocker", "running", T);
    d.submit("long", json!({"command": "sleep 0.2", "cwd": d.work, "label": "long", "category": "sleep"}));
    d.wait_state("long", "queued", T);
    std::thread::sleep(Duration::from_millis(300));
    d.submit("short", json!({"command": "sleep 0.2", "cwd": d.work, "label": "short", "category": "sleep"}));
    d.wait_state("short", "queued", T);
    let st = d.status_json();
    assert_eq!(d.waiters(), vec!["short", "long"]);
    assert_eq!(st["waiters"][1]["skip_after_seconds"], 4);
    // `short` goes overdue first and keeps its place; once `long` is overdue too, the two
    // overdue waiters go by arrival, so `long` is admitted first.
    wait_for(Duration::from_secs(10), || {
        let st = d.status_json();
        let w = st["waiters"].as_array()?;
        (w.iter().any(|x| x["label"] == "long" && x["overdue"] == true)).then_some(())
    })
    .expect("long becomes overdue");
    assert_eq!(d.waiters(), vec!["long", "short"]);
    fs::write(d.inbox.join("blocker.cancel"), "").unwrap();
    d.wait_exit("long", T);
    d.wait_exit("short", T);
    assert!(started(&d, "long") < started(&d, "short"));
}

#[test]
fn capacity_override_raises_at_once_and_only_while_valid() {
    let d = Daemon::start(&[]);
    d.submit("blocker", d.job("sleep 3"));
    d.wait_state("blocker", "running", T);
    d.submit("waiter", d.job("sleep 0.2"));
    d.wait_state("waiter", "queued", T);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(d.status_of("waiter").unwrap()["state"], "queued");
    d.write_override(Some(2));
    d.wait_state("waiter", "running", Duration::from_secs(5));
    assert_eq!(d.status_json()["capacity"], 2);
    assert_eq!(d.status_json()["override"]["slots"], 2);
    // an expired override is ignored, and so is a malformed one
    fs::write(d.state.join("capacity-override"), "3 2020-01-01T00:00:00Z\n").unwrap();
    assert_eq!(d.status_json()["capacity"], 1);
    fs::write(d.state.join("capacity-override"), "lots\n").unwrap();
    assert_eq!(d.status_json()["capacity"], 1);
    d.wait_exit("blocker", T);
}

#[test]
fn lower_capacity_lands_under_load() {
    let d = Daemon::start(&[("DRMANHATTAN_SLOTS", "2")]);
    d.submit("a", d.job("sleep 1.5"));
    d.submit("b", d.job("sleep 3"));
    d.wait_state("a", "running", T);
    d.wait_state("b", "running", T);
    d.write_override(Some(1));
    d.submit("c", d.job("sleep 0.2"));
    d.wait_exit("a", T);
    std::thread::sleep(Duration::from_millis(500));
    // one slot is still held by b, capacity is 1: c must keep waiting
    assert_eq!(d.status_of("c").unwrap()["state"], "queued");
    d.wait_exit("b", T);
    assert_eq!(d.wait_exit("c", T), 0);
    assert!(started(&d, "c") >= epoch(&d.status_of("b").unwrap()["finished_at"]) - 0.5);
}

#[test]
fn status_says_which_waiters_are_quick_and_why() {
    let mut d = Daemon::start(&[]);
    d.seed_history(&[("make", "make lint", 2), ("make", "make lint", 3), ("make", "make lint", 2)]);
    d.submit("blocker", d.job("sleep 30"));
    d.wait_state("blocker", "running", T);
    d.submit("lint", json!({"command": ["make", "lint"], "cwd": d.work, "label": "make lint"}));
    d.submit("git", json!({"command": ["git", "--version"], "cwd": d.work, "label": "git"}));
    d.submit(
        "build",
        json!({"command": ["make", "build"], "cwd": d.work, "label": "make build", "category": "make-build"}),
    );
    d.wait_state("build", "queued", T);
    let st = d.status_json();
    let lane = |label: &str| {
        let w = st["waiters"].as_array().unwrap().iter().find(|w| w["label"] == label).unwrap().clone();
        (w["quick"].as_bool().unwrap(), w["lane_reason"].as_str().unwrap_or_default().to_string())
    };
    assert_eq!(lane("make lint"), (true, "3 of its last 3 runs here took under 30s".into()));
    assert_eq!(lane("git"), (true, "git usually takes seconds".into()));
    let (quick, reason) = lane("make build");
    assert!(!quick && reason.starts_with("new here"), "{reason}");
    fs::write(d.inbox.join("blocker.cancel"), "").unwrap();
    for name in ["lint", "git", "build"] {
        d.wait_exit(name, T);
    }
}
