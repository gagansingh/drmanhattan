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
    let mut d = Daemon::start(&[("DRMANHATTAN_QUICK", "0")]);
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
    let mut d = Daemon::start(&[("DRMANHATTAN_QUICK", "0")]);
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
    let mut d = Daemon::start(&[("DRMANHATTAN_QUICK", "0")]);
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
    let mut d = Daemon::start(&[("DRMANHATTAN_MAX_SKIP_SECONDS", "0"), ("DRMANHATTAN_QUICK", "0")]);
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
fn more_slots_apply_at_once_and_stay() {
    let mut d = Daemon::start(&[]);
    d.submit("blocker", d.job("sleep 3"));
    d.wait_state("blocker", "running", T);
    d.submit("waiter", d.job("sleep 0.2"));
    d.wait_state("waiter", "queued", T);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(d.status_of("waiter").unwrap()["state"], "queued");
    d.set_slots(40);
    d.wait_state("waiter", "running", Duration::from_secs(5));
    assert_eq!(d.status_json()["capacity"], 40, "no ceiling below the guard");
    d.wait_exit("blocker", T);
    // A timed setting from before 0.7.1 is cleared at start, and the count stays.
    let until = chrono::Utc::now() + chrono::Duration::hours(1);
    fs::write(d.state.join("capacity-override"), format!("3 {}\n", until.to_rfc3339())).unwrap();
    d.stop_daemon();
    d.launch();
    assert!(!d.state.join("capacity-override").exists());
    assert_eq!(d.status_json()["capacity"], 40);
    let out = d.cmd(&["slots", "257"]).output().unwrap();
    assert!(!out.status.success(), "the guard: {out:?}");
}

