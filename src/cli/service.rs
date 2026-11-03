//! `submit --service`: waiting for a service to be ready, and saying where it is.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::error::Result;
use crate::fsutil;
use crate::job::{JobFiles, Ready};
use crate::log;

const POLL: Duration = Duration::from_millis(200);

/// The parts of a service's `.status.json` that `submit --service` reports.
#[derive(Deserialize)]
struct ServiceStatus {
    state: String,
    ready_at: Option<String>,
    process_group: Option<i32>,
    ready: Option<Ready>,
    failure_reason: Option<String>,
    log: Option<PathBuf>,
}

/// Waits for a service to be ready, then says where it is and returns 0; if it ends first,
/// shows the end of its log and returns its exit code. `existing`: it was already running.
pub(super) fn wait_ready(files: &JobFiles, existing: bool) -> Result<i32> {
    loop {
        let status: Option<ServiceStatus> = fsutil::read_json(&files.status());
        if let Some(s) = status.as_ref().filter(|s| s.ready_at.is_some() && s.state == "running") {
            let port = match &s.ready {
                Some(Ready::Port(p)) => format!(" · port {p}"),
                _ => String::new(),
            };
            let pid = s.process_group.map(|p| format!(" · pid {p}")).unwrap_or_default();
            let log = s.log.as_ref().map_or_else(|| files.log(), Clone::clone);
            let how = if existing { "already running" } else { "ready" };
            let line = format!(
                "{}{} {how}{pid}{port} · log {} · stop it with `manhattan stop {}`",
                log::NOTE_PREFIX,
                files.name(),
                log.display(),
                files.name()
            );
            println!("{}", log::mark_own_line(&line, log::stdout_colour()));
            return Ok(0);
        }
        if files.exit().exists() && !files.job().exists() {
            let code = std::fs::read_to_string(files.exit()).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(1);
            if let Some(text) = fsutil::read_tail(&files.log(), 16 * 1024) {
                let lines: Vec<&str> = text.lines().collect();
                for line in &lines[lines.len().saturating_sub(20)..] {
                    eprintln!("{line}");
                }
            }
            let reason = status.and_then(|s| s.failure_reason).unwrap_or_else(|| "it stopped".into());
            log::error(format_args!("{} did not become ready: {reason}", files.name()));
            return Ok(if code == 0 { 1 } else { code });
        }
        std::thread::sleep(POLL);
    }
}
