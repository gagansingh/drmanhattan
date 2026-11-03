//! Services: long-running servers that are ready rather than finished and take no slot.

mod common;

use std::fs;
use std::process::Output;
use std::time::Duration;

use common::{group_exists, wait_for, Daemon, WRITE_HEADERS};
use serde_json::json;

const T: Duration = Duration::from_secs(30);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A server on `port` that also prints a ready line after a moment.
fn server(port: u16) -> String {
    format!("sleep 0.3; echo 'Listening on {port}'; exec python3 -m http.server {port} --bind 127.0.0.1")
}

fn submit(d: &Daemon, args: &[&str]) -> Output {
    let mut all = vec!["submit"];
    all.extend_from_slice(args);
    d.cmd(&all).current_dir(&d.work).output().unwrap()
}

fn said(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

#[test]
fn a_service_is_ready_by_its_port_and_takes_no_slot() {
    let d = Daemon::start(&[]);
    d.submit("blocker", d.job("sleep 60 & wait"));
    d.wait_state("blocker", "running", T);
    let port = free_port();
    let out = submit(
        &d,
        &["--service", "--label", "web", "--ready-port", &port.to_string(), "--", "sh", "-c", &server(port)],
    );
    assert!(out.status.success(), "{}", said(&out));
    let text = said(&out);
    assert!(text.contains("web ready · pid ") && text.contains(&format!("· port {port} · log ")), "{text}");
    assert!(text.contains("manhattan stop web"), "{text}");
    std::net::TcpStream::connect(("127.0.0.1", port)).expect("it is serving");
    let s = d.status_of("web").unwrap();
    assert_eq!((&s["state"], &s["service"]), (&json!("running"), &json!(true)));
    assert!(s["ready_at"].is_string() && s["slot"].is_null(), "{s}");
    assert_eq!(d.status_of("blocker").unwrap()["state"], "running", "the only slot was never needed");
    let state = d.api_state();
    assert_eq!(state["services"][0]["port"], port);
    assert_eq!(state["running"].as_array().unwrap().len(), 1, "services are listed apart");
    // Stopping it ends its whole group.
    let pgid = s["process_group"].as_i64().unwrap() as i32;
    let out = d.cmd(&["stop", "web"]).output().unwrap();
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(d.status_of("web").unwrap()["state"], "cancelled");
    assert!(!group_exists(pgid));
}

#[test]
fn a_service_is_ready_by_a_line_of_its_log() {
    let d = Daemon::start(&[]);
    let out = submit(
        &d,
        &[
            "--service",
            "--label",
            "api",
            "--ready-log",
            r"^Listening on \d+",
            "--",
            "sh",
            "-c",
            "sleep 0.3; echo 'Listening on 4000'; sleep 60",
        ],
    );
    assert!(out.status.success(), "{}", said(&out));
    assert!(said(&out).contains("api ready"));
    assert!(d.log_of("api").contains("drmanhattan · ready after"));
    d.cmd(&["stop", "api"]).output().unwrap();
}

#[test]
fn a_service_not_ready_in_time_is_stopped_and_says_why() {
    let d = Daemon::start(&[]);
    let port = free_port();
    let out = submit(
        &d,
        &[
            "--service",
            "--label",
            "slow",
            "--ready-port",
            &port.to_string(),
            "--ready-timeout",
            "1",
            "--",
            "sh",
            "-c",
            "echo booting; sleep 60",
        ],
    );
    assert!(!out.status.success());
    let text = said(&out);
    assert!(text.contains("booting") && text.contains("slow did not become ready: not ready within 1.0s"), "{text}");
    let s = d.status_of("slow").unwrap();
    assert_eq!(s["state"], "failed");
    // One that exits first fails at once with its own status.
    let out = submit(
        &d,
        &["--service", "--label", "crash", "--ready-port", &port.to_string(), "--", "sh", "-c", "echo boom; exit 3"],
    );
    assert_eq!(out.status.code(), Some(3), "{}", said(&out));
    assert!(said(&out).contains("it exited before it was ready"));
}

#[test]
fn the_same_service_is_reused_and_restart_replaces_it() {
    let d = Daemon::start(&[]);
    let port = free_port();
    let args = ["--service", "--label", "web", "--ready-port", &port.to_string(), "--", "sh", "-c", &server(port)];
    let args: Vec<&str> = args.iter().map(|s| s.as_ref()).collect();
    assert!(submit(&d, &args).status.success());
    let first = d.run_id_of("web");
    let again = submit(&d, &args);
    assert!(again.status.success() && said(&again).contains("web already running"), "{}", said(&again));
    assert_eq!(d.run_id_of("web"), first, "no second copy started");
    let mut restart = vec!["--restart"];
    restart.extend(&args);
    let out = submit(&d, &restart);
    assert!(out.status.success() && said(&out).contains("web ready"), "{}", said(&out));
    assert_ne!(d.run_id_of("web"), first);
    // The dashboard's restart does the same from the page.
    let second = d.run_id_of("web");
    let (code, body) =
        d.post("/api/service", &format!(r#"{{"run_id": "{second}", "action": "restart"}}"#), WRITE_HEADERS);
    assert_eq!(code, 200, "{body}");
    wait_for(T, || {
        let s = d.status_of("web")?;
        (s["run_id"] != second && s["ready_at"].is_string()).then_some(())
    })
    .expect("restarted and ready");
    let third = d.run_id_of("web");
    let (code, _) = d.post("/api/service", &format!(r#"{{"run_id": "{third}", "action": "stop"}}"#), WRITE_HEADERS);
    assert_eq!(code, 200);
    assert_eq!(d.wait_exit("web", T), 143);
    let (code, _) = d.post("/api/service", &format!(r#"{{"run_id": "{third}", "action": "stop"}}"#), WRITE_HEADERS);
    assert_eq!(code, 400, "a service that has ended is not stopped again");
}

#[test]
fn a_service_past_its_ttl_is_stopped() {
    let d = Daemon::start(&[]);
    let out = submit(&d, &["--service", "--label", "brief", "--ttl", "1", "--", "sleep", "60"]);
    assert!(out.status.success(), "{}", said(&out));
    assert_eq!(d.wait_exit("brief", T), 143);
    let s = d.status_of("brief").unwrap();
    assert_eq!(s["state"], "cancelled");
    assert!(s["failure_reason"].as_str().unwrap().contains("ttl"), "{s}");
}

#[test]
fn stopping_the_daemon_stops_its_services_and_waits_for_nothing() {
    let mut d = Daemon::start(&[("DRMANHATTAN_SHUTDOWN_GRACE_SECONDS", "30")]);
    let out = submit(&d, &["--service", "--label", "svc", "--", "sleep", "300"]);
    assert!(out.status.success(), "{}", said(&out));
    let pgid = d.status_of("svc").unwrap()["process_group"].as_i64().unwrap() as i32;
    let started = std::time::Instant::now();
    assert_eq!(d.stop_daemon(), 0);
    assert!(started.elapsed() < Duration::from_secs(15), "the grace period is not spent on a service");
    assert!(!group_exists(pgid), "its group is gone");
    assert_eq!(d.status_of("svc").unwrap()["state"], "cancelled");
    let _ = fs::read_dir(&d.inbox);
}

#[test]
fn service_options_are_checked() {
    let d = Daemon::start(&[]);
    let out = submit(&d, &["--service", "--wait", "--", "true"]);
    assert!(!out.status.success() && said(&out).contains("a service never finishes"));
    let out = submit(&d, &["--ready-port", "80", "--", "true"]);
    assert!(!out.status.success() && said(&out).contains("are for --service"));
    d.submit("bad", json!({"command": ["true"], "ready": {"port": 80}}));
    assert_eq!(d.wait_exit("bad", T), 2);
}