#[test]
fn lower_capacity_lands_under_load() {
    let d = Daemon::start(&[]);
    d.set_slots(2);
    d.submit("a", d.job("sleep 1.5"));
    d.submit("b", d.job("sleep 3"));
    d.wait_state("a", "running", T);
    d.wait_state("b", "running", T);
    d.set_slots(1);
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
fn quick_jobs_start_beside_a_busy_slot_and_say_why() {
    let mut d = Daemon::start(&[("DRMANHATTAN_QUICK", "2")]);
    d.seed_history(&[("make", "make lint", 2), ("make", "make lint", 3), ("make", "make lint", 2)]);
    d.submit("blocker", d.job("sleep 30"));
    d.wait_state("blocker", "running", T);
    // A command whose runs here were quick, and a read-only tool: both start at once.
    d.submit(
        "lint",
        json!({"command": ["sh", "-c", "sleep 2"], "cwd": d.work, "label": "make lint", "category": "make"}),
    );
    d.submit("git", json!({"command": ["git", "--version"], "cwd": d.work, "label": "git"}));
    // New here, and a script that runs more than a read-only tool: both wait for the slot.
    d.submit(
        "build",
        json!({"command": ["make", "build"], "cwd": d.work, "label": "make build", "category": "make-build"}),
    );
    d.submit("script", json!({"command": "git --version && make --version", "cwd": d.work}));
    let lint = d.wait_state("lint", "running", T);
    assert_eq!(lint["quick"], true);
    assert!(lint["slot"].as_u64().unwrap() > 16, "its DRMANHATTAN_SLOT is above any slot: {lint}");
    assert_eq!(d.wait_exit("git", T), 0);
    assert_eq!(d.status_of("git").unwrap()["quick"], true);
    let st = d.status_json();
    assert_eq!(st["active"], 1, "the quick run holds no slot: {st}");
    let waiter = |label: &str| {
        let w = st["waiters"].as_array().unwrap().iter().find(|w| w["label"] == label).cloned();
        let w = w.unwrap_or_else(|| panic!("{label} is not waiting: {st}"));
        (w["quick"].as_bool().unwrap(), w["lane_reason"].as_str().unwrap_or_default().to_string())
    };
    let (quick, reason) = waiter("make build");
    assert!(!quick && reason.starts_with("new here"), "{reason}");
    let (quick, reason) = waiter("script");
    assert!(!quick && reason == "a script that also runs make", "{reason}");
    assert!(d.log_of("lint").contains("started at once (quick)"), "{}", d.log_of("lint"));
    fs::write(d.inbox.join("blocker.cancel"), "").unwrap();
    for name in ["lint", "build", "script"] {
        d.wait_exit(name, T);
    }
}

#[test]
fn the_quick_lane_has_its_own_limit_and_can_be_turned_off() {
    let mut d = Daemon::start(&[("DRMANHATTAN_QUICK", "1")]);
    d.seed_history(&[("t", "t", 1), ("t", "t", 1), ("t", "t", 1)]);
    d.submit("blocker", d.job("sleep 30"));
    d.wait_state("blocker", "running", T);
    let job = || json!({"command": ["sh", "-c", "sleep 3"], "cwd": d.work, "label": "t", "category": "t"});
    d.submit("q1", job());
    d.wait_state("q1", "running", T);
    d.submit("q2", job());
    let q2 = d.wait_state("q2", "queued", T);
    assert_eq!(q2["quick"], true, "one quick place: q2 waits for q1 though it is quick");
    assert_eq!(d.wait_exit("q2", T), 0);
    assert!(started(&d, "q2") >= epoch(&d.status_of("q1").unwrap()["finished_at"]) - 0.5);
    fs::write(d.inbox.join("blocker.cancel"), "").unwrap();
    d.wait_exit("blocker", T);

    let mut d = Daemon::start(&[("DRMANHATTAN_QUICK", "0")]);
    d.seed_history(&[("t", "t", 1), ("t", "t", 1), ("t", "t", 1)]);
    d.submit("blocker", d.job("sleep 3"));
    d.wait_state("blocker", "running", T);
    d.submit("q", job());
    let w = d.wait_state("q", "queued", T);
    assert!(w.get("quick").is_none() && w["lane_reason"].as_str().unwrap().contains("off"), "{w}");
    assert_eq!(d.wait_exit("q", T), 0);
    assert!(started(&d, "q") >= epoch(&d.status_of("blocker").unwrap()["finished_at"]) - 0.5);
}

#[test]
fn a_quick_job_takes_a_free_slot_when_the_quick_lane_is_full() {
    let mut d = Daemon::start(&[("DRMANHATTAN_QUICK", "1"), ("DRMANHATTAN_SLOTS", "2")]);
    d.seed_history(&[("t", "t", 1), ("t", "t", 1), ("t", "t", 1)]);
    let job = || json!({"command": ["sh", "-c", "sleep 5"], "cwd": d.work, "label": "t", "category": "t"});
    d.submit("q1", job());
    let q1 = d.wait_state("q1", "running", T);
    assert_eq!(q1["quick"], true);
    d.submit("q2", job());
    let q2 = d.wait_state("q2", "running", T);
    assert!(q2.get("quick").is_none(), "it took a slot: {q2}");
    assert!(q2["slot"].as_u64().unwrap() <= 2, "{q2}");
    assert_eq!(d.status_json()["active"], 1);
    // A worker reports only what holds its slots: a quick run, or a service, leaves them free.
    let stats = d.cmd(&["machine-stats"]).output().unwrap();
    let stats: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(stats["running"], 1, "{stats}");
    d.wait_exit("q1", T);
    d.wait_exit("q2", T);
}

/// The light lane, open whatever the test machine's memory pressure.
fn light_daemon(extra: &[(&'static str, &'static str)]) -> Daemon {
    let mut env = vec![("DRMANHATTAN_LIGHT_PRESSURE", "off")];
    env.extend_from_slice(extra);
    Daemon::start(&env)
}

#[test]
fn light_jobs_start_beside_a_busy_slot_up_to_the_lanes_limit() {
    let d = light_daemon(&[("DRMANHATTAN_LIGHT", "1")]);
    d.submit("blocker", d.job("sleep 30"));
    d.wait_state("blocker", "running", T);
    let light = |secs: &str| json!({"command": ["sleep", secs], "cwd": d.work, "light": true});
    d.submit("l1", light("3"));
    let l1 = d.wait_state("l1", "running", T);
    assert_eq!(l1["light_lane"], true, "{l1}");
    assert!(l1["slot"].as_u64().unwrap() > 256, "its DRMANHATTAN_SLOT is above any slot: {l1}");
    assert!(d.log_of("l1").contains("started beside the slots (light)"), "{}", d.log_of("l1"));
    // One place in the lane: the second light job waits for it. A job that does not say it
    // is light waits for the slot, however short.
    d.submit("l2", light("1"));
    d.submit("plain", d.job("sleep 1"));
    let l2 = d.wait_state("l2", "queued", T);
    assert_eq!(l2["light_lane"], true, "{l2}");
    let st = d.status_json();
    assert_eq!((&st["active"], &st["light"], &st["light_capacity"]), (&json!(1), &json!(1), &json!(1)), "{st}");
    let waiter = |label: &str| {
        let w = st["waiters"].as_array().unwrap().iter().find(|w| w["label"] == label).cloned();
        w.unwrap_or_else(|| panic!("{label} is not waiting: {st}"))
    };
    assert_eq!(waiter("l2")["lane_reason"], "it says it is light", "{st}");
    assert_eq!(waiter("l2")["light_lane"], true, "{st}");
    assert_eq!(d.wait_exit("l2", T), 0);
    assert!(started(&d, "l2") >= epoch(&d.status_of("l1").unwrap()["finished_at"]) - 0.5);
    assert_eq!(d.status_of("l2").unwrap()["light_lane"], true, "it ran in the lane, beside the blocker");
    assert_eq!(d.status_of("plain").unwrap()["state"], "queued", "the slot is still the blocker's");
    fs::write(d.inbox.join("blocker.cancel"), "").unwrap();
    assert_eq!(d.wait_exit("plain", T), 0);
    assert!(d.status_of("plain").unwrap().get("light_lane").is_none());
}

#[test]
fn a_light_job_takes_a_free_slot_when_the_light_lane_is_full_or_off() {
    let d = light_daemon(&[("DRMANHATTAN_LIGHT", "1"), ("DRMANHATTAN_SLOTS", "2")]);
    let light = || json!({"command": ["sleep", "4"], "cwd": d.work, "light": true});
    d.submit("l1", light());
    assert_eq!(d.wait_state("l1", "running", T)["light_lane"], true);
    d.submit("l2", light());
    let l2 = d.wait_state("l2", "running", T);
    assert!(l2.get("light_lane").is_none() && l2["slot"].as_u64().unwrap() <= 2, "it took a slot: {l2}");
    assert_eq!(d.status_json()["active"], 1, "only the one in a slot holds one");
    // A worker reports only what holds its slots.
    let stats = d.cmd(&["machine-stats"]).output().unwrap();
    let stats: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(stats["running"], 1, "{stats}");
    d.wait_exit("l1", T);
    d.wait_exit("l2", T);

    // Off: `light` changes nothing, and the waiter says why.
    let d = light_daemon(&[("DRMANHATTAN_LIGHT", "0")]);
    d.submit("blocker", d.job("sleep 3"));
    d.wait_state("blocker", "running", T);
    d.submit("l", json!({"command": ["true"], "cwd": d.work, "light": true}));
    let w = d.wait_state("l", "queued", T);
    assert!(w.get("light_lane").is_none() && w["lane_reason"].as_str().unwrap().contains("light lane is off"), "{w}");
    assert_eq!(d.wait_exit("l", T), 0);
    assert!(started(&d, "l") >= epoch(&d.status_of("blocker").unwrap()["finished_at"]) - 0.5);
}

#[test]
fn a_light_job_keeps_its_group_and_a_service_cannot_be_light() {
    let d = light_daemon(&[]);
    let job = |secs: &str| json!({"command": ["sleep", secs], "cwd": d.work, "light": true, "group": "device"});
    d.submit("g1", job("3"));
    d.wait_state("g1", "running", T);
    d.submit("g2", job("1"));
    assert_eq!(d.wait_state("g2", "queued", T)["light_lane"], true, "the lane has room, its group does not");
    assert_eq!(d.wait_exit("g2", T), 0);
    assert!(started(&d, "g2") >= epoch(&d.status_of("g1").unwrap()["finished_at"]) - 0.5);
    d.submit("svc", json!({"command": ["sleep", "5"], "cwd": d.work, "service": true, "light": true}));
    let rejected = wait_for(T, || d.status_of("svc").filter(|s| s["state"] == "rejected" || s.get("error").is_some()));
    assert!(rejected.is_some_and(|s| s.to_string().contains("light")), "a service that says light is refused");
}
