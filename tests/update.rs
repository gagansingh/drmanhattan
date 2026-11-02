//! `drmanhattan update`, against a stand-in release server rather than GitHub.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use common::{bin, wait_for, Daemon, WRITE_HEADERS};

const T: Duration = Duration::from_secs(30);
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A release server: `/latest` redirects to `/tag/<tag>`, and `/download/<tag>/` serves this
/// platform's archive (two scripts that say they are `<tag>`) and its checksum.
struct Releases {
    url: String,
}

fn asset(tag: &str) -> String {
    let platform = if cfg!(target_os = "macos") { "macos-universal" } else { "linux-x86_64" };
    format!("drmanhattan-{tag}-{platform}.tar.gz")
}

fn releases(root: &Path, tag: &str, good_checksum: bool) -> Releases {
    let pack = root.join("pack/drmanhattan");
    fs::create_dir_all(&pack).unwrap();
    let version = tag.trim_start_matches('v');
    for program in ["drmanhattan", "manhattan"] {
        let p = pack.join(program);
        fs::write(&p, format!("#!/bin/sh\necho 'drmanhattan {version}'\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let archive = root.join(asset(tag));
    assert!(std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(root.join("pack"))
        .arg("drmanhattan")
        .status()
        .unwrap()
        .success());
    let sum = if good_checksum {
        let out = std::process::Command::new("shasum").args(["-a", "256"]).arg(&archive).output().unwrap();
        String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap().to_string()
    } else {
        "0".repeat(64)
    };
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr().to_ip().unwrap());
    let (tag, bytes, name) = (tag.to_string(), fs::read(&archive).unwrap(), asset(tag));
    let base = url.clone();
    std::thread::spawn(move || {
        while let Ok(req) = server.recv() {
            let path = req.url().to_string();
            let response = if path == "/latest" {
                let to = tiny_http::Header::from_bytes("Location", format!("{base}/tag/{tag}")).unwrap();
                tiny_http::Response::from_data(Vec::new()).with_status_code(302).with_header(to)
            } else if path == format!("/tag/{tag}") {
                tiny_http::Response::from_data(b"release notes".to_vec())
            } else if path == format!("/download/{tag}/{name}") {
                tiny_http::Response::from_data(bytes.clone())
            } else if path == format!("/download/{tag}/{name}.sha256") {
                tiny_http::Response::from_data(format!("{sum}  {name}\n").into_bytes())
            } else {
                tiny_http::Response::from_data(b"not found".to_vec()).with_status_code(404)
            };
            let _ = req.respond(response);
        }
    });
    Releases { url }
}

/// A copy of drmanhattan installed in a directory of its own, as the installer would.
fn installed(root: &Path) -> PathBuf {
    let dir = root.join("bin");
    fs::create_dir_all(&dir).unwrap();
    for program in ["drmanhattan", "manhattan"] {
        fs::copy(bin(), dir.join(program)).unwrap();
    }
    dir
}

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
