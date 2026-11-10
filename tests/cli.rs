//! The command line: submit, slots, and the source `submit` records.

mod common;

use std::fs;
use std::time::Duration;

use common::{wait_for, Daemon};
use serde_json::json;

const T: Duration = Duration::from_secs(30);

#[test]
fn submit_warns_about_piped_input_and_runs_the_job_anyway() {
    use std::fs::File;
    use std::os::unix::io::FromRawFd;
    use std::process::Stdio;

    let d = Daemon::start(&[]);
    // A pipe with bytes already waiting, as `echo msg | manhattan submit …` gives.
    let mut fds = [0; 2];
    // SAFETY: pipe() fills two fds; the write end is written once and closed here.
    let piped = unsafe {
        assert_eq!(libc::pipe(fds.as_mut_ptr()), 0);
        assert_eq!(libc::write(fds[1], b"msg\n".as_ptr().cast(), 4), 4);
        libc::close(fds[1]);
        File::from_raw_fd(fds[0])
    };
    let out = d
        .cmd(&["submit", "--name", "piped", "--wait", "--", "sh", "-c", "cat; echo done"])
        .stdin(Stdio::from(piped))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("input piped to submit is not passed on"), "{stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("done\n"));
    // Nothing piped: no warning.
    let quiet = d.cmd(&["submit", "--name", "quiet", "--wait", "--", "true"]).stdin(Stdio::null()).output().unwrap();
    assert_eq!(quiet.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&quiet.stderr).contains("piped"));
}

#[test]
fn submit_command_writes_a_job_and_can_wait_for_it() {
    let d = Daemon::start(&[]);
    let out = d
        .cmd(&[
            "submit",
            "--name",
            "viacli",
            "--label",
            "via cli",
            "--category",
            "demo",
            "--timeout",
            "60",
            "--env",
            "X=1",
            "--wait",
            "--",
            "sh",
            "-c",
            "echo X=$X; exit 4",
        ])
        .current_dir(&d.work)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("as viacli in") && stdout.contains("X=1\n"), "{stdout}");
    let s = d.status_of("viacli").unwrap();
    assert_eq!(s["label"], "via cli");
    assert_eq!(s["category"], "demo");
    assert_eq!(s["cwd"], json!(d.work.to_str().unwrap()));
    // a finished job's name can be used again; a queued one's cannot
    let again = d.cmd(&["submit", "--name", "viacli", "--wait", "--", "true"]).output().unwrap();
    assert_eq!(again.status.code(), Some(0), "{}", String::from_utf8_lossy(&again.stderr));
    fs::write(d.inbox.join("busy.json.claimed"), "{}").unwrap();
    let busy = d.cmd(&["submit", "--name", "busy", "--", "true"]).output().unwrap();
    assert_eq!(busy.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&busy.stderr).contains("already queued or running"));
    // a default name is derived from the label
    let auto = d.cmd(&["submit", "--label", "auto job", "--", "true"]).output().unwrap();
    assert!(String::from_utf8_lossy(&auto.stdout).contains("as auto_job-"));
    assert_eq!(d.cmd(&["submit", "--",]).output().unwrap().status.code(), Some(2));
}

#[test]
fn slots_command_sets_the_standing_capacity() {
    let d = Daemon::start(&[]);
    let text = |args: &[&str]| String::from_utf8_lossy(&d.cmd(args).output().unwrap().stdout).into_owned();
    assert!(text(&["slots"]).contains("capacity=1 source=default"));
    assert_eq!(d.cmd(&["slots", "3"]).output().unwrap().status.code(), Some(0));
    assert!(text(&["slots"]).contains("capacity=3 source=config"));
    assert_eq!(d.cmd(&["slots", "0"]).output().unwrap().status.code(), Some(2));
    assert_eq!(d.cmd(&["slots", "257"]).output().unwrap().status.code(), Some(2));
    // the daemon reads the new standing capacity live
    d.submit("a", d.job("sleep 1"));
    d.submit("b", d.job("sleep 1"));
    d.submit("c", d.job("sleep 1"));
    d.wait_state("a", "running", T);
    d.wait_state("b", "running", T);
    d.wait_state("c", "running", T);
    assert!(text(&["paths"]).contains("daemon=pid"));
}

