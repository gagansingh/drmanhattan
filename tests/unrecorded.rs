//! A run whose results cannot be written when it ends (a full disk) must still read as
//! ended: `status <run>` shows its outcome, `wait` returns, and it holds no slot in
//! `status`. The daemon says so loudly and writes it again once it can.
//!
//! A directory made read-only stands in for a full disk: creating the temp file fails, as
//! it did with ENOSPC, on the same path through the code.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use common::{wait_for, Daemon};
use serde_json::Value;

const T: Duration = Duration::from_secs(30);

fn run(d: &Daemon, args: &[&str]) -> (i32, String) {
    let out = d.cmd(args).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

fn writable(dir: &Path, yes: bool) {
    fs::set_permissions(dir, fs::Permissions::from_mode(if yes { 0o700 } else { 0o500 })).unwrap();
}

/// The state the run's record under `state/runs/` says, as written on disk.
fn record_state(d: &Daemon, run_id: &str) -> String {
    let text = fs::read_to_string(d.state.join("runs").join(format!("{run_id}.json"))).unwrap_or_default();
    serde_json::from_str::<Value>(&text).map(|v| v["state"].as_str().unwrap_or("").to_string()).unwrap_or_default()
}

/// One slot; `blocker` holds it until `release` appears in the work directory.
fn queue_behind_blocker(d: &Daemon) -> String {
    d.set_slots(1);
    d.submit("blocker", d.job("while [ ! -e release ]; do sleep 0.05; done"));
    d.wait_state("blocker", "running", T);
    d.submit("job", d.job("echo building; exit 74"));
    d.wait_state("job", "queued", T);
    d.run_id_of("job")
}

/// The reported case: the record cannot be written from before the run starts until after
/// it fails, so on disk it stays `queued`; `.status.json` and `.exit` are written.
#[test]
fn a_run_whose_record_cannot_be_written_still_reads_as_failed() {
    let d = Daemon::start(&[]);
    let job = queue_behind_blocker(&d);
    let runs = d.state.join("runs");
    writable(&runs, false);
    fs::write(d.work.join("release"), "").unwrap();
    assert_eq!(d.wait_exit("job", T), 74);
    assert_eq!(record_state(&d, &job), "queued", "the record is stale, as in the report");

    let (code, out) = run(&d, &["status", &job]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), format!("run_id={job} state=failed job=job exit=74"));
    let (code, out) = run(&d, &["wait", &job, "--timeout", "10"]);
    assert_eq!(code, 74, "wait returns with the run's exit code: {out}");
    let status = d.status_json();
    assert_eq!(status["active"], 0, "no run holds the slot: {status}");
    assert_eq!(status["waiters"].as_array().unwrap().len(), 0, "{status}");
    assert!(
        d.daemon_log().contains(&format!("run {job} is failed, but that could not be written")),
        "{}",
        d.daemon_log()
    );

    writable(&runs, true);
    wait_for(T, || (record_state(&d, &job) == "failed").then_some(()))
        .unwrap_or_else(|| panic!("the record was never written again\n{}", d.daemon_log()));
    assert!(d.daemon_log().contains(&format!("run {job} is written down at last: failed")), "{}", d.daemon_log());
}

/// Nothing at all can be written as the run ends. `wait` cannot know yet and says it is
/// still waiting; once the files can be written it returns, without another state change.
#[test]
fn results_that_could_not_be_written_are_written_once_they_can() {
    let d = Daemon::start(&[]);
    d.submit("job", d.job("while [ ! -e go ]; do sleep 0.05; done; echo building; exit 74"));
    d.wait_state("job", "running", T);
    let job = d.run_id_of("job");
    let runs = d.state.join("runs");
    writable(&runs, false);
    writable(&d.inbox, false);
    fs::write(d.work.join("go"), "").unwrap();
    wait_for(T, || d.daemon_log().contains(&format!("run {job} is failed, but")).then_some(()))
        .unwrap_or_else(|| panic!("the daemon never said it could not write the run\n{}", d.daemon_log()));
    assert!(!d.inbox.join("job.exit").exists());
    let (code, out) = run(&d, &["wait", &job, "--timeout", "1"]);
    assert_eq!(code, 124, "nothing says it has ended yet: {out}");

    writable(&runs, true);
    writable(&d.inbox, true);
    let (code, out) = run(&d, &["wait", &job, "--timeout", "20"]);
    assert_eq!(code, 74, "{out}");
    assert_eq!(d.wait_exit("job", T), 74);
    assert_eq!(record_state(&d, &job), "failed");
    assert_eq!(d.status_json()["active"], 0);
}
