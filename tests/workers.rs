//! Workers: jobs handed to another drmanhattan over SSH. A stand-in `ssh` runs the
//! worker's commands on this machine against a second daemon (`common::fake_ssh`).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{bin, fake_ssh, wait_for, write_config, Daemon, WRITE_HEADERS};
use serde_json::{json, Value};

const T: Duration = Duration::from_secs(30);

/// A dispatcher with one local slot and a worker `w1`, sharing a home directory.
struct Pair {
    home: PathBuf,
    fake: PathBuf,
    worker: Daemon,
    here: Daemon,
}

fn pair(worker_extra: Value) -> Pair {
    let base = std::env::temp_dir().join(format!("drmanhattan-pair-{}-{}", std::process::id(), rand_suffix()));
    let home = base.join("home");
    fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let fake = base.join("fake");
    fs::create_dir_all(&fake).unwrap();
    let h = home.to_str().unwrap().to_string();
    let worker = Daemon::start(&[("HOME", &h), ("DRMANHATTAN_SLOTS", "2")]);
    let ssh = fake_ssh(&fake, &worker);
    let mut here = Daemon::start(&[
        ("HOME", &h),
        ("DRMANHATTAN_SSH", ssh.to_str().unwrap()),
        ("DRMANHATTAN_WORKER_POLL_SECONDS", "0.5"),
    ]);
    let mut w = json!({"ssh": "w1", "drmanhattan": bin(), "min_free_gb": 0});
    for (k, v) in worker_extra.as_object().unwrap() {
        w[k] = v.clone();
    }
    write_config(&here, json!({"workers": [w]}));
    here.stop_daemon();
    here.launch();
    Pair { home, fake, worker, here }
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

fn project(home: &Path, name: &str) -> PathBuf {
    let p = home.join(name);
    fs::create_dir_all(p.join("build")).unwrap();
    fs::write(p.join("a.txt"), "source").unwrap();
    fs::write(p.join(".gitignore"), "build/\n.env\n").unwrap();
    fs::write(p.join("build/out.o"), "product").unwrap();
    fs::write(p.join(".env"), "SECRET=1").unwrap();
    p
}

fn blocker(d: &Daemon) {
    d.submit("blocker", d.job("sleep 60 & wait"));
    d.wait_state("blocker", "running", T);
}

#[test]
fn a_job_runs_on_a_worker_when_every_local_slot_is_busy() {
    let p = pair(json!({}));
    let proj = project(&p.home, "proj");
    blocker(&p.here);
    p.here.submit(
        "remote",
        json!({"command": "echo ran in $(pwd) on ${DRMANHATTAN_RUN_ID%%-*} FOO=$FOO; echo '::note far away'; exit 3",
               "cwd": proj, "remote": "allowed", "env": {"FOO": "bar"}}),
    );
    assert_eq!(p.here.wait_exit("remote", T), 3);
    let s = p.here.status_of("remote").unwrap();
    assert_eq!((&s["state"], &s["machine"], &s["slot"]), (&json!("failed"), &json!("w1"), &json!(1)));
    assert_eq!(s["remote_run"]["job"], format!("dispatch-{}", s["run_id"].as_str().unwrap()));
    assert_eq!(s["notes"], json!(["far away"]));
    let log = p.here.log_of("remote");
    assert!(log.contains(&format!("ran in {} on", proj.display())) && log.contains("FOO=bar"), "{log}");
    assert!(log.contains("drmanhattan · remote sent to w1 (slot 1)"), "{log}");
    // The worker ran it as its own job, from this machine.
    let name = s["remote_run"]["job"].as_str().unwrap();
    let theirs: Value =
        serde_json::from_str(&fs::read_to_string(p.worker.inbox.join(format!("{name}.status.json"))).unwrap()).unwrap();
    assert_eq!((&theirs["state"], &theirs["exit_status"]), (&json!("failed"), &json!(3)));
    assert_eq!(theirs["dispatch_id"], s["run_id"]);
    // What reached it: the source, not what git would ignore.
    let mirrored = p.fake.join("root").join(proj.strip_prefix("/").unwrap());
    assert_eq!(fs::read_to_string(mirrored.join("a.txt")).unwrap(), "source");
    assert!(!mirrored.join("build").exists() && !mirrored.join(".env").exists(), "ignored files stay here");
    let history = fs::read_to_string(p.here.data.join("history.jsonl")).unwrap();
    assert!(
        history.lines().any(|l| l.contains("\"label\":\"remote\"") && l.contains("\"machine\":\"w1\"")),
        "{history}"
    );
    assert_eq!(p.here.status_of("blocker").unwrap()["state"], "running", "the local slot was never needed");
}

#[test]
fn prefer_takes_a_free_worker_first_and_requires_must_match() {
    let p = pair(json!({"tags": ["xcode-27"]}));
    let proj = project(&p.home, "proj");
    p.here.submit("pref", json!({"command": "echo hi", "cwd": proj, "remote": "prefer", "requires": ["xcode-27"]}));
    assert_eq!(p.here.wait_exit("pref", T), 0);
    assert_eq!(p.here.status_of("pref").unwrap()["machine"], "w1", "a free local slot did not stop it");
    p.here.submit("needs", json!({"command": "echo hi", "cwd": proj, "remote": "prefer", "requires": ["postgres"]}));
    assert_eq!(p.here.wait_exit("needs", T), 0);
    assert!(p.here.status_of("needs").unwrap()["machine"].is_null(), "the worker lacks the tag");
    p.here.submit("never", json!({"command": "echo hi", "cwd": proj}));
    assert_eq!(p.here.wait_exit("never", T), 0);
    assert!(p.here.status_of("never").unwrap()["machine"].is_null());
}

#[test]
fn an_unreachable_worker_sends_the_job_back_here_and_says_why() {
    let p = pair(json!({}));
    let proj = project(&p.home, "proj");
    fs::write(p.fake.join("down-w1"), "").unwrap();
    // The poller notices first: the worker is out of admission, with its reason.
    let w = wait_for(T, || {
        let ws = p.here.status_json()["workers"].clone();
        (ws[0]["state"] == "unavailable").then_some(ws[0].clone())
    })
    .expect("the worker is shown unavailable");
    assert!(w["reason"].as_str().unwrap().contains("Connection refused"), "{w}");
    p.here.submit("pref", json!({"command": "echo here", "cwd": proj, "remote": "prefer"}));
    assert_eq!(p.here.wait_exit("pref", T), 0);
    assert!(p.here.status_of("pref").unwrap()["machine"].is_null());
    let state = p.here.api_state();
    let m = &state["machines"];
    assert_eq!(m[1]["name"], "w1", "{m}");
    assert!(m[1]["reason"].as_str().is_some(), "the dashboard says why it takes no work: {m}");
    // Back online, the next poll clears what the poller found, without waiting out the window.
    fs::remove_file(p.fake.join("down-w1")).unwrap();
    wait_for(T, || (p.here.status_json()["workers"][0]["state"] == "free").then_some(()))
        .expect("an answer clears the poller's reason");
}

#[test]
fn a_failed_dispatch_runs_the_job_here_with_one_line_in_its_log() {
    let p = pair(json!({"check": "echo 'postgres is not answering'; exit 1"}));
    let proj = project(&p.home, "proj");
    p.here.submit("pref", json!({"command": "echo ran here", "cwd": proj, "remote": "prefer"}));
    assert_eq!(p.here.wait_exit("pref", T), 0);
    let s = p.here.status_of("pref").unwrap();
    assert!(s["machine"].is_null(), "{s}");
    assert!(s["remote_fallback"].as_str().unwrap().contains("postgres is not answering"), "{s}");
    let log = p.here.log_of("pref");
    assert!(log.contains("worker w1 unavailable: its check failed: postgres is not answering; running here"), "{log}");
    assert!(log.contains("ran here"));
    let w = p.here.status_json()["workers"][0].clone();
    assert_eq!(w["state"], "unavailable", "{w}");
}

#[test]
fn cancel_and_timeout_reach_the_job_on_the_worker() {
    let p = pair(json!({}));
    let proj = project(&p.home, "proj");
    p.here.submit("long", json!({"command": "echo started; sleep 60", "cwd": proj, "remote": "prefer"}));
    wait_for(T, || p.here.log_of("long").contains("started\n").then_some(())).expect("it runs on the worker");
    fs::write(p.here.inbox.join("long.cancel"), "").unwrap();
    assert_eq!(p.here.wait_exit("long", T), 143);
    assert_eq!(p.here.status_of("long").unwrap()["state"], "cancelled");
    let name = p.here.status_of("long").unwrap()["remote_run"]["job"].as_str().unwrap().to_string();
    assert_eq!(p.worker.status_of(&name).unwrap()["state"], "cancelled");

    p.here.submit("slow", json!({"command": "sleep 60", "cwd": proj, "remote": "prefer", "timeout": 1}));
    assert_eq!(p.here.wait_exit("slow", T), 124);
    let s = p.here.status_of("slow").unwrap();
    assert_eq!(s["state"], "timed_out");
    let name = s["remote_run"]["job"].as_str().unwrap();
    assert_eq!(p.worker.status_of(name).unwrap()["state"], "cancelled", "this machine asked it to stop");
}

#[test]
fn a_restarted_dispatcher_follows_its_job_again_and_a_dropped_follow_reconnects() {
    let mut p = pair(json!({}));
    let proj = project(&p.home, "proj");
    p.here.submit(
        "job",
        json!({"command": "echo one; sleep 2; echo two; sleep 4; echo three", "cwd": proj, "remote": "prefer"}),
    );
    wait_for(T, || p.here.log_of("job").contains("one\n").then_some(())).unwrap();
    p.here.kill_daemon();
    p.here.launch();
    wait_for(T, || p.here.log_of("job").contains("two\n").then_some(())).expect("following again");
    // Kill the follow itself: the runner asks again and reconnects from where it was.
    let _ = std::process::Command::new("pkill").args(["-f", "follow dispatch-"]).status();
    assert_eq!(p.here.wait_exit("job", T), 0);
    let log = p.here.log_of("job");
    assert_eq!(
        (log.matches("one\n").count(), log.matches("two\n").count(), log.matches("three\n").count()),
        (1, 1, 1),
        "{log}"
    );
    assert_eq!(p.here.status_of("job").unwrap()["machine"], "w1");
}

#[test]
fn the_worker_is_on_the_dashboard_with_its_readings_and_slots() {
    let p = pair(json!({"slots": 1}));
    let m = wait_for(T, || {
        let st = p.here.api_state();
        let m = st["machines"].clone();
        (m[1]["online"] == true && m[1]["stats"]["ncpu"].is_number()).then_some(m)
    })
    .expect("the worker's readings arrive");
    assert_eq!((&m[0]["name"], &m[0]["local"]), (&json!("local"), &json!(true)));
    assert_eq!((&m[1]["name"], &m[1]["capacity"]), (&json!("w1"), &json!(1)));
    // − to 0 pauses it.
    let (code, body) = p.here.post("/api/capacity", r#"{"worker": "w1", "slots": 0}"#, WRITE_HEADERS);
    assert_eq!(code, 200, "{body}");
    let proj = project(&p.home, "proj");
    p.here.submit("pref", json!({"command": "echo hi", "cwd": proj, "remote": "prefer"}));
    assert_eq!(p.here.wait_exit("pref", T), 0);
    assert!(p.here.status_of("pref").unwrap()["machine"].is_null(), "a paused worker takes nothing");
    let st = p.here.api_state();
    assert_eq!((&st["machines"][1]["capacity"], &st["machines"][1]["paused"]), (&json!(0), &json!(true)));
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(p.here.data.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["workers"][0]["slots"], 0, "it stays paused, in config.json: {config}");
    let (code, _) = p.here.post("/api/capacity", r#"{"worker": "w1", "slots": 1}"#, WRITE_HEADERS);
    assert_eq!(code, 200);
    assert_eq!(p.here.api_state()["machines"][1]["capacity"], 1);
    let (code, _) = p.here.post("/api/capacity", r#"{"worker": "nope", "slots": 1}"#, WRITE_HEADERS);
    assert_eq!(code, 400);
}

#[test]
fn sync_git_sends_the_checkouts_git_metadata_but_not_other_worktrees() {
    let p = pair(json!({}));
    let main = p.home.join("repo");
    fs::create_dir_all(&main).unwrap();
    let git = |dir: &Path, args: &[&str]| {
        let ok = std::process::Command::new("git").arg("-C").arg(dir).args(args).env("HOME", &p.home).output().unwrap();
        assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    };
    git(&main, &["init", "-q", "-b", "main"]);
    fs::write(main.join(".gitignore"), "build/\n").unwrap();
    fs::write(main.join("src.txt"), "x").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "first"]);
    let wt1 = p.home.join("repo/.wt/one");
    git(&main, &["worktree", "add", "-q", wt1.to_str().unwrap()]);
    git(&main, &["worktree", "add", "-q", p.home.join("repo/.wt/two").to_str().unwrap()]);
    fs::create_dir_all(wt1.join("build")).unwrap();
    fs::write(wt1.join("build/product"), "big").unwrap();
    p.here.submit("git", json!({"command": "git rev-parse HEAD", "cwd": wt1, "remote": "prefer", "sync_git": true}));
    assert_eq!(p.here.wait_exit("git", T), 0, "{}", p.here.log_of("git"));
    let there = |path: &Path| p.fake.join("root").join(path.strip_prefix("/").unwrap());
    let common = there(&main.canonicalize().unwrap().join(".git"));
    assert!(
        common.join("objects").is_dir() && common.join("worktrees/one").is_dir(),
        "the checkout's git metadata went"
    );
    assert!(!common.join("worktrees/two").exists(), "another worktree's entry did not");
    assert!(!common.join("HEAD").exists(), "nor the main worktree's HEAD");
    let wt = there(&wt1.canonicalize().unwrap());
    assert!(wt.join(".git").is_file() && wt.join("src.txt").is_file());
    assert!(!wt.join("build").exists(), "git-ignored products stay here");
}

#[test]
fn stopping_the_dispatcher_leaves_a_job_running_on_its_worker() {
    let mut p = pair(json!({}));
    let proj = project(&p.home, "proj");
    p.here.submit("job", json!({"command": "echo one; sleep 3; echo two", "cwd": proj, "remote": "prefer"}));
    wait_for(T, || p.here.log_of("job").contains("one\n").then_some(())).unwrap();
    assert_eq!(p.here.stop_daemon(), 0);
    let name = p.here.status_of("job").unwrap()["remote_run"]["job"].as_str().unwrap().to_string();
    assert_eq!(p.worker.status_of(&name).unwrap()["state"], "running", "it carries on there");
    p.here.launch();
    assert_eq!(p.here.wait_exit("job", T), 0);
    assert!(p.here.log_of("job").contains("two\n"));
    p.here.wait_daemon_log("job on a worker followed again");
}
