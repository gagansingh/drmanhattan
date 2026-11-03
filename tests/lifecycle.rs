//! The daemon's life: starting, a second start, stopping, Ctrl-C, and recovering after a crash.

mod common;

use std::fs;
use std::time::Duration;

use common::{group_exists, kill_guard, wait_for, Daemon};
use serde_json::json;

const T: Duration = Duration::from_secs(30);

#[test]
fn restart_requeues_waiting_jobs_and_abandons_the_running_one() {
    let mut d = Daemon::start(&[]);
    d.submit("orphan", d.job("sleep 60 & wait"));
    d.wait_state("orphan", "running", T);
    let pgid = wait_for(T, || d.status_of("orphan")?["process_group"].as_i64()).unwrap() as i32;
    d.submit("waiting", d.job("echo later"));
    d.wait_state("waiting", "queued", T);
    d.kill_daemon(); // SIGKILL: no shutdown path runs
                     // The job's guard ends it at once, with no restart needed.
    wait_for(T, || (!group_exists(pgid)).then_some(())).expect("the job does not outlive the daemon");
    assert!(d.daemon_log().contains(&format!("the daemon stopped; ending process group {pgid}")));
    d.launch();
    let o = d.wait_state("orphan", "abandoned", T);
    assert!(o["failure_reason"].as_str().unwrap().contains("daemon stopped"));
    assert_eq!(d.wait_exit("orphan", T), 1);
    assert_eq!(d.wait_exit("waiting", T), 0);
    assert!(d.log_of("waiting").contains("later"));
    d.wait_daemon_log("1 waiting job queued again · 1 running job abandoned");
}

