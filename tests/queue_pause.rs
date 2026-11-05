//! Pausing the whole queue: running jobs stop where they are, nothing starts, and paused time
//! counts toward no timeout; resume carries on.

mod common;

use std::fs;
use std::time::Duration;

use common::{wait_for, Daemon};
use serde_json::json;

const T: Duration = Duration::from_secs(30);

/// A job that counts to `n` in `count`, a step every 0.1s.
fn counter(d: &Daemon, n: u32) -> serde_json::Value {
    d.job(&format!("i=0; while [ $i -lt {n} ]; do i=$((i+1)); echo $i > count; sleep 0.1; done"))
}

fn count(d: &Daemon) -> u32 {
    fs::read_to_string(d.work.join("count")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

fn run(d: &Daemon, args: &[&str]) -> String {
    let out = d.cmd(args).output().unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_paused_job_stops_and_its_timeout_ignores_the_pause() {
    let d = Daemon::start(&[]);
    // About 3s of work against a 4s timeout, paused for longer than the timeout.
    let mut job = counter(&d, 30);
    job["timeout"] = json!(4);
    d.submit("counter", job);
    d.wait_state("counter", "running", T);
    wait_for(T, || (count(&d) >= 3).then_some(())).expect("it starts counting");

    assert!(run(&d, &["pause"]).contains("paused since"));
    wait_for(T, || d.log_of("counter").contains("paused: stopped").then_some(())).expect("the runner stops it");
    let at_pause = count(&d);
    d.submit("later", d.job("true"));
    std::thread::sleep(Duration::from_secs(5));
    assert!(count(&d) <= at_pause + 1, "it went on counting while paused: {} after {at_pause}", count(&d));
    assert_eq!(d.status_of("counter").unwrap()["state"], "running", "paused time is not running time");
    assert_eq!(d.status_of("later").unwrap()["state"], "queued", "nothing starts while paused");
    let status = d.status_json();
    assert_eq!(status["state"], "paused");
    assert!(status["queue_paused_since"].is_string());
    assert_eq!(status["running"][0]["paused"], true);
    assert!(status["running"][0]["elapsed_seconds"].as_u64().unwrap() < 4, "{status}");
    assert!(run(&d, &["status"]).contains("paused since="));
    let state = d.api_state();
    assert!(state["queue_paused_since"].is_number());
    assert_eq!(state["running"][0]["paused"], true);

    assert!(run(&d, &["resume"]).contains("resumed"));
    assert_eq!(d.wait_exit("counter", T), 0);
    let s = d.status_of("counter").unwrap();
    assert_eq!(s["state"], "succeeded");
    assert!(s["paused_seconds"].as_u64().unwrap() >= 5, "{s}");
    assert_eq!(count(&d), 30);
    assert!(d.log_of("counter").contains("resumed after"));
    assert_eq!(d.wait_exit("later", T), 0);
}

#[test]
fn a_paused_job_can_be_cancelled() {
    let d = Daemon::start(&[]);
    d.submit("long", d.job("sleep 60"));
    d.wait_state("long", "running", T);
    run(&d, &["pause"]);
    wait_for(T, || d.log_of("long").contains("paused: stopped").then_some(())).expect("the runner stops it");
    fs::write(d.inbox.join("long.cancel"), "").unwrap();
    assert_ne!(d.wait_exit("long", T), 0);
    assert_eq!(d.status_of("long").unwrap()["state"], "cancelled");
}

#[test]
fn the_pause_outlasts_a_restart_and_the_dashboard_resumes_it() {
    let mut d = Daemon::start(&[]);
    let headers = [("Content-Type", "application/json"), ("X-Drmanhattan", "1")];
    let (code, body) = d.post("/api/pause", r#"{"paused": true}"#, &headers);
    assert_eq!(code, 200, "{body}");
    d.stop_daemon();
    d.launch();
    d.submit("waiter", d.job("true"));
    d.wait_state("waiter", "queued", T);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(d.status_of("waiter").unwrap()["state"], "queued");
    assert_eq!(d.status_json()["state"], "paused");
    let (code, body) = d.post("/api/pause", r#"{"paused": false}"#, &headers);
    assert_eq!(code, 200, "{body}");
    assert_eq!(d.wait_exit("waiter", T), 0);
    assert!(d.api_state()["queue_paused_since"].is_null());
}

#[test]
fn pausing_after_current_lets_the_running_job_finish() {
    let d = Daemon::start(&[]);
    d.submit("current", counter(&d, 15));
    d.wait_state("current", "running", T);
    assert!(run(&d, &["pause", "--after-current"]).contains("after the current jobs"));
    d.submit("next", d.job("true"));
    d.wait_state("next", "queued", T);
    wait_for(T, || (d.status_json()["state"] == "pausing").then_some(())).expect("it says pausing");
    assert_eq!(d.status_json()["queue_pause_after_current"], true);
    assert_eq!(d.wait_exit("current", T), 0);
    assert!(!d.log_of("current").contains("paused: stopped"), "it was not stopped");
    assert_eq!(count(&d), 15);
    wait_for(T, || (d.status_json()["state"] == "paused").then_some(())).expect("then it says paused");
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(d.status_of("next").unwrap()["state"], "queued", "it stays paused until resumed");
    run(&d, &["resume"]);
    assert_eq!(d.wait_exit("next", T), 0);
}

#[test]
fn pausing_names_a_job_that_drives_a_simulator() {
    let d = Daemon::start(&[]);
    d.set_slots(2);
    // Only its words matter: it never runs xcodebuild.
    d.submit("uitests", d.job("sleep 30 # xcodebuild test -scheme App"));
    d.submit("plain", d.job("sleep 30"));
    d.wait_state("uitests", "running", T);
    d.wait_state("plain", "running", T);
    let out = run(&d, &["pause"]);
    assert!(out.contains("warning: uitests") && out.contains("may fail"), "{out}");
    assert!(!out.contains("warning: plain"), "{out}");
    let state = wait_for(T, || {
        let s = d.api_state();
        (s["queue_paused_since"].is_number() && s["running"].as_array()?.len() == 2).then_some(s)
    })
    .unwrap();
    let flagged: Vec<_> = state["running"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["may_not_pause_cleanly"] == true)
        .map(|r| r["label"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(flagged, ["uitests"]);
}
