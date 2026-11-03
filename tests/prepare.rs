//! `prepare`: a command that runs while its job waits, outside any slot.

mod common;

use std::fs;
use std::time::Duration;

use common::{group_exists, wait_for, Daemon};
use serde_json::json;

const T: Duration = Duration::from_secs(30);

#[test]
fn prepare_runs_while_the_job_waits_and_the_run_follows_it_in_the_log() {
    let d = Daemon::start(&[]);
    d.submit("blocker", d.job("sleep 30 & wait"));
    d.wait_state("blocker", "running", T);
    let marker = d.work.join("prepared");
    d.submit(
        "job",
        json!({"command": "echo run DRMANHATTAN_PREPARE=${DRMANHATTAN_PREPARE:-unset}", "cwd": d.work,
               "prepare": format!("echo prep DRMANHATTAN_PREPARE=$DRMANHATTAN_PREPARE; echo '::note prepped'; touch {}", marker.display())}),
    );
    // The only slot is busy, yet the prepare command runs.
    wait_for(T, || marker.exists().then_some(())).expect("the prepare command ran while the slot was busy");
    let waiter = wait_for(T, || {
        let st = d.status_json();
        let w = st["waiters"][0].clone();
        (w["prepare"] == "succeeded").then_some(w)
    })
    .unwrap_or_else(|| {
        panic!("status shows the prepare done: {} {:?}\n{}", d.status_json(), d.status_of("job"), d.daemon_log())
    });
    assert_eq!(waiter["label"], "job");
    assert_eq!(d.status_of("job").unwrap()["state"], "queued", "it still waits for the slot");
    assert_eq!(d.status_of("blocker").unwrap()["state"], "running");
    fs::write(d.inbox.join("blocker.cancel"), "").unwrap();
    assert_eq!(d.wait_exit("job", T), 0);
    let log = d.log_of("job");
    let (prep, run) =
        (log.find("prep DRMANHATTAN_PREPARE=1").unwrap(), log.find("run DRMANHATTAN_PREPARE=unset").unwrap());
    assert!(prep < run, "{log}");
    assert!(log.contains("drmanhattan · prepared in"), "{log}");
    let status = d.status_of("job").unwrap();
    assert_eq!(status["prepare_status"]["state"], "succeeded");
    assert_eq!(status["notes"], json!(["prepped"]), "a note from the prepare command stays");
    assert!(status["started_at"].is_string());
}

#[test]
fn a_failed_prepare_fails_the_job_before_it_takes_a_slot() {
    let d = Daemon::start(&[]);
    let ran = d.work.join("ran");
    d.submit(
        "job",
        json!({"command": format!("touch {}", ran.display()), "cwd": d.work,
               "prepare": "echo 'error: could not fetch dependencies'; exit 3"}),
    );
    assert_eq!(d.wait_exit("job", T), 3);
    let s = d.status_of("job").unwrap();
    assert_eq!(s["state"], "failed");
    assert_eq!(s["prepare_status"]["state"], "failed");
    assert!(s["slot"].is_null() && s["started_at"].is_null(), "it never took a slot: {s}");
    assert!(s["failure_reason"].as_str().unwrap().contains("prepare command failed, exit 3"), "{s}");
    assert_eq!(s["error_line"], "error: could not fetch dependencies");
    assert!(!ran.exists());
    assert!(d.log_of("job").contains("drmanhattan · prepare failed in"));
    let history = fs::read_to_string(d.data.join("history.jsonl")).unwrap_or_default();
    assert!(!history.contains("\"label\":\"job\""), "a run that never ran teaches no estimates");
}

#[test]
fn a_job_waits_for_its_prepare_even_with_a_free_slot_and_can_be_cancelled_during_it() {
    let d = Daemon::start(&[]);
    d.submit("slow", json!({"command": "echo ran", "cwd": d.work, "prepare": "sleep 30 & wait"}));
    let pgid = wait_for(T, || {
        let s = d.status_of("slow")?;
        (s["prepare_status"]["state"] == "running").then(|| s["process_group"].as_i64()).flatten()
    })
    .expect("the prepare command runs") as i32;
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(d.status_json()["active"], 0, "the free slot is not taken while it prepares");
    fs::write(d.inbox.join("slow.cancel"), "").unwrap();
    assert_eq!(d.wait_exit("slow", T), 130);
    let s = d.status_of("slow").unwrap();
    assert_eq!(s["state"], "cancelled");
    assert!(!group_exists(pgid), "the prepare command's group is gone");
    assert!(!d.log_of("slow").contains("\nran\n"));
}

#[test]
fn a_prepare_cut_short_by_the_daemon_dying_runs_again() {
    let mut d = Daemon::start(&[]);
    let count = d.work.join("count");
    d.submit(
        "job",
        json!({"command": "echo ran", "cwd": d.work,
               "prepare": format!("echo x >> {}; [ \"$(wc -l < {})\" -ge 2 ] || sleep 30", count.display(), count.display())}),
    );
    let pgid = wait_for(T, || {
        let s = d.status_of("job")?;
        (s["prepare_status"]["state"] == "running").then(|| s["process_group"].as_i64()).flatten()
    })
    .unwrap() as i32;
    d.kill_daemon();
    wait_for(T, || (!group_exists(pgid)).then_some(())).expect("the guard ended the prepare command");
    d.launch();
    assert_eq!(d.wait_exit("job", T), 0);
    assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 2, "it prepared again");
}

#[test]
fn a_cancel_as_the_prepare_command_ends_still_cancels_the_run() {
    let d = Daemon::start(&[]);
    // Many tries at the moment between the prepare command's exit and its record. A run may
    // only have started before its cancel was asked for (one that started first can still
    // finish: its command may exit before the cancel reaches it).
    for i in 0..30 {
        let name = format!("race{i}");
        d.submit(&name, json!({"command": "true", "cwd": d.work, "prepare": "true"}));
        std::thread::sleep(Duration::from_millis(100 + (i % 10) * 3));
        let asked = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        fs::write(d.inbox.join(format!("{name}.cancel")), "").unwrap();
        let code = d.wait_exit(&name, T);
        let s = d.status_of(&name).unwrap();
        // Succeeded, or cancelled before it started (130) or while running (143).
        let ended = (code, s["state"].as_str());
        assert!(matches!(ended, (0, Some("succeeded")) | (130 | 143, Some("cancelled"))), "{name}: {s}");
        if let Some(started) = s["started_at"].as_str() {
            assert!(common::epoch(&json!(started)) <= asked + 0.05, "{name} started after its cancel: {s}");
        }
    }
}