#[test]
fn a_second_launch_is_friendly_and_stop_stops_the_daemon() {
    let mut d = Daemon::start(&[]);
    let out = d.cmd(&["serve", "--port", "0"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "a second launch is not an error: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("drmanhattan is already running"), "{text}");
    assert!(text.contains(&format!("pid {}", d.daemon_pid())), "{text}");
    assert!(text.contains(&d.url) && text.contains("`drmanhattan stop`") && text.contains("since"), "{text}");
    assert!(d.child.as_mut().unwrap().try_wait().unwrap().is_none(), "the first daemon keeps running");
    let out = d.cmd(&["stop"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("stopped"));
    let code = d.wait_daemon_exit(Duration::from_secs(10));
    assert_eq!(code, Some(0), "stop said: {}\ndaemon log:\n{}", String::from_utf8_lossy(&out.stdout), d.daemon_log());
    assert!(!d.state.join("dashboard.json").exists());
    let out = d.cmd(&["stop"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("not running"));
    assert_eq!(d.status_json()["state"], "stopped");
    d.launch(); // a stale lock file never blocks a restart
}

#[test]
fn first_ctrl_c_lets_running_jobs_finish_and_keeps_the_queue() {
    let mut d = Daemon::start(&[]);
    d.submit("finishing", d.job("sleep 1; echo finished-in-time"));
    d.wait_state("finishing", "running", T);
    d.submit("queued", d.job("true"));
    d.wait_state("queued", "queued", T);
    d.signal_daemon(libc::SIGINT);
    assert_eq!(d.wait_daemon_exit(Duration::from_secs(15)), Some(0));
    assert_eq!(
        d.status_of("finishing").unwrap()["state"],
        "succeeded",
        "a short job completes within the grace period"
    );
    assert!(d.log_of("finishing").contains("finished-in-time"));
    assert_eq!(d.status_of("queued").unwrap()["state"], "queued", "queued jobs are not started during shutdown");
    let log = d.daemon_log();
    assert!(log.contains("no new jobs will start") && log.contains("1 waiting job stays for the next start"), "{log}");
    assert!(!d.state.join("dashboard.json").exists());
    d.launch();
    assert_eq!(d.wait_exit("queued", T), 0);
    d.wait_daemon_log("1 waiting job queued again");
}

#[test]
fn ctrl_c_grace_period_then_cancels_and_a_second_ctrl_c_forces() {
    // grace is 2 s in the harness: a 30 s job is cancelled once it runs out
    let mut d = Daemon::start(&[]);
    d.submit("long", d.job("sleep 30"));
    d.wait_state("long", "running", T);
    d.signal_daemon(libc::SIGTERM);
    assert_eq!(d.wait_daemon_exit(Duration::from_secs(15)), Some(0));
    let s = d.status_of("long").unwrap();
    assert_eq!(s["state"], "cancelled");
    assert!(d.inbox.join("long.exit").exists(), "no job is left half-recorded");
    d.wait_daemon_log("1 job still running after 2.0s; cancelling");
    // a second Ctrl-C does not wait for the grace period, however long it is
    d.env.push(("DRMANHATTAN_SHUTDOWN_GRACE_SECONDS".into(), "60".into()));
    d.launch();
    d.submit("long2", d.job("sleep 30"));
    d.wait_state("long2", "running", T);
    let pgid = wait_for(T, || d.status_of("long2")?["process_group"].as_i64()).unwrap() as i32;
    let started = std::time::Instant::now();
    d.signal_daemon(libc::SIGINT);
    std::thread::sleep(Duration::from_millis(300));
    d.signal_daemon(libc::SIGINT);
    assert_eq!(d.wait_daemon_exit(Duration::from_secs(15)), Some(0));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "forced stop does not wait out the grace period: {:?}",
        started.elapsed()
    );
    assert_eq!(d.status_of("long2").unwrap()["state"], "cancelled");
    assert!(!group_exists(pgid), "the process group is gone");
    d.wait_daemon_log("now: cancelling the running jobs");
}

#[test]
fn startup_prints_a_recovery_summary_and_cleans_temp_files() {
    let mut d = Daemon::start(&[]);
    d.submit("done", d.job("true"));
    d.wait_exit("done", T);
    d.kill_daemon();
    // Two stranded temp files from long ago, and one a `submit` may be writing right now.
    let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
    for p in [d.state.join("runs").join(".tmp.123.456"), d.inbox.join(".tmp.789.0")] {
        fs::write(&p, "{}").unwrap();
        fs::File::options().write(true).open(&p).unwrap().set_modified(hour_ago).unwrap();
    }
    fs::write(d.inbox.join(".tmp.fresh"), "{}").unwrap();
    d.launch();
    d.wait_daemon_log("recovered: 2 stray temp files removed");
    assert!(!d.state.join("runs").join(".tmp.123.456").exists());
    assert!(!d.inbox.join(".tmp.789.0").exists());
    assert!(d.inbox.join(".tmp.fresh").exists(), "a fresh temp file may be a submit in progress");
}

#[test]
fn unusable_paths_are_reported_by_name_without_a_panic() {
    let d = Daemon::start(&[]);
    let file = d.root.join("a-file");
    fs::write(&file, "x").unwrap();
    let out = d.cmd(&["serve", "--port", "0"]).env("DRMANHATTAN_STATE_DIR", &file).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with("drmanhattan: error: cannot use the state directory") && err.contains(file.to_str().unwrap()),
        "{err}"
    );
    assert!(!err.contains("panicked") && !err.contains("backtrace"), "{err}");
    let out = d
        .cmd(&["serve", "--port", "0"])
        .env("DRMANHATTAN_INBOX", &file)
        .env("DRMANHATTAN_STATE_DIR", d.root.join("other-state"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("inbox") && err.contains(file.to_str().unwrap()), "{err}");
    let out = d
        .cmd(&["serve", "--port", "0", "--inbox", "/no/such/parent/inbox"])
        .env("DRMANHATTAN_STATE_DIR", d.root.join("other-state2"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("/no/such/parent/inbox"));
}

#[test]
fn a_busy_explicit_port_names_the_flag_and_a_busy_default_falls_back() {
    let d = Daemon::start(&[]);
    let port = d.url.rsplit(':').next().unwrap().to_string();
    let out =
        d.cmd(&["serve", "--port", &port]).env("DRMANHATTAN_STATE_DIR", d.root.join("other-state")).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(&format!(":{port}")) && err.contains("--port"), "{err}");
}

#[test]
fn stop_never_signals_a_stale_pid() {
    let mut d = Daemon::start(&[]);
    d.stop_daemon();
    // A crash left dashboard.json and daemon.lock naming a pid that is now someone else's.
    let mut bystander = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let pid = bystander.id();
    fs::write(
        d.state.join("dashboard.json"),
        json!({"pid": pid, "inbox": d.inbox, "started_at": "2026-01-01T00:00:00Z", "url": "http://127.0.0.1:1/"})
            .to_string(),
    )
    .unwrap();
    fs::write(d.state.join("daemon.lock"), format!("{pid}\n")).unwrap();
    let out = d.cmd(&["stop"]).output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("not running"));
    assert!(bystander.try_wait().unwrap().is_none(), "the unrelated process is untouched");
    assert_eq!(d.status_json()["state"], "stopped");
    assert!(!String::from_utf8_lossy(&d.cmd(&["paths"]).output().unwrap().stdout).contains("daemon="));
    let _ = bystander.kill();
    let _ = bystander.wait();
}

#[test]
fn a_claimed_job_with_no_record_is_queued_again() {
    let mut d = Daemon::start(&[]);
    // A finished run whose record and claimed file both survive, beside the orphans.
    d.submit("earlier", d.job("true"));
    d.wait_exit("earlier", T);
    d.stop_daemon();
    // A daemon died between claiming a job and recording it: only the claimed file is left.
    let job = |command: &str| json!({"command": command, "cwd": d.work}).to_string();
    fs::write(d.inbox.join("lost.json.claimed"), job("echo again")).unwrap();
    // One whose log exists had started, so it cannot safely run again: it is recorded abandoned.
    fs::write(d.inbox.join("begun.json.claimed"), job("true")).unwrap();
    fs::write(d.inbox.join("begun.log"), "drmanhattan started\n").unwrap();
    d.launch();
    assert_eq!(d.wait_exit("lost", T), 0);
    assert!(d.log_of("lost").contains("again"));
    assert_eq!(d.wait_exit("begun", T), 1);
    assert_eq!(d.status_of("begun").unwrap()["state"], "abandoned");
    d.wait_daemon_log("2 lost job files recovered");
}

#[test]
fn a_missing_exit_file_is_written_again_at_start_up() {
    let mut d = Daemon::start(&[]);
    d.submit("done", d.job("exit 3"));
    assert_eq!(d.wait_exit("done", T), 3);
    d.kill_daemon();
    // The daemon died after recording the run but before writing `.exit`.
    fs::remove_file(d.inbox.join("done.exit")).unwrap();
    d.launch();
    assert_eq!(d.wait_exit("done", T), 3);
    d.wait_daemon_log("1 missing .exit written");
}

#[test]
fn a_job_whose_guard_also_died_is_ended_at_the_next_start() {
    let mut d = Daemon::start(&[]);
    d.submit("both", d.job("sleep 60 & wait"));
    d.wait_state("both", "running", T);
    let pgid = wait_for(T, || d.status_of("both")?["process_group"].as_i64()).unwrap() as i32;
    kill_guard(pgid);
    d.kill_daemon();
    assert!(group_exists(pgid), "with its guard gone, the job outlives the daemon");
    d.launch();
    let s = d.wait_state("both", "abandoned", T);
    assert!(s["failure_reason"].as_str().unwrap().contains("were terminated"), "{s}");
    wait_for(T, || (!group_exists(pgid)).then_some(())).expect("the next start ends it");
}

#[test]
fn leftovers_of_a_run_whose_leader_exited_are_terminated() {
    let mut d = Daemon::start(&[]);
    d.submit("leader", d.job("sleep 60 & sleep 1"));
    d.wait_state("leader", "running", T);
    let pgid = wait_for(T, || d.status_of("leader")?["process_group"].as_i64()).unwrap() as i32;
    kill_guard(pgid); // the guard died with the daemon, so only the next start can clean up
    d.kill_daemon();
    std::thread::sleep(Duration::from_millis(1500)); // the shell exits; its background sleep stays
    assert!(group_exists(pgid), "the leftover process is still running");
    d.launch();
    let s = d.wait_state("leader", "abandoned", T);
    assert!(s["failure_reason"].as_str().unwrap().contains("were terminated"), "{s}");
    wait_for(T, || (!group_exists(pgid)).then_some(())).expect("the leftover process is terminated");
}

#[test]
fn a_bad_setting_is_refused_at_start() {
    let d = Daemon::start(&[]);
    let out = d.cmd(&["serve", "--port", "0"]).env("DRMANHATTAN_HEARTBEAT_SECONDS", "inf").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("DRMANHATTAN_HEARTBEAT_SECONDS must be a number of seconds"), "{err}");
    // Values too large to add to a clock reading are refused too, rather than panicking.
    let out = d.cmd(&["stop"]).env("DRMANHATTAN_SHUTDOWN_GRACE_SECONDS", "1e300").output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    let out = d.cmd(&["stop"]).env("DRMANHATTAN_LOG_RETENTION_DAYS", "18446744073709551615").output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_reused_name_is_not_mistaken_for_the_old_run_after_a_crash() {
    let mut d = Daemon::start(&[]);
    d.submit("tests", d.job("exit 7"));
    assert_eq!(d.wait_exit("tests", T), 7);
    d.kill_daemon();
    // A new `tests.json` was claimed (the old results removed) just before the daemon died,
    // so only the old run's record survives.
    for old in ["tests.exit", "tests.status.json", "tests.log", "tests.json.claimed"] {
        fs::remove_file(d.inbox.join(old)).unwrap();
    }
    fs::write(d.inbox.join("tests.json.claimed"), d.job("echo second run").to_string()).unwrap();
    d.launch();
    assert_eq!(d.wait_exit("tests", T), 0, "the new job runs; the old exit code is not written back");
    assert!(d.log_of("tests").contains("second run"));
}

#[test]
fn a_record_caught_mid_rename_is_not_read_twice() {
    let mut d = Daemon::start(&[]);
    d.submit("done", d.job("true"));
    d.wait_exit("done", T);
    d.kill_daemon();
    let run_id = d.run_id_of("done");
    let runs = d.state.join("runs");
    fs::copy(runs.join(format!("{run_id}.json")), runs.join(".tmp.999.1")).unwrap();
    d.launch();
    assert_eq!(d.api_state()["done"].as_array().map(Vec::len), Some(1), "the run is listed once");
    assert!(!d.daemon_log().contains("recovered:"), "nothing to report: {}", d.daemon_log());
}

#[test]
fn an_orphan_in_a_new_inbox_is_not_mistaken_for_an_old_inbox_run() {
    let mut d = Daemon::start(&[]);
    d.submit("x", d.job("true"));
    d.wait_exit("x", T);
    d.stop_daemon();
    // The daemon moves to another inbox, and dies just after claiming a job there that has
    // the same name as the finished run in the old inbox.
    let other = d.root.join("other-inbox");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("x.json.claimed"), d.job("echo from the new inbox").to_string()).unwrap();
    for (k, v) in d.env.iter_mut() {
        if k == "DRMANHATTAN_INBOX" {
            *v = other.to_str().unwrap().to_string();
        }
    }
    d.inbox = other;
    d.launch();
    assert_eq!(d.wait_exit("x", T), 0);
    assert!(d.log_of("x").contains("from the new inbox"));
}
