//! When a service is ready: its port accepts connections, or a line of its log matches.

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use regex::Regex;

use crate::activity::OWN_LINE_PREFIX;
use crate::job::Ready;
use crate::joblog;

const CONNECT_LIMIT: Duration = Duration::from_millis(200);
const MAX_PARTIAL: usize = 4096;

pub(super) struct Readiness {
    port: Option<u16>,
    log: Option<Regex>,
    offset: u64,
    partial: String,
}

impl Readiness {
    /// `from`: where the service's own output starts in its log.
    pub(super) fn new(ready: Option<&Ready>, from: u64) -> Readiness {
        let (port, log) = match ready {
            Some(Ready::Port(p)) => (Some(*p), None),
            // Checked when the job was queued.
            Some(Ready::Log(re)) => (None, Regex::new(re).ok()),
            None => (None, None),
        };
        Readiness { port, log, offset: from, partial: String::new() }
    }

    /// Whether it is ready now. A service with no check is ready as soon as it starts.
    pub(super) fn check(&mut self, log: &Path) -> bool {
        if let Some(port) = self.port {
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            return TcpStream::connect_timeout(&addr, CONNECT_LIMIT).is_ok();
        }
        let Some(re) = &self.log else { return true };
        while let Some(gained) = joblog::read_from(log, self.offset, 1024 * 1024) {
            if gained.bytes.is_empty() {
                break;
            }
            self.offset = gained.offset;
            let text = format!("{}{}", self.partial, String::from_utf8_lossy(&gained.bytes));
            let mut lines: Vec<&str> = text.split(['\n', '\r']).collect();
            let rest = lines.pop().unwrap_or_default();
            if lines.iter().any(|l| !l.starts_with(OWN_LINE_PREFIX) && re.is_match(l)) {
                return true;
            }
            self.partial = rest.chars().rev().take(MAX_PARTIAL).collect::<Vec<_>>().into_iter().rev().collect();
        }
        // A last line with no newline yet counts too: servers often print it and wait.
        !self.partial.is_empty() && re.is_match(&self.partial)
    }
}
