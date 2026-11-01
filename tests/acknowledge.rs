//! The one-time confirmation before the first start.

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::{bin, wait_for, Daemon};

const T: Duration = Duration::from_secs(30);
const HINT: &str = "Run `drmanhattan` once in a terminal to confirm, or pass --yes";

/// A queue of its own with no confirmation saved, and none in the environment.
struct Fresh {
    d: Daemon,
    data: PathBuf,
    state: PathBuf,
}

impl Fresh {
    fn new() -> Fresh {
        let d = Daemon::start(&[]);
        let (data, state) = (d.root.join("fresh-data"), d.root.join("fresh-state"));
        Fresh { d, data, state }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = self.d.cmd(args);
        c.env_remove("DRMANHATTAN_ACCEPT")
            .env("DRMANHATTAN_DATA_DIR", &self.data)
            .env("DRMANHATTAN_STATE_DIR", &self.state);
        c
    }

    fn acknowledged(&self) -> bool {
        self.data.join("acknowledged.json").exists()
    }

    /// Starts a daemon and waits until it is up; returns it to be stopped.
    fn serve(&self, extra: &[&str]) -> std::process::Child {
        let mut args = vec!["serve", "--port", "0", "--no-dashboard"];
        args.extend(extra);
        let child = self.cmd(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        wait_for(T, || self.state.join("dashboard.json").exists().then_some(())).expect("the daemon starts");
        child
    }

    fn stop(&self, mut child: std::process::Child) {
        assert!(self.cmd(&["stop"]).output().unwrap().status.success());
        let _ = child.wait();
    }
}

#[test]
fn without_a_terminal_the_first_start_says_how_to_confirm() {
    let f = Fresh::new();
    let out = f.cmd(&["serve", "--port", "0"]).stdin(Stdio::null()).output().unwrap();
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(HINT) && err.lines().count() == 1, "{err}");
    assert!(!f.acknowledged() && !f.state.join("dashboard.json").exists(), "nothing started");
}

#[test]
fn yes_confirms_once_and_for_good() {
    let f = Fresh::new();
    let daemon = f.serve(&["--yes"]);
    let saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(f.data.join("acknowledged.json")).unwrap()).unwrap();
    assert_eq!((saved["how"].as_str(), saved["version"].as_str()), (Some("--yes"), Some(env!("CARGO_PKG_VERSION"))));
    f.stop(daemon);
    // Later starts, with no terminal and no --yes, do not ask again.
    f.stop(f.serve(&[]));
}

#[test]
fn the_environment_variable_confirms_too() {
    let f = Fresh::new();
    let mut c = f.cmd(&["serve", "--port", "0", "--no-dashboard"]);
    let daemon = c.env("DRMANHATTAN_ACCEPT", "1").stdin(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    wait_for(T, || f.state.join("dashboard.json").exists().then_some(())).expect("the daemon starts");
    assert!(f.acknowledged());
    f.stop(daemon);
}

#[test]
fn other_commands_never_ask() {
    let f = Fresh::new();
    for args in [&["status"][..], &["paths"], &["stop"], &["submit", "--", "true"]] {
        let out = f.cmd(args).stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("Continue?"), "{args:?}");
    }
    assert!(!f.acknowledged());
}

/// Runs `drmanhattan <args>` in a pseudo-terminal, typing `input`.
fn in_a_terminal(f: &Fresh, args: &[&str], input: &str) -> std::process::Child {
    let program = bin();
    let mut c = if cfg!(target_os = "macos") {
        let mut c = Command::new("script");
        c.arg("-q").arg("/dev/null").arg(&program).args(args);
        c
    } else {
        let line = std::iter::once(program.to_string_lossy().into_owned())
            .chain(args.iter().map(|a| a.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        let mut c = Command::new("script");
        c.args(["-qec", &line, "/dev/null"]);
        c
    };
    for (k, v) in &f.d.env {
        c.env(k, v);
    }
    c.env_remove("DRMANHATTAN_ACCEPT").env("DRMANHATTAN_DATA_DIR", &f.data).env("DRMANHATTAN_STATE_DIR", &f.state);
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    use std::io::Write;
    child.stdin.as_mut().unwrap().write_all(input.as_bytes()).unwrap();
    child
}

fn inbox_shown(f: &Fresh) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let inbox = f.d.inbox.to_string_lossy().into_owned();
    match inbox.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => inbox,
    }
}

#[test]
fn the_first_start_in_a_terminal_asks_once() {
    let f = Fresh::new();
    let args = ["serve", "--port", "0", "--no-dashboard"];
    // No: nothing starts and nothing is saved.
    let no = in_a_terminal(&f, &args, "n\n").wait_with_output().unwrap();
    let shown = String::from_utf8_lossy(&no.stdout);
    assert_eq!(no.status.code(), Some(0), "{shown}");
    let notice =
        format!("drmanhattan will run any command placed in {}, as you, with your permissions.", inbox_shown(&f));
    assert!(
        shown.contains(&notice) && shown.contains("Only start it if you're fine with that. Continue? [y/N]"),
        "{shown}"
    );
    assert!(!f.acknowledged() && !f.state.join("dashboard.json").exists());
    // Yes: it starts and remembers.
    let mut yes = in_a_terminal(&f, &args, "y\n");
    wait_for(T, || f.state.join("dashboard.json").exists().then_some(())).expect("the daemon starts");
    assert!(f.acknowledged());
    assert!(f.cmd(&["stop"]).output().unwrap().status.success());
    let _ = yes.wait();
    // And does not ask again.
    let again = in_a_terminal(&f, &args, "");
    wait_for(T, || f.state.join("dashboard.json").exists().then_some(())).expect("starts without asking");
    assert!(f.cmd(&["stop"]).output().unwrap().status.success());
    let out = again.wait_with_output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains("Continue?"));
}
