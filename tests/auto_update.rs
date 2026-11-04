//! Auto-update: a daemon installed from a copy, against a stand-in release server.

mod common;

use std::fs;
use std::path::Path;
use std::time::Duration;

use common::releases::{installed, releases, Releases};
use common::{wait_for, Daemon};
use serde_json::Value;

const T: Duration = Duration::from_secs(40);
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The next minor release after this one, which auto-update may install.
fn next_minor() -> String {
    let mut parts: Vec<u64> = CURRENT.split('.').map(|p| p.parse().unwrap()).collect();
    parts[1] += 1;
    format!("v{}.{}.0", parts[0], parts[1])
}

/// A queue run by its own copy of drmanhattan, with releases at `tag`, a short idle wait and
/// a fast check loop, plus any `extra` settings.
fn queue_with(tag: &str, good_checksum: bool, extra: &[(&str, &str)]) -> (Daemon, tempdir::Dir, Releases) {
    let dir = tempdir::Dir::new("auto");
    let r = releases(&dir.0, tag, good_checksum);
    let mut env = vec![
        ("DRMANHATTAN_NO_UPDATE_CHECK", "0"),
        ("DRMANHATTAN_RELEASES_URL", r.url.as_str()),
        ("DRMANHATTAN_AUTO_UPDATE_IDLE_SECONDS", "2"),
        ("DRMANHATTAN_UPDATE_TICK_SECONDS", "0.5"),
    ];
    env.extend_from_slice(extra);
    let d = Daemon::start_with(&env, |root| installed(root).join("drmanhattan"));
    // The first check runs a tick after start; wait for what it found.
    wait_for(T, || d.data.join("update-check.json").exists().then_some(())).expect("the release check ran");
    (d, dir, r)
}

fn queue(tag: &str, good_checksum: bool) -> (Daemon, tempdir::Dir) {
    let (d, dir, _) = queue_with(tag, good_checksum, &[]);
    (d, dir)
}

fn check(d: &Daemon) -> Value {
    serde_json::from_str(&fs::read_to_string(d.data.join("update-check.json")).unwrap_or_default()).unwrap_or_default()
}

fn auto_state(d: &Daemon) -> String {
    d.status_json()["auto_update_state"].as_str().unwrap_or_default().to_string()
}

fn version_of(program: &Path) -> String {
    let out = std::process::Command::new(program).arg("--version").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn auto(d: &Daemon, on: &str) {
    assert!(d.cmd(&["update", "--auto", on]).output().unwrap().status.success());
}

fn updated(d: &Daemon) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(d.data.join("updated.json")).ok()?).ok()
}

#[test]
fn it_is_off_by_default() {
    let (d, _dir) = queue(&next_minor(), true);
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(version_of(&d.program), format!("drmanhattan {CURRENT}"));
    assert_eq!(d.status_json()["auto_update"], false);
    assert_eq!(auto_state(&d), "off");
    assert_eq!(d.api_state()["update"]["version"], next_minor().trim_start_matches('v'));
}

#[test]
fn it_waits_for_an_idle_queue_and_new_work_resets_the_wait() {
    let (d, _dir) = queue(&next_minor(), true);
    auto(&d, "on");
    assert_eq!(d.status_json()["auto_update"], true);
    d.submit("first", d.job("sleep 3"));
    d.wait_state("first", "running", T);
    let found = next_minor().trim_start_matches('v').to_string();
    wait_for(T, || auto_state(&d).contains(&format!("{found} found, queue busy")).then_some(())).expect("busy");
    d.wait_exit("first", T);
    // Arrives inside the idle wait: the wait starts again once it is done.
    wait_for(T, || auto_state(&d).contains("found, waiting for 1 min idle").then_some(())).expect("waiting");
    d.submit("second", d.job("sleep 2"));
    assert_eq!(d.wait_exit("second", T), 0, "never updated mid-job");
    let second_done = common::epoch(&d.status_of("second").unwrap()["finished_at"]);
    let note = wait_for(T, || updated(&d)).expect("it updated once idle");
    assert!(common::epoch(&note["at"]) >= second_done + 1.5, "the wait restarted after the second job: {note}");
    assert_eq!((&note["auto"], note["to"].as_str()), (&Value::Bool(true), Some(next_minor().trim_start_matches('v'))));
    assert_eq!(version_of(&d.program), format!("drmanhattan {}", next_minor().trim_start_matches('v')));
}

