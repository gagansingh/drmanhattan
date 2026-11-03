//! Test harness: a real daemon on a private temp queue, driven through job files and HTTP.
#![allow(dead_code)]

pub mod releases;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// One line of `history.jsonl`: a successful run that finished just now.
pub fn history_row(category: &str, label: &str, seconds: u64) -> Value {
    serde_json::json!({
        "finished_at": chrono::Utc::now().to_rfc3339(), "category": category, "label": label,
        "seconds": seconds, "exit_status": 0, "outcome": "succeeded",
    })
}

/// A timestamp field of a record or status file (RFC 3339), as Unix seconds.
pub fn epoch(v: &Value) -> f64 {
    let text = v.as_str().unwrap_or_else(|| panic!("a timestamp, not {v}"));
    chrono::DateTime::parse_from_rfc3339(text).unwrap().timestamp_millis() as f64 / 1000.0
}

pub fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_drmanhattan"))
}

pub struct Daemon {
    pub root: PathBuf,
    pub inbox: PathBuf,
    pub state: PathBuf,
    pub data: PathBuf,
    pub work: PathBuf,
    pub env: Vec<(String, String)>,
    pub child: Option<Child>,
    pub url: String,
    /// The drmanhattan this queue runs: the built one, or a copy a test installed.
    pub program: PathBuf,
}

impl Daemon {
    /// A fresh queue in a private temp directory, with fast timers and the given extra env.
    pub fn start(extra: &[(&str, &str)]) -> Daemon {
        Daemon::start_with(extra, |_| bin())
    }

    /// As `start`, running the drmanhattan `program` gives for the queue's root directory.
    pub fn start_with(extra: &[(&str, &str)], program: impl FnOnce(&Path) -> PathBuf) -> Daemon {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("drmanhattan-test-{}-{}", std::process::id(), n));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap_or(root); // macOS puts TMPDIR behind a symlink
        let inbox = root.join("inbox");
        let state = root.join("state");
        let data = root.join("data");
        let work = root.join("work");
        fs::create_dir_all(&inbox).unwrap();
        fs::create_dir_all(&work).unwrap();
        let mut env: Vec<(String, String)> = vec![
            ("DRMANHATTAN_STATE_DIR", state.to_str().unwrap()),
            ("DRMANHATTAN_DATA_DIR", data.to_str().unwrap()),
            ("DRMANHATTAN_ACCEPT", "1"),
            ("DRMANHATTAN_INBOX", inbox.to_str().unwrap()),
            ("DRMANHATTAN_HEARTBEAT_SECONDS", "0.5"),
            ("DRMANHATTAN_TERM_GRACE_SECONDS", "1"),
            ("DRMANHATTAN_SHUTDOWN_GRACE_SECONDS", "2"),
            ("DRMANHATTAN_POLL_MS", "50"),
            ("DRMANHATTAN_NO_UPDATE_CHECK", "1"),
            // Not the developer's own shell: the shell environment tests bring a stand-in.
            ("DRMANHATTAN_SHELL_ENV", "0"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for (k, v) in extra {
            env.push((k.to_string(), v.to_string()));
        }
        let program = program(&root);
        let mut d = Daemon { root, inbox, state, data, work, env, child: None, url: String::new(), program };
        d.launch();
        d
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.program);
        c.args(args);
        for (k, v) in &self.env {
            c.env(k, v);
        }
        // The developer's own shell may carry tool markers; detection tests set their own.
        // The tests' daemons see only the settings the tests give them: nothing from the
        // environment `cargo test` was started in, which may be a real queue's.
        for (k, _) in std::env::vars() {
            let ours = self.env.iter().any(|(set, _)| *set == k);
            if (k.starts_with("DRMANHATTAN_") && !ours)
                || k == "CLAUDE_CODE_SESSION_ID"
                || k == "TERM_PROGRAM"
                || k.starts_with("CODEX_")
                || k.starts_with("CURSOR_")
                || k.starts_with("COPILOT_")
                || k.starts_with("GITHUB_COPILOT_")
            {
                c.env_remove(k);
            }
        }
        c
    }

