//! `drmanhattan update`, against a stand-in release server rather than GitHub.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use common::releases::{installed, releases, Releases};
use common::{wait_for, Daemon, WRITE_HEADERS};

const T: Duration = Duration::from_secs(30);
const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn update(d: &Daemon, dir: &Path, releases: &Releases, args: &[&str]) -> Output {
    let mut c = d.cmd(&[]);
    c.env("DRMANHATTAN_RELEASES_URL", &releases.url);
    let mut cmd = std::process::Command::new(dir.join("drmanhattan"));
    for (k, v) in c.get_envs() {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    cmd.arg("update").args(args).output().unwrap()
}

fn said(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn version_of(dir: &Path) -> String {
    let out = std::process::Command::new(dir.join("drmanhattan")).arg("--version").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn an_up_to_date_install_is_left_alone() {
    let mut d = Daemon::start(&[]);
    d.kill_daemon();
    let r = releases(&d.root, &format!("v{CURRENT}"), true);
    let dir = installed(&d.root);
    let out = update(&d, &dir, &r, &[]);
    assert!(out.status.success() && said(&out).contains("is already up to date"), "{}", said(&out));
    assert_eq!(version_of(&dir), format!("drmanhattan {CURRENT}"));
}

#[test]
fn a_newer_release_replaces_both_commands() {
    let mut d = Daemon::start(&[]);
    d.kill_daemon();
    let r = releases(&d.root, "v99.0.0", true);
    let dir = installed(&d.root);
    let out = update(&d, &dir, &r, &[]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(said(&out).contains(&format!("updated {CURRENT} → 99.0.0")), "{}", said(&out));
    assert_eq!(version_of(&dir), "drmanhattan 99.0.0");
    let manhattan = std::process::Command::new(dir.join("manhattan")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&manhattan.stdout).trim(), "drmanhattan 99.0.0");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2, "nothing staged is left behind");
}

#[test]
fn a_bad_checksum_keeps_the_installed_version() {
    let mut d = Daemon::start(&[]);
    d.kill_daemon();
    let r = releases(&d.root, "v99.0.0", false);
    let dir = installed(&d.root);
    let out = update(&d, &dir, &r, &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(said(&out).contains("checksum") && said(&out).contains("unchanged"), "{}", said(&out));
    assert_eq!(version_of(&dir), format!("drmanhattan {CURRENT}"));
}

#[test]
fn an_unwritable_install_directory_points_at_the_installer() {
    let mut d = Daemon::start(&[]);
    d.kill_daemon();
    let r = releases(&d.root, "v99.0.0", true);
    let dir = installed(&d.root);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let out = update(&d, &dir, &r, &[]);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(said(&out).contains("cannot write to") && said(&out).contains("install.sh | sh -s --"), "{}", said(&out));
    assert_eq!(version_of(&dir), format!("drmanhattan {CURRENT}"));
}

#[test]
fn update_waits_for_the_queue_and_now_does_not() {
    let d = Daemon::start(&[]);
    let r = releases(&d.root, "v99.0.0", true);
    let dir = installed(&d.root);
    d.submit("busy", d.job("sleep 2"));
    d.wait_state("busy", "running", T);
    let out = update(&d, &dir, &r, &[]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(said(&out).contains("waiting for busy (running) to finish (or run with --now)"), "{}", said(&out));
    assert_eq!(d.status_of("busy").unwrap()["state"], "succeeded", "it finished before the update went on");

    let d = Daemon::start(&[]);
    let r = releases(&d.root, "v99.0.0", true);
    let dir = installed(&d.root);
    d.submit("long", d.job("sleep 30"));
    d.wait_state("long", "running", T);
    let started = std::time::Instant::now();
    let out = update(&d, &dir, &r, &["--now"]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(said(&out).contains("updating now: running jobs end when the daemon restarts"), "{}", said(&out));
    assert!(started.elapsed() < Duration::from_secs(25), "--now did not wait for the job");
}

#[test]
fn the_dashboard_offers_the_update_only_when_it_can_run() {
    let tag = format!("v{CURRENT}");
    let root = std::env::temp_dir().join(format!("drmanhattan-update-dash-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    // The server says the current version is the latest, so a started update changes nothing.
    let r = releases(&root, &tag, true);
    let d = Daemon::start(&[("DRMANHATTAN_RELEASES_URL", &r.url)]);
    let state = d.api_state();
    assert_eq!((state["version"].as_str(), state["update"].is_null()), (Some(CURRENT), true));
    let (code, _) = d.post("/api/update", "{}", WRITE_HEADERS);
    assert_eq!(code, 400, "no newer release is known");
    // As if the daily check had found one.
    let check = serde_json::json!({"checked_at": chrono::Utc::now().to_rfc3339(), "latest": "v99.0.0"});
    fs::write(d.data.join("update-check.json"), check.to_string()).unwrap();
    let state = d.api_state();
    assert_eq!(state["update"]["version"], "99.0.0");
    assert_eq!(state["update"]["url"], format!("{}/tag/v99.0.0", r.url));
    let status = d.cmd(&["status"]).output().unwrap();
    assert!(String::from_utf8_lossy(&status.stdout).contains("update available=99.0.0"));
    let (code, _) = d.post("/api/update", "{}", &[("Content-Type", "application/json")]);
    assert_eq!(code, 403, "the write guards apply");
    d.submit("busy", d.job("sleep 30"));
    d.wait_state("busy", "running", T);
    assert_eq!(d.api_state()["idle"], false);
    let (code, _) = d.post("/api/update", "{}", WRITE_HEADERS);
    assert_eq!(code, 409, "not while a job runs");
    fs::write(d.inbox.join("busy.cancel"), "").unwrap();
    d.wait_exit("busy", T);
    wait_for(T, || (d.api_state()["idle"] == true).then_some(())).unwrap();
    let (code, body) = d.post("/api/update", "{}", WRITE_HEADERS);
    assert_eq!(code, 202, "{body}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn status_and_the_dashboard_say_when_the_daemon_runs_an_older_version() {
    let d = Daemon::start_with(&[], |root| installed(root).join("drmanhattan"));
    let line = |d: &Daemon| {
        String::from_utf8_lossy(&d.cmd(&["status"]).output().unwrap().stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    assert!(line(&d).contains(&format!("version={CURRENT} ")) && !line(&d).contains("installed="), "{}", line(&d));
    assert!(d.api_state()["installed"].is_null());
    // As if an update had replaced the file and the restart were still to come.
    let info = d.state.join("dashboard.json");
    let mut doc: serde_json::Value = serde_json::from_str(&fs::read_to_string(&info).unwrap()).unwrap();
    doc["version"] = "0.0.1".into();
    fs::write(&info, doc.to_string()).unwrap();
    assert!(
        line(&d).contains(" version=0.0.1 ")
            && line(&d).contains(&format!(" installed={CURRENT} running=0.0.1 restart=pending")),
        "{}",
        line(&d)
    );
    assert_eq!(d.status_json()["installed"], CURRENT);
    let replacement = d.program.with_extension("new");
    fs::write(&replacement, "#!/bin/sh\necho 'drmanhattan 9.9.9'\n").unwrap();
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&replacement, &d.program).unwrap(); // as an update swaps it in
    assert_eq!(d.api_state()["installed"], "9.9.9");
}

#[cfg(target_os = "macos")]
fn signature(program: &Path) -> String {
    let out = std::process::Command::new("codesign").arg("-dv").arg(program).output().unwrap();
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[cfg(target_os = "macos")]
#[test]
fn sign_signs_both_commands_before_installing_them() {
    let mut d = Daemon::start(&[]);
    d.kill_daemon();
    // `-` is an ad-hoc signature: no keychain needed.
    common::write_config(&d, serde_json::json!({"sign": {"identity": "-", "identifier": "test.drmanhattan"}}));
    let r = releases(&d.root, "v99.0.0", true);
    let dir = installed(&d.root);
    let out = update(&d, &dir, &r, &[]);
    assert!(out.status.success(), "{}", said(&out));
    for program in ["drmanhattan", "manhattan"] {
        assert!(signature(&dir.join(program)).contains("Identifier=test.drmanhattan"), "{program} is signed");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_failed_signature_keeps_the_installed_version() {
    let mut d = Daemon::start(&[]);
    d.kill_daemon();
    common::write_config(&d, serde_json::json!({"sign": {"identity": "No Such Identity"}}));
    let r = releases(&d.root, "v99.0.0", true);
    let dir = installed(&d.root);
    let out = update(&d, &dir, &r, &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(said(&out).contains("could not sign") && said(&out).contains("unchanged"), "{}", said(&out));
    assert_eq!(version_of(&dir), format!("drmanhattan {CURRENT}"));
}
