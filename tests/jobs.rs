//! Running one job: its results, its log, rejection, cancel and timeout, and the inbox's files.

mod common;

use std::fs;
use std::time::Duration;

use common::{group_exists, wait_for, Daemon};
use serde_json::json;

const T: Duration = Duration::from_secs(30);

#[test]
fn job_runs_and_reports_beside_its_file() {
    let d = Daemon::start(&[]);
    d.submit("hello", json!({"command": "echo out; echo err >&2; echo cwd=$(pwd) FOO=$FOO slot=$DRMANHATTAN_SLOT; exit 3", "cwd": d.work, "env": {"FOO": "bar"}, "timeout": 60}));
    assert_eq!(d.wait_exit("hello", T), 3);
    let status = d.status_of("hello").unwrap();
    assert_eq!(status["state"], "failed");
    assert_eq!(status["exit_status"], 3);
    assert_eq!(status["label"], "hello"); // defaults to the file name
    assert_eq!(status["category"], "sh"); // no clear command inside `sh -c`: the shell's name
    assert_eq!(status["timeout"], 60);
    assert_eq!(status["slot"], 1);
    assert!(status["job_file"].as_str().unwrap().ends_with("hello.json.claimed"));
    assert!(status.get("env").is_none(), "env values stay out of the status file");
    let log = d.log_of("hello");
    assert!(log.contains("out\nerr\n"), "{log}");
    assert!(log.contains(&format!("cwd={} FOO=bar slot=1", d.work.display())), "{log}");
    assert!(log.contains("drmanhattan · failed in") && log.contains(", exit 3"), "{log}");
    assert!(d.inbox.join("hello.json.claimed").exists() && !d.inbox.join("hello.json").exists());
    let history = fs::read_to_string(d.data.join("history.jsonl")).unwrap();
    assert!(history.contains("\"label\":\"hello\"") && history.contains("\"exit_status\":3"));
    // The record is archived just after the result files are written.
    let archive = wait_for(T, || fs::read_dir(d.data.join("archive")).ok()?.next()?.ok()).unwrap().path();
    assert_eq!(fs::read_dir(archive).unwrap().count(), 1);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(fs::metadata(&d.state).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(d.inbox.join("hello.log")).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn invalid_jobs_are_rejected_and_nothing_runs_twice() {
    let d = Daemon::start(&[]);
    d.submit_text("garbage", "this is not json");
    d.submit("nocommand", json!({"label": "x"}));
    d.submit("nocwd", json!({"command": ["true"], "cwd": "/no/such/dir"}));
    d.submit("badenv", json!({"command": ["true"], "env": {"A": 1}}));
    d.submit("ok", json!({"command": ["true"]}));
    for name in ["garbage", "nocommand", "nocwd", "badenv"] {
        assert_eq!(d.wait_exit(name, T), 2, "{name}");
        let s = d.status_of(name).unwrap();
        assert_eq!(s["state"], "rejected", "{name}");
        assert!(d.inbox.join(format!("{name}.json.rejected")).exists(), "{name}");
    }
    assert!(d.status_of("garbage").unwrap()["failure_reason"].as_str().unwrap().contains("not valid JSON"));
    assert!(d.status_of("nocommand").unwrap()["failure_reason"].as_str().unwrap().contains("missing field `command`"));
    assert!(d.status_of("nocwd").unwrap()["failure_reason"].as_str().unwrap().contains("cwd does not exist"));
    assert_eq!(d.wait_exit("ok", T), 0);
    assert_eq!(d.status_of("ok").unwrap()["cwd"], json!(d.inbox.to_str().unwrap())); // cwd defaults to the inbox
    std::thread::sleep(Duration::from_millis(400));
    // `.status.json` files are not jobs, and claimed files are never re-read
    assert_eq!(fs::read_dir(d.state.join("runs")).unwrap().count(), 1);
    assert!(!d.daemon_log().contains("ok.status"));
}

#[test]
fn missing_executable_is_a_failed_run() {
    let d = Daemon::start(&[]);
    d.submit("nope", json!({"command": ["/no/such/program"], "cwd": d.work}));
    assert_eq!(d.wait_exit("nope", T), 127);
    let s = d.status_of("nope").unwrap();
    assert_eq!(s["state"], "failed");
    assert!(s["failure_reason"].as_str().unwrap().contains("could not start"));
}

#[test]
fn cancel_file_stops_queued_and_running_jobs() {
    let d = Daemon::start(&[]);
    d.submit("blocker", d.job("sleep 30 & wait"));
    let running = d.wait_state("blocker", "running", T);
    d.submit("waiter", d.job("sleep 30"));
    d.wait_state("waiter", "queued", T);
    fs::write(d.inbox.join("waiter.cancel"), "").unwrap();
    assert_eq!(d.wait_exit("waiter", T), 130);
    let w = d.status_of("waiter").unwrap();
    assert_eq!(w["state"], "cancelled");
    assert!(w.get("slot").is_none() && w.get("exit_status").is_none());
    assert!(!d.inbox.join("waiter.cancel").exists());
    let pgid =
        wait_for(T, || d.status_of("blocker")?["process_group"].as_i64()).expect("process group recorded") as i32;
    assert!(group_exists(pgid));
    assert_eq!(running["state"], "running");
    fs::write(d.inbox.join("blocker.cancel"), "").unwrap();
    let code = d.wait_exit("blocker", T);
    assert!(code >= 128, "cancelled runs report the signal: {code}");
    let b = d.status_of("blocker").unwrap();
    assert_eq!(b["state"], "cancelled");
    assert_eq!(b["slot"], 1);
    wait_for(T, || (!group_exists(pgid)).then_some(())).expect("process group gone");
    assert_eq!(d.status_json()["state"], "idle");
    // a cancel for a finished job is ignored, not an error
    fs::write(d.inbox.join("blocker.cancel"), "").unwrap();
    wait_for(T, || d.daemon_log().contains("blocker.cancel").then_some(())).unwrap();
}

#[test]
fn timeout_terminates_the_group() {
    let d = Daemon::start(&[]);
    d.submit("slow", json!({"command": "sleep 30 & sleep 30", "cwd": d.work, "timeout": 1}));
    assert_eq!(d.wait_exit("slow", T), 124);
    let s = d.status_of("slow").unwrap();
    assert_eq!(s["state"], "timed_out");
    assert!(d.log_of("slow").contains("timed out after 1.0s"));
    assert_eq!(d.status_json()["state"], "idle");
}

#[test]
fn a_cancelled_job_that_exits_zero_still_reports_failure() {
    let d = Daemon::start(&[]);
    d.submit("trapper", d.job("trap 'exit 0' TERM; while true; do sleep 0.1; done"));
    d.wait_state("trapper", "running", T);
    wait_for(T, || d.status_of("trapper")?["process_group"].as_i64()).unwrap();
    fs::write(d.inbox.join("trapper.cancel"), "").unwrap();
    let code = d.wait_exit("trapper", T);
    assert_ne!(code, 0, "a cancelled run never reports success");
    assert_eq!(d.status_of("trapper").unwrap()["state"], "cancelled");
}

#[test]
fn a_failed_run_records_its_last_error_line() {
    let d = Daemon::start(&[]);
    d.submit(
        "broken",
        d.job(
            "echo compiling; echo 'error: test_parse failed: expected 3, got 4'; echo '1 failed, 119 passed'; exit 1",
        ),
    );
    assert_eq!(d.wait_exit("broken", T), 1);
    assert_eq!(d.status_of("broken").unwrap()["error_line"], "error: test_parse failed: expected 3, got 4");
    let state = d.api_state();
    assert_eq!(state["done"][0]["error_line"], "error: test_parse failed: expected 3, got 4");
}

#[test]
fn each_output_line_is_written_to_the_log_exactly_once() {
    let d = Daemon::start(&[]);
    d.submit(
        "once",
        json!({"command": "echo alpha-line; echo beta-line >&2; printf 'gamma-line\\n'; exit 0", "cwd": d.work}),
    );
    assert_eq!(d.wait_exit("once", T), 0);
    let log = d.log_of("once");
    for needle in ["alpha-line\n", "beta-line\n", "gamma-line\n", " started in slot ", "drmanhattan · succeeded in "] {
        assert_eq!(log.matches(needle).count(), 1, "{needle:?} should appear once in:\n{log}");
    }
    // Two lines say what started and where, three are the output, one says how it ended.
    assert_eq!(log.lines().count(), 6, "{log}");
}

#[test]
fn a_resubmitted_name_never_reports_the_old_result() {
    let d = Daemon::start(&[]);
    d.submit("same", d.job("echo first-run; exit 3"));
    assert_eq!(d.wait_exit("same", T), 3);
    // Hold the only slot, then drop the same name again: until it runs and ends, its old
    // .exit, .log and .status.json must be gone, not left for a poller to mistake.
    d.submit("blocker", d.job("sleep 2"));
    d.wait_state("blocker", "running", T);
    d.submit("same", d.job("echo second-run"));
    let s = d.wait_state("same", "queued", T);
    assert!(s.get("exit_status").is_none());
    assert!(!d.inbox.join("same.exit").exists(), "the old result is gone as soon as the job is claimed");
    assert!(!d.log_of("same").contains("first-run"));
    assert_eq!(d.wait_exit("same", T), 0);
    let log = d.log_of("same");
    assert!(log.contains("second-run") && !log.contains("first-run"), "{log}");
}

#[test]
fn a_name_that_is_still_live_waits_its_turn() {
    let d = Daemon::start(&[("DRMANHATTAN_SLOTS", "2")]);
    d.submit("dup", d.job("sleep 1.5; echo one"));
    d.wait_state("dup", "running", T);
    d.submit("dup", d.job("echo two"));
    std::thread::sleep(Duration::from_millis(500));
    assert!(d.inbox.join("dup.json").exists(), "the second file waits in the inbox while the first runs");
    assert_eq!(d.status_of("dup").unwrap()["state"], "running");
    wait_for(T, || d.log_of("dup").contains("two").then_some(())).expect("the second job runs after the first");
    assert_eq!(d.wait_exit("dup", T), 0);
}

#[test]
fn symlinked_state_files_are_refused() {
    let d = Daemon::start(&[]);
    let evil = d.root.join("evil");
    // A manual order behind a symlink is ignored: the waiters keep the automatic order.
    d.submit("blocker", d.job("sleep 30"));
    d.wait_state("blocker", "running", T);
    d.submit("first", d.job("true"));
    d.wait_state("first", "queued", T);
    d.submit("second", d.job("true"));
    d.wait_state("second", "queued", T);
    fs::write(&evil, format!("{}\n", d.run_id_of("second"))).unwrap();
    std::os::unix::fs::symlink(&evil, d.state.join("manual-order")).unwrap();
    let waiters = d.status_json()["waiters"].clone();
    assert!(waiters.as_array().unwrap().iter().all(|w| w["manual"] == false), "{waiters}");
    assert_eq!(d.waiters(), vec!["first", "second"]);
}

#[test]
fn an_inbox_others_can_write_to_is_refused() {
    let d = Daemon::start(&[]);
    let open = d.root.join("open-inbox");
    fs::create_dir_all(&open).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
    let out = d
        .cmd(&["serve", "--port", "0", "--inbox", open.to_str().unwrap()])
        .env("DRMANHATTAN_STATE_DIR", d.root.join("other-state"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("writable by other users") && err.contains("chmod 700"), "{err}");
}

#[test]
fn old_inbox_results_are_pruned_and_live_or_pending_files_are_not() {
    let mut d = Daemon::start(&[("DRMANHATTAN_LOG_RETENTION_DAYS", "7")]);
    let month_ago = std::time::SystemTime::now() - Duration::from_secs(30 * 86400);
    let old_files = ["old.log", "old.exit", "old.status.json", "old.json.claimed", "bad.json.rejected", "bad.exit"];
    for name in old_files {
        let p = d.inbox.join(name);
        fs::write(&p, "x").unwrap();
        fs::File::options().write(true).open(&p).unwrap().set_modified(month_ago).unwrap();
    }
    fs::write(d.inbox.join("recent.log"), "x").unwrap();
    fs::write(d.inbox.join("notes.txt"), "x").unwrap();
    d.stop_daemon();
    d.launch();
    for name in old_files {
        assert!(!d.inbox.join(name).exists(), "{name} is past the retention window");
    }
    assert!(d.inbox.join("recent.log").exists());
    assert!(d.inbox.join("notes.txt").exists(), "files that are not drmanhattan's are left alone");
}

#[test]
fn history_grows_as_runs_finish_and_old_archives_are_pruned() {
    let mut d = Daemon::start(&[("DRMANHATTAN_ARCHIVE_DAYS", "30")]);
    d.seed_history(&[("sleep", "x", 5); 3]);
    assert_eq!(d.api_state()["history_count"], 3);
    d.submit("one", d.job("true"));
    d.wait_exit("one", T);
    wait_for(T, || (d.api_state()["history_count"] == 4).then_some(())).expect("history grows as runs finish");
    // Archived records past the archive window go at start-up; recent ones stay.
    let month = d.data.join("archive").join("2020-01");
    fs::create_dir_all(&month).unwrap();
    let old = month.join("1-1-aaaaaa.json");
    fs::write(&old, "{}").unwrap();
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(40 * 86400))
        .unwrap();
    d.stop_daemon();
    d.launch();
    assert!(!old.exists() && !month.exists(), "old archive records and their empty month are pruned");
    let kept: usize =
        fs::read_dir(d.data.join("archive")).unwrap().flatten().map(|m| fs::read_dir(m.path()).unwrap().count()).sum();
    assert_eq!(kept, 1, "this run's record stays");
}

#[test]
fn ingesting_many_jobs_does_not_stall_the_dashboard() {
    let d = Daemon::start(&[]);
    d.submit("blocker", d.job("sleep 30"));
    d.wait_state("blocker", "running", T);
    for i in 0..300 {
        d.submit(&format!("bulk{i}"), d.job("true"));
    }
    let mut slowest = Duration::ZERO;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let t = std::time::Instant::now();
        d.api_state();
        slowest = slowest.max(t.elapsed());
    }
    assert!(slowest < Duration::from_millis(1500), "a poll waited {slowest:?} for the queue lock");
    wait_for(Duration::from_secs(20), || (d.status_json()["waiters"].as_array()?.len() == 300).then_some(()))
        .expect("all 300 queued");
    fs::write(d.inbox.join("blocker.cancel"), "").unwrap();
}

#[test]
fn a_cancel_written_with_the_job_still_cancels_it() {
    let d = Daemon::start(&[]);
    let names: Vec<String> = (0..3).map(|i| format!("early{i}")).collect();
    for name in &names {
        d.submit(name, d.job("sleep 5"));
        fs::write(d.inbox.join(format!("{name}.cancel")), "").unwrap();
    }
    for name in &names {
        d.wait_exit(name, T);
        assert_eq!(d.status_of(name).unwrap()["state"], "cancelled", "{name}");
    }
}

#[test]
fn a_runaway_log_is_cut_back_and_the_daemon_keeps_its_own_log() {
    let d = Daemon::start(&[("DRMANHATTAN_MAX_LOG_MB", "1")]);
    d.submit(
        "noisy",
        d.job("yes 0123456789 | head -c 3000000; echo error: the real failure; sleep 1.5; echo after-the-cut"),
    );
    assert_eq!(d.wait_exit("noisy", T), 0);
    let log = d.log_of("noisy");
    assert!(log.len() < 1024 * 1024, "the log was cut back: {} bytes", log.len());
    assert!(
        log.contains("the log passed 1 MB") && log.contains("after-the-cut"),
        "{}",
        &log[log.len().saturating_sub(400)..]
    );
    assert!(log.contains("error: the real failure"), "the newest output before the cut is kept");
    // The daemon writes its own lines to a file in the data directory too, without colour.
    let own = fs::read_to_string(d.data.join("drmanhattan.log")).unwrap();
    assert!(own.contains("succeeded") && own.contains("noisy") && !own.contains('\x1b'), "{own}");
}

#[test]
fn fail_patterns_fail_a_run_that_exited_0() {
    let d = Daemon::start(&[]);
    let patterns = json!([r"^Skipped \d+ suites?", "^never$"]);
    d.submit("skips", json!({"command": "echo building; echo 'Skipped 2 suites: no database'; exit 0", "cwd": d.work, "fail_patterns": patterns}));
    d.submit("clean", json!({"command": "echo all ran", "cwd": d.work, "fail_patterns": patterns}));
    d.submit("broken", json!({"command": "echo 'Skipped 1 suite'; exit 4", "cwd": d.work, "fail_patterns": patterns}));
    assert_eq!(d.wait_exit("skips", T), 1);
    let s = d.status_of("skips").unwrap();
    assert_eq!((&s["state"], &s["exit_status"]), (&json!("failed"), &json!(1)));
    assert_eq!(s["error_line"], "Skipped 2 suites: no database");
    assert!(s["failure_reason"].as_str().unwrap().contains(r"fail pattern"), "{s}");
    assert!(d.log_of("skips").contains("drmanhattan · the command exited 0, but its output matched"));
    let history = fs::read_to_string(d.data.join("history.jsonl")).unwrap();
    assert!(history.lines().any(|l| l.contains("\"label\":\"skips\"") && l.contains("\"outcome\":\"failed\"")));
    assert_eq!(d.wait_exit("clean", T), 0, "no line matched");
    assert_eq!(d.wait_exit("broken", T), 4, "a run that failed anyway keeps its own exit status");
}
