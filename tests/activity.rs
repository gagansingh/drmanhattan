//! What a running job is doing, as the dashboard shows it.

mod common;

use std::fs;
use std::time::Duration;

use common::{wait_for, Daemon};
use serde_json::json;

const T: Duration = Duration::from_secs(30);

#[test]
fn activity_markers_show_what_a_run_is_doing() {
    let d = Daemon::start(&[]);
    let script = "echo '::step 1/3 Fetching'; echo '::start download'; sleep 0.6; echo '::done download'; \
                  echo '::step 2/3 Building'; echo '::start compile'; echo 'some compiler output'; sleep 30";
    d.submit("act", json!({"command": script, "cwd": d.work, "label": "act", "activity": null}));
    d.wait_state("act", "running", T);
    let act = wait_for(T, || {
        let st = d.api_state();
        let a = st["running"][0]["activity"].clone();
        (a["step"]["n"] == 2 && a["active"].as_array().map(|v| v.len() == 1).unwrap_or(false)).then_some(a)
    })
    .expect("activity reflects the markers");
    assert_eq!(act["step"]["of"], 3);
    assert_eq!(act["step"]["text"], "Building");
    assert_eq!(act["active"][0]["label"], "compile");
    assert!(act["active"][0]["typical"].is_null(), "compile has never finished before");
    assert_eq!(act["last"], "some compiler output");
    // the finished `download` command taught the queue its usual duration
    let steps = fs::read_to_string(d.data.join("steps.jsonl")).unwrap();
    assert!(steps.contains("\"label\":\"download\""), "{steps}");
    fs::write(d.inbox.join("act.cancel"), "").unwrap();
    d.wait_exit("act", T);
    // a second run of a known command carries its typical
    d.submit("act2", json!({"command": "echo '::start download'; sleep 30", "cwd": d.work}));
    d.wait_state("act2", "running", T);
    let act = wait_for(T, || {
        let st = d.api_state();
        let a = st["running"][0]["activity"].clone();
        (a["active"].as_array().map(|v| v.len() == 1).unwrap_or(false)).then_some(a)
    })
    .unwrap();
    assert_eq!(act["active"][0]["label"], "download");
    assert!(act["active"][0]["typical"].is_number());
    fs::write(d.inbox.join("act2.cancel"), "").unwrap();
    d.wait_exit("act2", T);
}

#[test]
fn custom_activity_patterns_match_a_tools_own_output() {
    let d = Daemon::start(&[]);
    let script = "echo '[2/9] Build image'; echo '[10:00:00] docker build started; streaming command output'; echo progress; sleep 30";
    d.submit(
        "custom",
        json!({"command": script, "cwd": d.work, "activity": {
            "step": r"^\[(?P<n>\d+)/(?P<of>\d+)\] (?P<text>.*)$",
            "start": r"^\[\d\d:\d\d:\d\d\] (?P<label>.+?) started; streaming",
            "done": r"^\[\d\d:\d\d:\d\d\] (?P<label>.+?) finished in"
        }}),
    );
    d.wait_state("custom", "running", T);
    let act = wait_for(T, || {
        let st = d.api_state();
        let a = st["running"][0]["activity"].clone();
        (a["active"].as_array().map(|v| v.len() == 1).unwrap_or(false)).then_some(a)
    })
    .expect("custom patterns match");
    assert_eq!(act["step"]["n"], 2);
    assert_eq!(act["step"]["text"], "Build image");
    assert_eq!(act["active"][0]["label"], "docker build");
    fs::write(d.inbox.join("custom.cancel"), "").unwrap();
    d.wait_exit("custom", T);
    // a bad regex is rejected up front
    d.submit("badre", json!({"command": ["true"], "cwd": d.work, "activity": {"step": "("}}));
    assert_eq!(d.wait_exit("badre", T), 2);
    assert!(d.status_of("badre").unwrap()["failure_reason"].as_str().unwrap().contains("activity.step"));
    d.submit("nogroup", json!({"command": ["true"], "cwd": d.work, "activity": {"start": "go (.*)"}}));
    assert_eq!(d.wait_exit("nogroup", T), 2);
    assert!(d.status_of("nogroup").unwrap()["failure_reason"].as_str().unwrap().contains("(?P<label>"));
}
