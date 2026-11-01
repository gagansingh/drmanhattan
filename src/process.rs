//! Processes and process groups: liveness, signals, ages, and the machine's load.

use std::process::Command;
use std::time::{Duration, Instant};

/// The 1, 5 and 15 minute load averages.
pub(crate) fn load_averages() -> Option<[f64; 3]> {
    let mut loads = [0f64; 3];
    // SAFETY: the pointer refers to a live array of 3 f64s and we ask for at most 3.
    let n = unsafe { libc::getloadavg(loads.as_mut_ptr(), 3) };
    (n == 3).then_some(loads)
}

pub(crate) fn load_now() -> Option<f64> {
    load_averages().map(|l| l[0])
}

pub(crate) fn cpu_count() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// Whether `kill(target, 0)` finds something: a process (`target > 0`) or a process group
/// (`target < 0`). EPERM means it exists but belongs to someone else.
fn exists(target: i32) -> bool {
    // SAFETY: signal 0 checks for existence and sends nothing.
    let found = unsafe { libc::kill(target, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub(crate) fn process_alive(pid: i32) -> bool {
    pid > 0 && exists(pid)
}

pub(crate) fn group_alive(pgid: i32) -> bool {
    pgid > 0 && exists(-pgid)
}

pub(crate) fn signal_process(pid: i32, signal: i32) -> bool {
    // SAFETY: kill has no memory-safety preconditions.
    pid > 0 && unsafe { libc::kill(pid, signal) } == 0
}

pub(crate) fn signal_group(pgid: i32, signal: i32) {
    if pgid > 0 {
        // SAFETY: as above; a negative pid addresses the whole group.
        unsafe { libc::kill(-pgid, signal) };
    }
}

/// Parses ps's `etime` column, `[[dd-]hh:]mm:ss`.
pub(crate) fn parse_etime(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (days, rest) = match text.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, text),
    };
    let mut parts: Vec<u64> = rest.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    while parts.len() < 3 {
        parts.insert(0, 0);
    }
    Some(Duration::from_secs(days * 86400 + parts[0] * 3600 + parts[1] * 60 + parts[2]))
}

/// How long ago a process started.
pub(crate) fn process_age(pid: i32) -> Option<Duration> {
    let out = Command::new("ps").args(["-o", "etime=", "-p", &pid.to_string()]).output().ok()?;
    parse_etime(&String::from_utf8_lossy(&out.stdout))
}

/// The ages of the processes in a process group, from `ps` (which lists process groups
/// the same way on macOS and Linux).
pub(crate) fn group_member_ages(pgid: i32) -> Vec<Duration> {
    let Ok(out) = Command::new("ps").args(["-A", "-o", "pgid=,etime="]).output() else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let members = text.lines().filter_map(|l| l.trim().split_once(char::is_whitespace));
    members.filter(|(g, _)| g.trim().parse() == Ok(pgid)).filter_map(|(_, etime)| parse_etime(etime)).collect()
}

/// Ends a process group: TERM, then KILL if it is still there after `grace`, then waits
/// up to two more seconds for it to go.
pub(crate) fn terminate_group(pgid: i32, grace: Duration) {
    if !group_alive(pgid) {
        return;
    }
    signal_group(pgid, libc::SIGTERM);
    if !wait_for_group_exit(pgid, grace) {
        signal_group(pgid, libc::SIGKILL);
        wait_for_group_exit(pgid, Duration::from_secs(2));
    }
}

/// Polls until the group is gone; false if it outlived `limit`.
fn wait_for_group_exit(pgid: i32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while group_alive(pgid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etime_parses_every_ps_form() {
        let secs = |t: &str| parse_etime(t).map(|d| d.as_secs());
        assert_eq!(secs("00:05"), Some(5));
        assert_eq!(secs(" 12:34 "), Some(754));
        assert_eq!(secs("01:00:00"), Some(3600));
        assert_eq!(secs("2-03:04:05"), Some(2 * 86400 + 3 * 3600 + 4 * 60 + 5));
        assert_eq!(secs(""), None);
        assert_eq!(secs("x:1"), None);
        assert_eq!(secs("1:2:3:4"), None);
    }
}
