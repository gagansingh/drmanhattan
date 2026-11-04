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
    // Long enough not to be quick, so it holds the slot.
    d.seed_history(&[
        ("sleep", "blocker", 40),
        ("sleep", "blocker", 40),
        ("sleep", "blocker", 40),
        ("sleep", "blocker", 40),
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
    assert_eq!(running["typical_seconds"], 40.0);
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
    assert!(done["duration"].as_f64().unwrap() < 1.0, "durations keep their milliseconds");
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
    assert_eq!(d.status_json()["capacity"], 1, "none of those changed it");
    // the page itself
    let origin = format!("http://127.0.0.1:{port}");
    let (code, resp) = d.post(
        "/api/capacity",
        body,
        &[("Content-Type", "application/json"), ("X-Drmanhattan", "1"), ("Origin", &origin)],
    );
    assert_eq!(code, 200, "{resp}");
    let v: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(v["slots"], 2);
    assert_eq!(d.status_json()["capacity"], 2);
    let config: Value = serde_json::from_str(&fs::read_to_string(d.data.join("config.json")).unwrap()).unwrap();
    assert_eq!(config["slots"], 2, "it stays: the standing count, as `slots 2` sets it");
    assert_eq!(d.post("/api/capacity", r#"{"slots": 99}"#, WRITE_HEADERS).0, 200, "no ceiling below the guard");
    assert_eq!(d.post("/api/capacity", r#"{"slots": 257}"#, WRITE_HEADERS).0, 400);
    assert_eq!(d.post("/api/capacity", r#"{"slots": 0}"#, WRITE_HEADERS).0, 400);
    assert_eq!(d.post("/api/capacity", r#"{"slots": "2"}"#, WRITE_HEADERS).0, 400);
    assert_eq!(d.post("/api/capacity", r#"[1]"#, WRITE_HEADERS).0, 400);
    assert_eq!(d.post("/api/capacity", r#"{"slots": null}"#, WRITE_HEADERS).0, 400);
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

#[test]
fn commands_are_formatted_by_the_page_from_their_argv() {
    let d = Daemon::start(&[]);
    let (_, page) = d.get("/");
    assert!(page.contains(r#"<script src="command-format.js"></script>"#), "the page loads the formatter");
    // A long command never squeezes the output: one line in the strip, a capped box on demand.
    for needed in [r#"<div id="log-cmd" hidden></div>"#, "min-height: 60%", "max-height: min(6.5em, 20%)"] {
        assert!(page.contains(needed), "the page lacks {needed}");
    }
    let (code, format) = d.get("/command-format.js");
    assert_eq!(code, 200);
    for needed in ["function commandHtml(", "const cmdJoin = "] {
        assert!(format.contains(needed), "the formatter lacks {needed}");
    }
    // A run whose command has quotes, `&&` and a `--` wrapper still renders: the page gets its
    // argv untouched, and the log line it colours is the runner's quoting of that argv.
    let script = "echo 'a b' && true";
    let argv = json!(["bash", d.work.join("wrap.sh"), "--phase", "run", "--", "sh", "-c", script]);
    fs::write(d.work.join("wrap.sh"), "while [ \"$1\" != -- ]; do shift; done; shift; exec \"$@\"\n").unwrap();
    d.submit("wrapped", json!({"command": argv, "cwd": d.work, "prepare": ["true"]}));
    assert_eq!(d.wait_exit("wrapped", T), 0);
    let state = d.api_state();
    let run = &state["done"][0];
    assert_eq!(run["command"], argv);
    assert_eq!(run["prepare_command"], json!(["true"]));
    assert!(state["home"].as_str().is_some_and(|h| !h.is_empty()));
    let log = d.log_of("wrapped");
    assert!(log.contains("drmanhattan · in ") && log.contains("-- sh -c 'echo '\\''a b'\\'' && true'"), "{log}");
    assert!(log.contains("a b\n"), "{log}");

    // The formatter's own cases, run with node where there is one.
    let Ok(node) = std::process::Command::new("node").arg("--version").output() else { return };
    if !node.status.success() {
        return;
    }
    let esc = page.lines().skip_while(|l| !l.starts_with("const esc =")).take(2).collect::<Vec<_>>().join("\n");
    let cases = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/command_format.js")).unwrap();
    let program = format!("{esc}\n{format}\n{cases}");
    let out = std::process::Command::new("node").arg("-e").arg(program).output().unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && said.starts_with("ok "), "{said}");
}

#[test]
fn the_pane_shows_a_runs_command_once() {
    let d = Daemon::start(&[]);
    let argv = json!(["sh", "-c", "echo 'distinct output' && true"]);
    d.submit("once", json!({"command": argv, "cwd": d.work, "prepare": ["true"]}));
    assert_eq!(d.wait_exit("once", T), 0);
    let log = d.log_of("once");
    let command = "sh -c 'echo '\\''distinct output'\\'' && true'";
    assert!(log.contains(command), "the log file keeps its framing: {log}");
    // The strip shows the command from the run's argv; the output must not show it again.
    assert_eq!(d.api_state()["done"][0]["command"], argv);
    let Ok(node) = std::process::Command::new("node").arg("--version").output() else { return };
    if !node.status.success() {
        return;
    }
    let (_, page) = d.get("/");
    let framing = page.lines().find(|l| l.starts_with("const FRAMING =")).expect("the page's framing pattern");
    let program = format!(
        "{framing}\nconst lines = {};\nconst shown = lines.filter((l) => !FRAMING.test(l));\n\
         if (shown.some((l) => l.includes({command:?}))) throw new Error(shown.join('\\n'));\n\
         if (!shown.some((l) => l === 'distinct output')) throw new Error('output missing');\nconsole.log('ok');",
        serde_json::to_string(&log.lines().collect::<Vec<_>>()).unwrap(),
    );
    let out = std::process::Command::new("node").arg("-e").arg(program).output().unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

#[test]
fn drmanhattan_slots_in_the_environment_decides_and_the_page_says_so() {
    let d = Daemon::start(&[("DRMANHATTAN_SLOTS", "3")]);
    let (code, body) = d.post("/api/capacity", r#"{"slots": 4}"#, WRITE_HEADERS);
    assert_eq!(code, 400);
    assert!(body.contains("DRMANHATTAN_SLOTS=3"), "{body}");
    let st = d.api_state();
    assert_eq!((&st["capacity"], &st["capacity_fixed"]), (&json!(3), &json!("DRMANHATTAN_SLOTS=3")));
}

#[test]
fn finished_runs_come_a_page_at_a_time() {
    let d = Daemon::start(&[("DRMANHATTAN_SLOTS", "4")]);
    for i in 0..30 {
        d.submit(&format!("ok{i}"), json!({"command": "true", "cwd": d.work, "label": format!("ok {i}")}));
    }
    for i in 0..5 {
        d.submit(&format!("bad{i}"), json!({"command": "exit 1", "cwd": d.work, "label": format!("bad {i}")}));
    }
    let state = |query: &str| {
        let (code, body) = d.get(&format!("/api/state?{query}"));
        assert_eq!(code, 200, "{body}");
        serde_json::from_str::<Value>(&body).unwrap()
    };
    let ids = |state: &Value| -> Vec<String> {
        state["done"].as_array().unwrap().iter().map(|r| r["run_id"].as_str().unwrap().to_string()).collect()
    };
    common::wait_for(Duration::from_secs(60), || (state("")["finished"]["counts"]["all"] == 35).then_some(()))
        .expect("all 35 finished");

    let first = state("");
    let view = &first["finished"];
    assert_eq!(view["counts"], json!({"all": 35, "failed": 5, "succeeded": 30, "cancelled": 0}));
    assert_eq!((&view["newer"], &view["newer_from"]), (&json!(0), &Value::Null));
    assert_eq!(view["latest"].as_array().unwrap().len(), 20);
    let page1 = ids(&first);
    assert_eq!(page1.len(), 20);
    let older = view["older_from"].as_str().expect("an older page");

    let second = state(&format!("from={older}"));
    let page2 = ids(&second);
    assert_eq!((page2.len(), page2[0].as_str()), (15, older));
    assert_eq!(second["finished"]["newer"], 20);
    assert_eq!((&second["finished"]["older_from"], &second["finished"]["newer_from"]), (&Value::Null, &Value::Null));
    let all: std::collections::HashSet<_> = page1.iter().chain(&page2).collect();
    assert_eq!(all.len(), 35, "the pages neither overlap nor skip");

    let failed = state("filter=failed");
    assert!(failed["done"].as_array().unwrap().iter().all(|r| r["state"] == "failed"));
    assert_eq!((ids(&failed).len(), &failed["finished"]["counts"]["all"]), (5, &json!(35)));
    let search = state("search=BAD+3");
    assert_eq!(search["finished"]["counts"]["all"], 1);
    assert_eq!(search["done"][0]["label"], "bad 3");

    // A page that starts at a run holds still as others finish, and counts them.
    let top = &page1[0];
    d.submit("late", json!({"command": "true", "cwd": d.work, "label": "late"}));
    d.wait_exit("late", T);
    let held = state(&format!("from={top}"));
    assert_eq!(ids(&held), page1);
    assert_eq!((&held["finished"]["newer"], &held["finished"]["newer_from"]), (&json!(1), &Value::Null));
    assert_eq!(state("")["done"][0]["label"], "late");

    // The log pane's run comes along whatever the page; a run no longer kept starts an
    // empty page, from which the newer pages can still be reached.
    let old = &page2[14];
    assert_eq!(state(&format!("open={old}"))["open"]["run_id"], json!(old));
    let gone = state("from=1-2-abcdef");
    assert_eq!((ids(&gone).len(), &gone["finished"]["newer"]), (0, &json!(36)));
    assert!(gone["finished"]["newer_from"].is_string());
}
