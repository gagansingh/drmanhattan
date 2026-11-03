//! The user's login shell environment, captured when the daemon starts and given to every
//! job, so a daemon started by launchd or systemd runs commands with the PATH and variables
//! a terminal has. Agents then need no `zsh -ic` wrapper, whose interactive rc files can
//! wait for a terminal that a job does not have. `drmanhattan reload-env` captures it again
//! for the jobs that start afterwards (`reload`).

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::time::Timestamp;

const BEGIN: &[u8] = b"__DRMANHATTAN_ENV_BEGIN__\0";
const END: &[u8] = b"__DRMANHATTAN_ENV_END__\0";

/// Variables that describe the capturing shell, not the user's setup.
const VOLATILE: &[&str] = &[
    "TERM_SESSION_ID",
    "SHLVL",
    "PWD",
    "OLDPWD",
    "_",
    "PS1",
    "PS2",
    "PS3",
    "PS4",
    "PROMPT",
    "PROMPT2",
    "PROMPT3",
    "PROMPT4",
    "RPROMPT",
    "RPROMPT2",
    "RPS1",
    "RPS2",
];

pub(crate) struct ShellEnv {
    /// Set on every job before its own `env`.
    pub(crate) vars: Vec<(String, String)>,
    /// For `drmanhattan paths`: `zsh (/bin/zsh)`, `fallback: …` or `off`.
    pub(crate) summary: String,
    /// When the shell was asked; None when the capture is off.
    pub(crate) read_at: Option<Timestamp>,
}

/// The snapshot jobs start from. A job takes an `Arc` to it when it is spawned, so a reload
/// swapping it in the meantime changes nothing for that job.
static CURRENT: Mutex<Option<Arc<ShellEnv>>> = Mutex::new(None);

/// The jobs' base environment; empty until `init`, and in commands other than the daemon.
pub(crate) fn vars() -> Arc<ShellEnv> {
    crate::sync::lock(&CURRENT).clone().unwrap_or_else(|| Arc::new(off()))
}

/// Captures the environment, or settles on the fallback, and says which in the daemon log.
pub(crate) fn init(enabled: bool, timeout: Duration) -> Arc<ShellEnv> {
    let env = Arc::new(if enabled { read(timeout) } else { off() });
    *crate::sync::lock(&CURRENT) = Some(env.clone());
    env
}

/// Captures the environment again and makes it the jobs' base from now on. A shell that
/// fails now changes nothing: the jobs keep the snapshot they had, and the error says why.
pub(crate) fn reload(timeout: Duration) -> Result<Arc<ShellEnv>, String> {
    if vars().read_at.is_none() {
        return Err("the shell environment is off (DRMANHATTAN_SHELL_ENV=0), so jobs get the daemon's own".into());
    }
    let shell = login_shell();
    let vars = capture(&shell, timeout).map_err(|why| format!("could not read {shell}'s login environment ({why})"))?;
    let env = Arc::new(ShellEnv { vars, summary: summary(&shell), read_at: Some(Timestamp::now()) });
    *crate::sync::lock(&CURRENT) = Some(env.clone());
    Ok(env)
}

/// The capture turned off: nothing is set on jobs.
fn off() -> ShellEnv {
    ShellEnv { vars: Vec::new(), summary: "off".into(), read_at: None }
}

/// `zsh (/bin/zsh)`.
fn summary(shell: &str) -> String {
    format!("{} ({shell})", short_name(shell))
}

/// The first capture: the fallback stands in when the shell does not answer, and the daemon
/// log says so.
fn read(timeout: Duration) -> ShellEnv {
    let shell = login_shell();
    let read_at = Some(Timestamp::now());
    match capture(&shell, timeout) {
        Ok(vars) => ShellEnv { vars, summary: summary(&shell), read_at },
        Err(why) => {
            crate::log::problem(format_args!(
                "could not read {shell}'s login environment ({why}); jobs get a standard PATH"
            ));
            ShellEnv { vars: fallback(), summary: format!("fallback: {why}"), read_at }
        }
    }
}

/// `$SHELL`, else zsh, else bash.
fn login_shell() -> String {
    let usable = |p: &str| !p.is_empty() && Path::new(p).is_file();
    std::env::var("SHELL")
        .ok()
        .filter(|s| usable(s))
        .unwrap_or_else(|| if usable("/bin/zsh") { "/bin/zsh" } else { "/bin/bash" }.to_string())
}

fn short_name(shell: &str) -> &str {
    shell.rsplit('/').next().unwrap_or(shell)
}