#[test]
fn source_is_recorded_from_job_files_and_detected_by_submit() {
    let d = Daemon::start(&[]);
    d.submit("tagged", json!({"command": ["true"], "cwd": d.work, "source": {"tool": "Cursor", "session": "abc123", "url": "cursor://open/abc123"}}));
    d.submit("legacy", json!({"command": ["true"], "cwd": d.work, "session": "sess-9"}));
    d.submit("named", json!({"command": ["true"], "cwd": d.work, "source": "a cron job"}));
    d.submit("badurl", json!({"command": ["true"], "cwd": d.work, "source": {"url": "not a url"}}));
    for n in ["tagged", "legacy", "named"] {
        assert_eq!(d.wait_exit(n, T), 0, "{n}");
    }
    assert_eq!(d.wait_exit("badurl", T), 2);
    assert_eq!(
        d.status_of("tagged").unwrap()["source"],
        json!({"tool": "Cursor", "session": "abc123", "url": "cursor://open/abc123"})
    );
    assert_eq!(d.status_of("legacy").unwrap()["source"], json!({"session": "sess-9"}));
    assert_eq!(d.status_of("named").unwrap()["source"], json!({"tool": "a cron job"}));
    let state = d.api_state();
    let tagged = state["done"].as_array().unwrap().iter().find(|r| r["label"] == "tagged").unwrap();
    assert_eq!(tagged["source"]["tool"], "Cursor");
    assert_eq!(tagged["source"]["url"], "cursor://open/abc123");
    assert_eq!(tagged["source"]["short"], "abc123");
    // `submit` detects the tool from the environment, and flags override it
    let out = d
        .cmd(&["submit", "--name", "fromclaude", "--", "true"])
        .env("CLAUDE_CODE_SESSION_ID", "cc-1")
        .current_dir(&d.work)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(d.wait_state("fromclaude", "succeeded", T)["source"], json!({"tool": "Claude Code", "session": "cc-1"}));
    let out = d
        .cmd(&["submit", "--name", "fromcursor", "--", "true"])
        .env("CURSOR_TRACE_ID", "tr-1")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&d.work)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(d.wait_state("fromcursor", "succeeded", T)["source"], json!({"tool": "Cursor", "session": "tr-1"}));
    let out = d
        .cmd(&[
            "submit",
            "--name",
            "explicit",
            "--source",
            "Copilot",
            "--source-url",
            "https://example.invalid/x",
            "--",
            "true",
        ])
        .env("CLAUDE_CODE_SESSION_ID", "cc-1")
        .current_dir(&d.work)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        d.wait_state("explicit", "succeeded", T)["source"],
        json!({"tool": "Copilot", "url": "https://example.invalid/x"})
    );
}

#[test]
fn a_bb_thread_finds_its_own_runs_with_status_session() {
    let d = Daemon::start(&[]);
    let from = |thread: &str, name: &str, wait: bool, script: &str| {
        let mut args = vec!["submit", "--name", name];
        if wait {
            args.push("--wait");
        }
        args.extend(["--", "sh", "-c", script]);
        let out = d
            .cmd(&args)
            .env("BB_THREAD_ID", thread)
            .env("BB_SERVER_URL", "http://127.0.0.1:38886")
            .env("CLAUDE_CODE_SESSION_ID", "cc-inside-bb")
            .current_dir(&d.work)
            .output()
            .unwrap();
        out.status.code()
    };
    assert_eq!(from("thr_a", "a1", true, "true"), Some(0));
    assert_eq!(from("thr_a", "a2", true, "echo broken; exit 3"), Some(3));
    assert_eq!(from("thr_b", "b1", true, "true"), Some(0));
    assert_eq!(from("thr_a", "a3", false, "sleep 30"), Some(0));
    d.wait_state("a3", "running", T);
    assert_eq!(
        d.status_of("a1").unwrap()["source"],
        json!({"tool": "bb", "session": "thr_a", "url": "http://127.0.0.1:38886/threads/thr_a"}),
        "bb's thread wins over the Claude Code session it runs"
    );

    let status = |extra: &[&str]| {
        let mut args = vec!["status", "--session", "thr_a"];
        args.extend(extra);
        d.cmd(&args).output().unwrap()
    };
    let doc: serde_json::Value = serde_json::from_slice(&status(&["--json"]).stdout).unwrap();
    assert_eq!(doc["session"], "thr_a");
    let running = doc["running"].as_array().unwrap();
    assert_eq!(running.len(), 1, "{doc}");
    assert_eq!(running[0]["source"]["session"], "thr_a");
    let finished: Vec<_> = doc["finished"].as_array().unwrap().iter().map(|f| f["run_id"].clone()).collect();
    assert_eq!(finished, [d.status_of("a2").unwrap()["run_id"].clone(), d.status_of("a1").unwrap()["run_id"].clone()]);
    assert_eq!((&doc["finished"][0]["state"], &doc["finished"][0]["exit"]), (&json!("failed"), &json!(3)));
    let limited: serde_json::Value = serde_json::from_slice(&status(&["--json", "--limit", "1"]).stdout).unwrap();
    assert_eq!(limited["finished"].as_array().unwrap().len(), 1);
    let text = String::from_utf8_lossy(&status(&[]).stdout).into_owned();
    assert!(text.contains("finished run_id=") && text.contains("state=failed exit=3"), "{text}");
    assert!(!text.contains("b1"), "{text}");
    // Without --session, nothing changes but each run's source.
    let all = d.status_json();
    assert!(all.get("finished").is_none() && all.get("session").is_none());
    assert_eq!(all["running"][0]["source"]["tool"], "bb");
    assert_eq!(d.cmd(&["status", "--limit", "3"]).output().unwrap().status.code(), Some(2));
    assert_eq!(d.cmd(&["cancel", "a3"]).output().unwrap().status.code(), Some(0));
}

