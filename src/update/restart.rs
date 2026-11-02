//! Restarting the daemon on the new binary: through launchd or systemd when it runs under
//! one, else by stopping it and starting the new version itself.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::daemon;
use crate::error::{Error, Result};
use crate::paths::Paths;

pub(super) fn restart(paths: &Paths, dir: &Path) -> Result<()> {
    let Some(running) = daemon::running(paths) else {
        println!("drmanhattan is not running; the new version starts with the next `drmanhattan`");
        return Ok(());
    };
    if let Some(label) = launchd_label(running.pid) {
        let target = format!("gui/{}/{label}", crate::paths::uid());
        if Command::new("launchctl").args(["kickstart", "-k", &target]).status().is_ok_and(|s| s.success()) {
            println!("restarted launchd service {label}");
            return Ok(());
        }
    }
    if let Some(unit) = systemd_unit(running.pid) {
        if Command::new("systemctl").args(["--user", "restart", &unit]).status().is_ok_and(|s| s.success()) {
            println!("restarted systemd service {unit}");
            return Ok(());
        }
    }
    let info = running.info.clone();
    daemon::stop(paths, &crate::settings::Settings::from_env()?)?;
    // Started as it was: the same inbox and address.
    let mut serve = Command::new(dir.join("drmanhattan"));
    serve.arg("serve").arg("--yes");
    if let Some(info) = &info {
        serve.arg("--inbox").arg(&info.inbox);
        if let Some((host, port)) = info.url.as_deref().and_then(host_port) {
            serve.args(["--bind", &host, "--port", &port]);
        }
    }
    serve.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
    serve.spawn().map_err(|e| Error::Invalid(format!("could not start the new drmanhattan: {e}")))?;
    for _ in 0..50 {
        if let Some(url) = daemon::running(paths).and_then(|d| d.info).and_then(|i| i.url) {
            println!("started the new drmanhattan · dashboard {url}");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("started the new drmanhattan");
    Ok(())
}

/// `http://host:port/` as its host and port.
fn host_port(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("http://")?.trim_end_matches('/');
    let (host, port) = rest.rsplit_once(':')?;
    Some((host.trim_matches(['[', ']']).to_string(), port.to_string()))
}

/// The launchd job running `pid`, if one is.
fn launchd_label(pid: u32) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = Command::new("launchctl").arg("list").output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == pid.to_string()).then(|| words.nth(1).map(String::from)).flatten()
    })
}

/// The systemd user unit running `pid`, if one is.
fn systemd_unit(pid: u32) -> Option<String> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    cgroup.split('/').find(|part| part.ends_with(".service") && !part.starts_with("user@")).map(String::from)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_dashboard_url_gives_host_and_port() {
        assert_eq!(super::host_port("http://127.0.0.1:8791/"), Some(("127.0.0.1".into(), "8791".into())));
    }
}
