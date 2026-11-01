mod common;

use std::fs;
use std::time::Duration;

use common::{Daemon, WRITE_HEADERS};
use serde_json::{json, Value};

const T: Duration = Duration::from_secs(30);

#[test]
fn page_and_state_before_any_run() {
    let d = Daemon::start(&[]);
    let (code, body) = d.get("/");
    assert_eq!(code, 200);
    assert!(body.contains("<title>drmanhattan</title>") && body.contains("X-Drmanhattan"));
    let state = d.api_state();
    assert_eq!(state["capacity"], 1);
    assert_eq!(state["slots"], json!([{"slot": 1, "run_id": null}]));
    // CPU usage is sampled in the background; the first figure arrives within about a second.
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        let pct =
            common::wait_for(Duration::from_secs(10), || d.api_state()["cpu_pct"].as_u64()).expect("a CPU figure");
        assert!(pct <= 100);
    }
    assert_eq!(state["running"], json!([]));
    assert_eq!(state["queued"], json!([]));
    assert_eq!(state["done"], json!([]));
    assert_eq!(state["metrics_24h"]["runs"], 0);
    assert_eq!(d.get("/nope").0, 404);
}

#[test]
fn state_reflects_runs_and_the_log_is_served() {
    let mut d = Daemon::start(&[]);
    d.seed_history(&[
        ("sleep", "blocker", 10),
        ("sleep", "blocker", 10),
        ("sleep", "blocker", 10),
        ("sleep", "blocker", 10),
    ]);
    d.submit("earlier", json!({"command": "echo hello; exit 2", "cwd": d.work}));
    assert_eq!(d.wait_exit("earlier", T), 2);
    d.submit("blocker", json!({"command": "sleep 2", "cwd": d.work, "label": "blocker", "category": "sleep"}));
    d.wait_state("blocker", "running", T);
    d.submit("waiter", json!({"command": "true", "cwd": d.work, "variant": "cold"}));
    d.wait_state("waiter", "queued", T);
    let state = d.api_state();
    let running = &state["running"][0];
    assert_eq!(running["label"], "blocker");
    assert_eq!(running["typical_seconds"], 10);
    assert_eq!(running["slot"], 1);
    assert!(running["remaining_estimate"].is_number() && running["elapsed"].is_number());
    assert_eq!(state["slots"][0]["run_id"], running["run_id"]);
    let queued = &state["queued"][0];
    assert_eq!(queued["label"], "waiter");
    assert_eq!(queued["variant"], "cold");
    assert!(queued["eta_start"].as_f64().unwrap() >= state["now"].as_f64().unwrap());
    let done = &state["done"][0];
    assert_eq!(done["label"], "earlier");
    assert_eq!(done["state"], "failed");
    assert_eq!(done["exit_status"], 2);
    assert_eq!(done["duration"], 0);
    assert_eq!(state["metrics_24h"]["runs"], 1);
    let rid = done["run_id"].as_str().unwrap();
    let (code, body) = d.get(&format!("/api/log?run_id={rid}&lines=50"));
    assert_eq!(code, 200);
    assert!(body.contains("hello"));
    assert_eq!(d.get("/api/log?run_id=nope").0, 400);
    assert_eq!(d.get("/api/log?run_id=1-2-abcdef").0, 404);
    d.wait_exit("waiter", T);
}

