//! Commands about one run: wait, follow, cancel and status <run>.

mod common;

use std::time::Duration;

use common::{wait_for, Daemon};
use serde_json::{json, Value};

const T: Duration = Duration::from_secs(30);

fn run(d: &Daemon, args: &[&str]) -> (i32, String, String) {
    let out = d.cmd(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn wait_follow_status_and_cancel_one_run() {
    let d = Daemon::start(&[]);
    d.submit("blocker", d.job("sleep 30 & wait"));
    d.wait_state("blocker", "running", T);
    d.submit("job", d.job("echo first; sleep 0.5; echo second; exit 3"));
    d.wait_state("job", "queued", T);
    let (code, out, err) = run(&d, &["wait", "job", "--until", "started", "--timeout", "0.3"]);
    assert_eq!(code, 124, "{out}{err}");
    assert!(out.contains("state=queued") && err.contains("still queued"), "{out}{err}");

    let blocker = d.run_id_of("blocker");
    let short = blocker.rsplit('-').next().unwrap();
    let (code, out, _) = run(&d, &["cancel", &format!("#{short}")]);
    assert_eq!(code, 0);
    assert!(out.contains(&format!("run_id={blocker}")) && out.contains("cancel=requested"), "{out}");

    let (code, out, _) = run(&d, &["wait", "job", "--until", "started"]);
    assert_eq!(code, 0, "{out}");
    let job = d.run_id_of("job");
    assert!(out.starts_with(&format!("run_id={job} state=")), "{out}");

    let (code, out, _) = run(&d, &["follow", &job]);
    assert_eq!(code, 3, "follow exits with the run's code");
    assert!(out.contains("first\nsecond\n") && out.contains("drmanhattan · failed in"), "{out}");
    let from = out.find("second").unwrap();
    let (code, rest, _) = run(&d, &["follow", "job", "--from", &from.to_string()]);
    assert_eq!((code, rest.as_str()), (3, &out[from..]), "--from carries on where a reader stopped");

    assert_eq!(run(&d, &["wait", &job]).0, 3, "wait (until finished) exits with the run's code");
    let (code, out, _) = run(&d, &["status", "job", "--json"]);
    assert_eq!(code, 0);
    let doc: Value = serde_json::from_str(&out).unwrap();
    assert_eq!((&doc["run_id"], &doc["state"], &doc["exit"]), (&json!(job), &json!("failed"), &json!(3)));
    assert!(doc.get("env").is_none());
    let (_, line, _) = run(&d, &["status", &job]);
    assert_eq!(line.trim(), format!("run_id={job} state=failed job=job exit=3"));
    let (code, out, _) = run(&d, &["cancel", "job"]);
    assert_eq!((code, out.contains("cancel=already-over")), (0, true));
    assert_eq!(run(&d, &["wait", "blocker"]).0, 143, "the blocker was cancelled while running");
}

#[test]
fn a_queued_run_cancelled_by_id_ends_at_once() {
    let d = Daemon::start(&[]);
    d.submit("blocker", d.job("sleep 30 & wait"));
    d.wait_state("blocker", "running", T);
    d.submit("waiter", d.job("echo never"));
    d.wait_state("waiter", "queued", T);
    let id = d.run_id_of("waiter");
    assert_eq!(run(&d, &["cancel", &id]).0, 0);
    let (code, out, _) = run(&d, &["wait", &id, "--until", "started"]);
    assert_eq!(code, 130, "it ended without starting: {out}");
    assert!(out.contains("state=cancelled"));
    let (code, _, err) = run(&d, &["wait", "nosuchjob"]);
    assert_eq!(code, 2);
    assert!(err.contains("no run or job is named nosuchjob"), "{err}");
}

#[test]
fn a_job_file_not_yet_picked_up_can_be_waited_on() {
    let mut d = Daemon::start(&[]);
    d.kill_daemon();
    d.submit("later", d.job("echo later"));
    let (code, out, _) = run(&d, &["status", "later", "--json", "--inbox", d.inbox.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["state"], "submitted");
    let inbox = d.inbox.to_str().unwrap().to_string();
    let waiter = d.cmd(&["wait", "later", "--inbox", &inbox]).stdout(std::process::Stdio::piped()).spawn().unwrap();
    d.launch();
    let out = waiter.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("state=succeeded"));
    wait_for(T, || d.status_of("later")).unwrap();
}

#[test]
fn only_the_newest_finished_runs_are_kept() {
    let d = Daemon::start(&[("DRMANHATTAN_MAX_FINISHED", "3")]);
    for i in 0..5 {
        let name = format!("kept{i}");
        d.submit(&name, d.job("true"));
        assert_eq!(d.wait_exit(&name, T), 0);
    }
    let labels = || -> Option<Vec<String>> {
        let out = d.cmd(&["status", "--json", "--limit", "50"]).output().ok()?;
        let doc: Value = serde_json::from_slice(&out.stdout).ok()?;
        let finished = doc["finished"].as_array()?;
        Some(finished.iter().filter_map(|f| f["label"].as_str().map(String::from)).collect())
    };
    let kept = wait_for(T, || labels().filter(|l| l.len() == 3)).unwrap_or_else(|| panic!("{:?}", labels()));
    assert_eq!(kept, ["kept4", "kept3", "kept2"], "the newest, newest first");
    let records = std::fs::read_dir(d.state.join("runs")).unwrap().count();
    assert_eq!(records, 3, "the older records are gone from disk too");
    let archived: usize = std::fs::read_dir(d.data.join("archive"))
        .unwrap()
        .flatten()
        .map(|month| std::fs::read_dir(month.path()).unwrap().count())
        .sum();
    assert_eq!(archived, 5, "the archive keeps every run");
    let (code, out, _) = run(&d, &["status", "kept0"]);
    assert_eq!(code, 0, "a forgotten run's own result files still answer until the retention");
    assert!(out.contains("state=succeeded"), "{out}");
}
