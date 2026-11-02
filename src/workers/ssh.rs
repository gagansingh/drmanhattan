//! Reaching a worker: `ssh` with key-based auth only and one shared connection per worker,
//! and `rsync` over it.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::naming::shell_words;
use crate::paths;
use crate::workers::WorkerConfig;

/// What a command run on a worker printed, and its exit status (255: ssh itself failed).
pub(crate) struct Output {
    pub(crate) code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl Output {
    /// The first line of what it said, for a reason line.
    pub(crate) fn first_line(&self) -> String {
        let text = if self.stderr.trim().is_empty() { &self.stdout } else { &self.stderr };
        let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no output");
        line.chars().take(200).collect()
    }
}

fn program(env: &str, default: &str) -> String {
    std::env::var(env).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

pub(crate) fn ssh_program() -> String {
    program("DRMANHATTAN_SSH", "ssh")
}

/// Homebrew's rsync when there is one: macOS's own may be an older one.
pub(crate) fn rsync_program() -> String {
    let found =
        ["/opt/homebrew/bin/rsync", "/usr/local/bin/rsync"].into_iter().find(|p| std::path::Path::new(p).exists());
    program("DRMANHATTAN_RSYNC", found.unwrap_or("rsync"))
}

/// Never a password prompt, a five-second connect timeout, a dead connection noticed within
/// half a minute, and one connection per worker shared by every command.
pub(crate) fn options() -> Vec<String> {
    let control: PathBuf = paths::home().join(".ssh/dmh-%C");
    let _ = crate::fsutil::ensure_private_dir(&paths::home().join(".ssh"));
    [
        "BatchMode=yes".to_string(),
        "ConnectTimeout=5".into(),
        "ServerAliveInterval=10".into(),
        "ServerAliveCountMax=3".into(),
        "ControlMaster=auto".into(),
        format!("ControlPath={}", control.display()),
        "ControlPersist=60".into(),
    ]
    .into_iter()
    .flat_map(|o| ["-o".to_string(), o])
    .collect()
}

/// `ssh <worker> <argv>`, each word quoted for the worker's shell.
pub(crate) fn command(worker: &WorkerConfig, argv: &[String]) -> Command {
    let mut c = Command::new(ssh_program());
    c.args(options()).arg(&worker.ssh).arg(shell_words(argv));
    c
}

/// The worker's `drmanhattan` with `args`.
pub(crate) fn drmanhattan(worker: &WorkerConfig, args: &[&str]) -> Vec<String> {
    std::iter::once(worker.program()).chain(args.iter().copied()).map(String::from).collect()
}

/// Runs `argv` on the worker, with `input` on its stdin, for at most `limit`.
pub(crate) fn run(worker: &WorkerConfig, argv: &[String], input: Option<&str>, limit: Duration) -> Output {
    run_command(command(worker, argv), input, limit)
}

/// Runs a prepared command (ssh, or rsync over it) for at most `limit`; past it the
/// command is killed and the result is 255, as for a failed connection.
pub(crate) fn run_command(mut c: Command, input: Option<&str>, limit: Duration) -> Output {
    let failed = |e: String| Output { code: 255, stdout: String::new(), stderr: e };
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match c.spawn() {
        Ok(child) => child,
        Err(e) => return failed(format!("could not run {:?}: {e}", c.get_program())),
    };
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        let _ = stdin.write_all(text.as_bytes());
    }
    // Read both pipes on threads, so a chatty command cannot block on a full pipe.
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_string(&mut s);
        }
        s
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (stdout, mut stderr) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    let code = match status {
        Some(s) => s.code().unwrap_or(255),
        None => {
            stderr = format!("no answer within {}s", limit.as_secs());
            255
        }
    };
    Output { code, stdout, stderr }
}