#[test]
fn a_major_release_is_only_announced() {
    let (d, _dir) = queue("v99.0.0", true);
    auto(&d, "on");
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(version_of(&d.program), format!("drmanhattan {CURRENT}"));
    assert_eq!(d.api_state()["update"]["version"], "99.0.0");
}

#[test]
fn a_bad_checksum_keeps_the_running_version_and_says_why() {
    let (d, _dir) = queue(&next_minor(), false);
    auto(&d, "on");
    d.wait_daemon_log("failed: the checksum");
    assert_eq!(version_of(&d.program), format!("drmanhattan {CURRENT}"));
    assert!(updated(&d).is_none());
    assert!(auto_state(&d).starts_with("last attempt failed: the checksum"), "{}", auto_state(&d));
}

#[test]
fn a_failed_install_is_tried_again_after_the_back_off() {
    let (d, _dir, _r) = queue_with(&next_minor(), false, &[("DRMANHATTAN_AUTO_UPDATE_RETRY_SECONDS", "2")]);
    auto(&d, "on");
    let tries = || d.daemon_log().matches("failed: the checksum").count();
    wait_for(T, || (tries() >= 2).then_some(())).expect("tried again after the back-off");
}

#[test]
fn a_release_published_later_is_found_after_the_interval() {
    let (d, _dir, r) = queue_with(&next_minor(), true, &[("DRMANHATTAN_UPDATE_CHECK_SECONDS", "2")]);
    r.set_latest(&format!("v{CURRENT}"));
    wait_for(T, || (check(&d)["latest"] == format!("v{CURRENT}")).then_some(())).expect("up to date");
    auto(&d, "on");
    assert!(auto_state(&d).starts_with("up to date (checked "), "{}", auto_state(&d));
    r.set_latest(&next_minor());
    wait_for(T, || (check(&d)["latest"] == next_minor()).then_some(())).expect("found on a later check");
}

#[test]
fn a_check_older_than_the_interval_runs_on_the_next_loop() {
    // As after a night asleep: the record is old, though the daemon's own clock barely moved.
    let (d, _dir) = queue(&format!("v{CURRENT}"), true);
    let stale = serde_json::json!({"checked_at": "2020-01-01T00:00:00.000Z", "latest": format!("v{CURRENT}")});
    fs::write(d.data.join("update-check.json"), stale.to_string()).unwrap();
    wait_for(T, || (!check(&d)["checked_at"].as_str()?.starts_with("2020")).then_some(())).expect("checked again");
}

/// A private temporary directory, removed when dropped.
mod tempdir {
    pub struct Dir(pub std::path::PathBuf);

    impl Dir {
        pub fn new(name: &str) -> Dir {
            let p = std::env::temp_dir().join(format!("drmanhattan-{name}-{}-{}", std::process::id(), rand()));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn rand() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }
}

#[test]
fn a_restarted_daemon_drops_the_last_ones_phase() {
    let (mut d, _dir) = queue(&format!("v{CURRENT}"), true);
    auto(&d, "on");
    d.kill_daemon();
    // What the update that restarted it leaves behind.
    fs::write(d.data.join("auto-update.json"), r#"{"phase":"updating","version":"9.9.9"}"#).unwrap();
    d.launch();
    wait_for(T, || auto_state(&d).starts_with("up to date").then_some(())).expect("not stuck on updating");
}