#[test]
fn post_guards() {
    let d = Daemon::start(&[]);
    let port = d.url.rsplit(':').next().unwrap();
    let body = r#"{"slots": 2}"#;
    // no custom header
    assert_eq!(d.post("/api/capacity", body, &[("Content-Type", "application/json")]).0, 403);
    // wrong content type (a cross-site form post)
    assert_eq!(d.post("/api/capacity", body, &[("Content-Type", "text/plain"), ("X-Drmanhattan", "1")]).0, 403);
    // foreign Host (DNS rebinding)
    assert_eq!(
        d.post(
            "/api/capacity",
            body,
            &[("Content-Type", "application/json"), ("X-Drmanhattan", "1"), ("Host", &format!("evil.example:{port}"))]
        )
        .0,
        403
    );
    // foreign Origin
    assert_eq!(
        d.post(
            "/api/capacity",
            body,
            &[("Content-Type", "application/json"), ("X-Drmanhattan", "1"), ("Origin", "http://evil.example")]
        )
        .0,
        403
    );
    assert!(!d.state.join("capacity-override").exists());
    // the page itself
    let origin = format!("http://127.0.0.1:{port}");
    let (code, resp) = d.post(
        "/api/capacity",
        body,
        &[("Content-Type", "application/json"), ("X-Drmanhattan", "1"), ("Origin", &origin)],
    );
    assert_eq!(code, 200, "{resp}");
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["override"]["slots"], 2);
    assert!(d.state.join("capacity-override").exists());
    assert_eq!(d.status_json()["capacity"], 2);
    assert_eq!(d.post("/api/capacity", r#"{"slots": 99}"#, WRITE_HEADERS).0, 400);
    assert_eq!(d.post("/api/capacity", r#"{"slots": "2"}"#, WRITE_HEADERS).0, 400);
    assert_eq!(d.post("/api/capacity", r#"[1]"#, WRITE_HEADERS).0, 400);
    let (code, _) = d.post("/api/capacity", r#"{"slots": null}"#, WRITE_HEADERS);
    assert_eq!(code, 200);
    assert!(!d.state.join("capacity-override").exists());
    assert_eq!(d.post("/api/other", "{}", WRITE_HEADERS).0, 404);
}

#[test]
fn queue_order_post_writes_only_live_non_priority_waiters() {
    let d = Daemon::start(&[]);
    d.submit("blocker", d.job("sleep 2.5"));
    d.wait_state("blocker", "running", T);
    d.submit("a", d.job("true"));
    d.submit("b", d.job("true"));
    d.submit("p", json!({"command": "true", "cwd": d.work, "priority": true}));
    for n in ["a", "b", "p"] {
        d.wait_state(n, "queued", T);
    }
    let (a, b, p) = (d.run_id_of("a"), d.run_id_of("b"), d.run_id_of("p"));
    assert_eq!(d.post("/api/queue-order", r#"{"order": ["x"]}"#, WRITE_HEADERS).0, 400);
    let (code, resp) =
        d.post("/api/queue-order", &json!({"order": [b, p, a, "1-2-abcdef"]}).to_string(), WRITE_HEADERS);
    assert_eq!(code, 200, "{resp}");
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["order"], json!([b, a]));
    assert_eq!(fs::read_to_string(d.state.join("manual-order")).unwrap(), format!("{b}\n{a}\n"));
    let state = d.api_state();
    let order: Vec<&str> = state["queued"].as_array().unwrap().iter().map(|r| r["run_id"].as_str().unwrap()).collect();
    assert_eq!(order, vec![p.as_str(), b.as_str(), a.as_str()]);
    assert_eq!(state["manual_order"], true);
    assert_eq!(state["queued"][1]["manual"], true);
    let (code, _) = d.post("/api/queue-order", r#"{"order": null}"#, WRITE_HEADERS);
    assert_eq!(code, 200);
    assert!(!d.state.join("manual-order").exists());
    for n in ["a", "b", "p"] {
        d.wait_exit(n, T);
    }
}

#[test]
fn other_hosts_are_refused_and_the_page_cannot_be_framed() {
    let d = Daemon::start(&[]);
    // DNS rebinding: a hostile name that resolves to 127.0.0.1 must not read anything.
    for path in ["/", "/api/state", "/api/log?run_id=1-1-a"] {
        assert_eq!(d.http("GET", path, None, &[("Host", "evil.example:80")]).0, 403, "{path}");
    }
    let (code, head, _) = d.exchange("GET", "/", None, &[]);
    assert_eq!(code, 200);
    assert!(head.contains("X-Frame-Options: DENY") && head.contains("frame-ancestors 'none'"), "{head}");
}