    pub fn launch(&mut self) {
        let log =
            fs::File::create(self.root.join(format!("daemon-{}.log", self.root.read_dir().unwrap().count()))).unwrap();
        let child = self
            .cmd(&["serve", "--port", "0", "--no-session-links"])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("daemon starts");
        let pid = child.id() as i64;
        self.child = Some(child);
        // Wait for *this* daemon's dashboard.json: a daemon killed with SIGKILL leaves its
        // old file behind, which must not be mistaken for the new one being ready.
        let info_path = self.state.join("dashboard.json");
        let info = wait_for(Duration::from_secs(15), || {
            let text = fs::read_to_string(&info_path).ok()?;
            let v = serde_json::from_str::<Value>(&text).ok()?;
            (v["pid"].as_i64() == Some(pid)).then_some(v)
        })
        .unwrap_or_else(|| panic!("daemon publishes dashboard.json\n{}", self.daemon_log()));
        self.url = info["url"].as_str().unwrap().trim_end_matches('/').to_string();
    }

    pub fn daemon_pid(&self) -> i32 {
        self.child.as_ref().unwrap().id() as i32
    }

    pub fn kill_daemon(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    pub fn signal_daemon(&self, sig: i32) {
        if let Some(c) = &self.child {
            // SAFETY: kill has no memory-safety preconditions.
            unsafe {
                libc::kill(c.id() as i32, sig);
            }
        }
    }

    pub fn wait_daemon_exit(&mut self, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(c) = self.child.as_mut() {
                if let Ok(Some(status)) = c.try_wait() {
                    self.child = None;
                    return status.code();
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    pub fn stop_daemon(&mut self) -> i32 {
        if let Some(mut c) = self.child.take() {
            // SAFETY: as above.
            unsafe {
                libc::kill(c.id() as i32, libc::SIGTERM);
            }
            return c.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
        }
        -1
    }

    /// Waits for the daemon's own output to say `text`: it writes its start-up summary just
    /// after it publishes dashboard.json, which is what `launch` waits for.
    pub fn wait_daemon_log(&self, text: &str) {
        wait_for(Duration::from_secs(15), || self.daemon_log().contains(text).then_some(()))
            .unwrap_or_else(|| panic!("the daemon never said {text:?}\n{}", self.daemon_log()));
    }

    pub fn daemon_log(&self) -> String {
        let mut out = String::new();
        for e in fs::read_dir(&self.root).unwrap().flatten() {
            if e.file_name().to_string_lossy().starts_with("daemon-") {
                out += &fs::read_to_string(e.path()).unwrap_or_default();
            }
        }
        out
    }

    /// Writes `<name>.json` atomically; returns the result base path.
    pub fn submit(&self, name: &str, job: Value) -> PathBuf {
        let path = self.inbox.join(format!("{name}.json"));
        let tmp = self.inbox.join(format!(".{name}.tmp"));
        fs::write(&tmp, serde_json::to_string(&job).unwrap()).unwrap();
        fs::rename(&tmp, &path).unwrap();
        self.inbox.join(name)
    }

    pub fn submit_text(&self, name: &str, text: &str) -> PathBuf {
        let path = self.inbox.join(format!("{name}.json"));
        let tmp = self.inbox.join(format!(".{name}.tmp"));
        fs::write(&tmp, text).unwrap();
        fs::rename(&tmp, &path).unwrap();
        self.inbox.join(name)
    }

    pub fn job(&self, command: &str) -> Value {
        serde_json::json!({"command": command, "cwd": self.work})
    }

    pub fn status_of(&self, name: &str) -> Option<Value> {
        let text = fs::read_to_string(self.inbox.join(format!("{name}.status.json"))).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn wait_state(&self, name: &str, state: &str, timeout: Duration) -> Value {
        wait_for(timeout, || {
            let s = self.status_of(name)?;
            (s["state"] == state).then_some(s)
        })
        .unwrap_or_else(|| {
            panic!("{name} did not reach {state}; last status: {:?}\n{}", self.status_of(name), self.daemon_log())
        })
    }

    pub fn wait_exit(&self, name: &str, timeout: Duration) -> i32 {
        let path = self.inbox.join(format!("{name}.exit"));
        wait_for(timeout, || fs::read_to_string(&path).ok()?.trim().parse().ok())
            .unwrap_or_else(|| panic!("{name}.exit did not appear\n{}", self.daemon_log()))
    }

    pub fn log_of(&self, name: &str) -> String {
        fs::read_to_string(self.inbox.join(format!("{name}.log"))).unwrap_or_default()
    }

    pub fn status_json(&self) -> Value {
        let out = self.cmd(&["status", "--json"]).output().unwrap();
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("status --json: {}", String::from_utf8_lossy(&out.stderr)))
    }

    pub fn waiters(&self) -> Vec<String> {
        self.status_json()["waiters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["label"].as_str().unwrap().to_string())
            .collect()
    }

    /// Writes history.jsonl and restarts the daemon, which reads history once at start-up.
    pub fn seed_history(&mut self, rows: &[(&str, &str, u64)]) {
        fs::create_dir_all(&self.data).unwrap();
        let mut text = String::new();
        for (category, label, seconds) in rows {
            text += &format!("{}\n", history_row(category, label, *seconds));
        }
        fs::write(self.data.join("history.jsonl"), text).unwrap();
        self.stop_daemon();
        self.launch();
    }

    pub fn write_override(&self, slots: Option<u32>) {
        let path = self.state.join("capacity-override");
        match slots {
            None => {
                let _ = fs::remove_file(path);
            }
            Some(n) => {
                let until = chrono::Utc::now() + chrono::Duration::hours(1);
                fs::write(path, format!("{n} {}\n", until.to_rfc3339())).unwrap();
            }
        }
    }

    pub fn write_manual_order(&self, run_ids: &[&str]) {
        fs::write(self.state.join("manual-order"), run_ids.iter().map(|r| format!("{r}\n")).collect::<String>())
            .unwrap();
    }

    pub fn run_id_of(&self, name: &str) -> String {
        self.status_of(name).unwrap()["run_id"].as_str().unwrap().to_string()
    }

    pub fn http(&self, method: &str, path: &str, body: Option<&str>, headers: &[(&str, &str)]) -> (u16, String) {
        let (code, _, body) = self.exchange(method, path, body, headers);
        (code, body)
    }

    /// One HTTP request: the status code, the response head, and the body.
    pub fn exchange(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        headers: &[(&str, &str)],
    ) -> (u16, String, String) {
        let addr = self.url.trim_start_matches("http://");
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        // HTTP/1.0, so the server answers with a plain body rather than chunked encoding.
        let mut req = format!("{method} {path} HTTP/1.0\r\n");
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            req += &format!("Host: {addr}\r\n");
        }
        for (k, v) in headers {
            req += &format!("{k}: {v}\r\n");
        }
        req += "Connection: close\r\n";
        if let Some(b) = body {
            req += &format!("Content-Length: {}\r\n", b.len());
        }
        req += "\r\n";
        if let Some(b) = body {
            req += b;
        }
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let code: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (code, head.to_string(), body.to_string())
    }

    pub fn get(&self, path: &str) -> (u16, String) {
        self.http("GET", path, None, &[])
    }

    pub fn api_state(&self) -> Value {
        let (code, body) = self.get("/api/state");
        assert_eq!(code, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    pub fn post(&self, path: &str, body: &str, headers: &[(&str, &str)]) -> (u16, String) {
        self.http("POST", path, Some(body), headers)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.kill_daemon();
        // Stop anything a test left running in the queue's process groups.
        for e in fs::read_dir(self.state.join("runs")).into_iter().flatten().flatten() {
            if let Ok(text) = fs::read_to_string(e.path()) {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    if let Some(pg) = v["process_group"].as_i64() {
                        // SAFETY: killpg has no memory-safety preconditions.
                        unsafe {
                            libc::killpg(pg as i32, libc::SIGKILL);
                        }
                    }
                }
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn wait_for<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = probe() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Kills, with SIGKILL, the guard the daemon started beside the job in process group `pgid`
/// (see `runner::guard`), so that a test can see what happens when it is gone too.
pub fn kill_guard(pgid: i32) {
    let out = Command::new("ps").args(["-A", "-o", "pid=,command="]).output().unwrap();
    let marker = format!("__guard {pgid} ");
    let text = String::from_utf8_lossy(&out.stdout);
    let pid = text.lines().find(|l| l.contains(&marker)).and_then(|l| l.split_whitespace().next()?.parse::<i32>().ok());
    let pid = pid.unwrap_or_else(|| panic!("no guard for process group {pgid}"));
    // SAFETY: kill has no memory-safety preconditions.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    wait_for(Duration::from_secs(5), || {
        // Gone, or a zombie waiting for the daemon to reap it.
        let out = Command::new("ps").args(["-p", &pid.to_string(), "-o", "stat="]).output().ok()?;
        let stat = String::from_utf8_lossy(&out.stdout);
        (stat.trim().is_empty() || stat.trim_start().starts_with('Z')).then_some(())
    })
    .expect("the guard is gone");
}

pub fn group_exists(pgid: i32) -> bool {
    // SAFETY: signal 0 only checks that the group exists.
    unsafe { libc::killpg(pgid, 0) == 0 }
}

pub fn exists(p: &Path) -> bool {
    p.exists()
}

pub const WRITE_HEADERS: &[(&str, &str)] = &[("Content-Type", "application/json"), ("X-Drmanhattan", "1")];

/// A stand-in for `ssh`: runs the command on this machine as the worker `worker` (with its
/// state and data directories), and puts what `rsync` sends under `<dir>/root`, so a test
/// sees what reached the "worker" apart from the original. `<dir>/down-<alias>` makes the
/// alias unreachable; every command is appended to `<dir>/calls-<alias>`.
pub fn fake_ssh(dir: &Path, worker: &Daemon) -> PathBuf {
    fs::create_dir_all(dir.join("root")).unwrap();
    let script = dir.join("ssh");
    let text = format!(
        r#"#!/bin/bash
while [ $# -gt 0 ]; do
  case "$1" in -o|-p|-l|-i|-F|-e) shift 2 ;; -*) shift ;; *) break ;; esac
done
alias="$1"; shift
if [ -e "{dir}/down-$alias" ]; then echo "ssh: connect to host $alias port 22: Connection refused" >&2; exit 255; fi
printf '%s\n' "$*" >> "{dir}/calls-$alias"
export DRMANHATTAN_STATE_DIR="{state}" DRMANHATTAN_DATA_DIR="{data}"
unset DRMANHATTAN_INBOX DRMANHATTAN_SLOTS
if [ "$1" = rsync ]; then
  dest="${{@: -1}}"; dest="{dir}/root$dest"
  mkdir -p "$(dirname "$dest")"
  set -- "${{@:1:$#-1}}" "$dest"
  exec "$@"
fi
exec /bin/bash -c "$*"
"#,
        dir = dir.display(),
        state = worker.state.display(),
        data = worker.data.display(),
    );
    fs::write(&script, text).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// Writes the data directory's `config.json`.
pub fn write_config(d: &Daemon, config: Value) {
    fs::create_dir_all(&d.data).unwrap();
    fs::write(d.data.join("config.json"), serde_json::to_string_pretty(&config).unwrap()).unwrap();
}
