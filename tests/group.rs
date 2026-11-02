//! Concurrency groups: one running and one waiting per group, the newest waiter kept.

mod common;

use std::fs;
use std::time::Duration;

use common::{wait_for, Daemon};
use serde_json::json;

const T: Duration = Duration::from_secs(30);

#[test]
fn a_group_runs_one_job_at_a_time_and_keeps_only_the_newest_waiter() {
    let d = Daemon::start(&[("DRMANHATTAN_SLOTS", "4")]);
    let job = |cmd: &str| json!({"command": cmd, "cwd": d.work, "group": "deploy"});
    d.submit("first", job("sleep 30 & wait"));
    d.wait_state("first", "running", T);
    d.submit("second", job("echo second"));
    d.wait_state("second", "queued", T);
    d.submit("other", json!({"command": "echo other", "cwd": d.work}));
    assert_eq!(d.wait_exit("other", T), 0, "jobs outside the group take the free slots");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(d.status_of("second").unwrap()["state"], "queued", "it waits for its group, not for a slot");
    let waiter = d.status_json()["waiters"][0].clone();
    assert_eq!((&waiter["label"], &waiter["group"]), (&json!("second"), &json!("deploy")));
    assert_eq!(d.api_state()["queued"][0]["group_busy"], true);

    d.submit("third", job("echo third"));
    assert_eq!(d.wait_exit("second", T), 130, "the older waiter is replaced");
    let second = d.status_of("second").unwrap();
    assert_eq!(second["state"], "cancelled");
    assert_eq!(second["superseded_by"], d.run_id_of("third"));
    assert!(second["failure_reason"].as_str().unwrap().contains("replaced by a newer job in group deploy"));
    assert!(d.status_of("first").unwrap()["state"] == "running", "the running one is left alone");

    fs::write(d.inbox.join("first.cancel"), "").unwrap();
    assert_eq!(d.wait_exit("third", T), 0);
    assert!(d.log_of("third").contains("third"));
}

#[test]
fn a_waiter_replaced_while_it_prepares_is_stopped() {
    let d = Daemon::start(&[]);
    d.submit("old", json!({"command": "echo old", "cwd": d.work, "group": "g", "prepare": "sleep 30 & wait"}));
    wait_for(T, || (d.status_of("old")?["prepare_status"]["state"] == "running").then_some(())).unwrap();
    d.submit("new", json!({"command": "echo new", "cwd": d.work, "group": "g"}));
    assert_eq!(d.wait_exit("old", T), 130);
    assert!(d.status_of("old").unwrap()["failure_reason"].as_str().unwrap().contains("replaced"));
    assert_eq!(d.wait_exit("new", T), 0);
}