/// Runs a command, killing it if it has not finished within `limit`.
fn output_within(mut cmd: std::process::Command, limit: Duration) -> std::process::Output {
    let mut child = cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let finished = wait_for(limit, || child.try_wait().unwrap());
    if finished.is_none() {
        let _ = child.kill();
    }
    let out = child.wait_with_output().unwrap();
    assert!(finished.is_some(), "the command did not finish within {limit:?}");
    out
}

#[test]
fn submit_wait_returns_for_a_job_whose_name_has_a_dot() {
    let d = Daemon::start(&[]);
    fs::write(d.work.join("build.sh"), "echo built\n").unwrap();
    let mut cmd = d.cmd(&["submit", "--wait", "--", "sh", "build.sh"]);
    cmd.current_dir(&d.work);
    let out = output_within(cmd, T);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("as build.sh-") && stdout.contains("built\n"), "{stdout}");
}

#[test]
fn ctrl_c_while_waiting_cancels_the_job() {
    let d = Daemon::start(&[]);
    let mut child = d
        .cmd(&["submit", "--name", "waited", "--wait", "--", "sleep", "30"])
        .current_dir(&d.work)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    d.wait_state("waited", "running", T);
    // SAFETY: kill has no memory-safety preconditions.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let code = wait_for(T, || child.try_wait().unwrap()).expect("submit returns").code();
    assert_eq!(code, Some(128 + libc::SIGTERM), "it reports the cancelled job's status");
    assert_eq!(d.status_of("waited").unwrap()["state"], "cancelled");
}

#[test]
fn submitting_with_no_daemon_says_so() {
    let mut d = Daemon::start(&[]);
    d.stop_daemon();
    let out = d.cmd(&["submit", "--", "true"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "the job waits in the inbox");
    assert!(String::from_utf8_lossy(&out.stderr).contains("drmanhattan isn't running; the job waits"));
    // Waiting would never end, so --wait refuses at once and queues nothing.
    let queued = std::fs::read_dir(&d.inbox).unwrap().count();
    let out = d.cmd(&["submit", "--wait", "--", "true"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("drmanhattan isn't running, so nothing was queued"));
    assert_eq!(std::fs::read_dir(&d.inbox).unwrap().count(), queued);
}

#[test]
fn a_name_that_cannot_be_a_job_is_refused() {
    let d = Daemon::start(&[]);
    for name in ["ci.status", ".hidden", "a/b"] {
        let out = d.cmd(&["submit", "--name", name, "--", "true"]).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{name}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("cannot name a job"), "{name}");
    }
}

#[test]
fn status_reports_the_daemons_capacity_not_its_own_environments() {
    let d = Daemon::start(&[("DRMANHATTAN_SLOTS", "3")]);
    let out = d.cmd(&["status", "--json"]).env_remove("DRMANHATTAN_SLOTS").output().unwrap();
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["capacity"], 3);
    let out = d.cmd(&["slots"]).env_remove("DRMANHATTAN_SLOTS").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("capacity=3 source=DRMANHATTAN_SLOTS"));
}