/// Runs `shell -ilc` under a terminal, its rc output going there, and reads the exported
/// variables from a pipe on fd 3, between markers: a background process the rc starts may
/// keep the pipe open, so the end marker, not EOF, ends the read.
fn capture(shell: &str, timeout: Duration) -> Result<Vec<(String, String)>, String> {
    let pty = crate::pty::open().map_err(|e| format!("no terminal: {e}"))?;
    let (read_end, write_end) = pipe().map_err(|e| format!("no pipe: {e}"))?;
    let script =
        r"printf '__DRMANHATTAN_ENV_BEGIN__\0' >&3; env -0 >&3; printf '__DRMANHATTAN_ENV_END__\0' >&3; exec 3>&-";
    let mut command = Command::new(shell);
    command.args(["-ilc", script]);
    pty.attach(&mut command).map_err(|e| e.to_string())?;
    let fd = write_end.as_raw_fd();
    // SAFETY: dup2 and fcntl are async-signal-safe; fd 3 must survive exec.
    unsafe {
        command.pre_exec(move || {
            let ok = if fd == 3 { libc::fcntl(3, libc::F_SETFD, 0) } else { libc::dup2(fd, 3) };
            if ok < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command.spawn().map_err(|e| format!("cannot start it: {e}"))?;
    drop(write_end);
    let _noise = pty.copy_to(std::io::sink());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_dump(std::fs::File::from(read_end)));
    });
    let pgid = child.id() as i32;
    let result = rx.recv_timeout(timeout);
    if result.is_err() {
        // SAFETY: the shell leads its own session and group, so this ends it and its children.
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
    }
    std::thread::spawn(move || child.wait()); // reaped whenever it exits
    match result {
        Ok(Some(dump)) => Ok(parse(&dump)),
        Ok(None) => Err("it exited without printing its environment".into()),
        Err(_) => Err(format!("no answer within {}s", timeout.as_secs())),
    }
}

/// The bytes between the markers, once the end marker arrives; None if the pipe closes first.
fn read_dump(mut pipe: std::fs::File) -> Option<Vec<u8>> {
    let mut seen = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        if find(&seen, END).is_some() {
            return between_markers(&seen).map(<[u8]>::to_vec);
        }
        match pipe.read(&mut buf) {
            Ok(n @ 1..) => seen.extend_from_slice(&buf[..n]),
            _ => return None,
        }
    }
}

fn between_markers(seen: &[u8]) -> Option<&[u8]> {
    let start = find(seen, BEGIN)? + BEGIN.len();
    let end = start + find(&seen[start..], END)?;
    Some(&seen[start..end])
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `env -0` output, minus the shell's own variables.
fn parse(dump: &[u8]) -> Vec<(String, String)> {
    dump.split(|b| *b == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok()?.split_once('='))
        .filter(|(k, _)| !k.is_empty() && !VOLATILE.contains(k))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The daemon's own environment with the usual tool directories in front of its PATH.
fn fallback() -> Vec<(String, String)> {
    let home = crate::paths::home();
    let mut dirs: Vec<String> = ["/opt/homebrew/bin", "/usr/local/bin"].map(String::from).to_vec();
    dirs.push(home.join(".local/bin").display().to_string());
    dirs.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(String::from));
    for dir in std::env::var("PATH").unwrap_or_default().split(':').filter(|d| !d.is_empty()) {
        if !dirs.iter().any(|d| d == dir) {
            dirs.push(dir.to_string());
        }
    }
    vec![("PATH".into(), dirs.join(":"))]
}

fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: pipe fills both slots with fresh descriptors on success.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: fcntl on descriptors we just opened; the child gets the write end via dup2.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: both are fresh and unowned.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dump_is_read_between_the_markers_and_shell_variables_dropped() {
        let raw = b"noise\0__DRMANHATTAN_ENV_BEGIN__\0PATH=/x:/y\0SHLVL=2\0A=b=c\0PS1=%\0__DRMANHATTAN_ENV_END__\0";
        assert_eq!(parse(between_markers(raw).unwrap()), [("PATH".into(), "/x:/y".into()), ("A".into(), "b=c".into())]);
    }

    #[test]
    fn the_fallback_puts_tool_directories_first() {
        let path = &fallback()[0].1;
        assert!(path.starts_with("/opt/homebrew/bin:/usr/local/bin:"), "{path}");
        assert!(path.contains("/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin"), "{path}");
    }
}
