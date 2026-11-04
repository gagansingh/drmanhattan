//! Where everything lives.
//!
//! ```text
//! State (volatile, 0700, per user; /tmp/drmanhattan-<uid> by default):
//!     runs/<run id>.json        one record per run, rewritten at each state change
//!     daemon.lock               flock held while a daemon runs; holds its pid
//!     dashboard.json            the running daemon's pid, inbox, dashboard URL and settings
//!     heartbeat                 rewritten every few seconds while the daemon runs
//!     manual-order              run ids, one per line, first goes first (drag to reorder)
//!     workers/<name>.json       what this daemon last saw of each worker
//!
//! Data (durable; ~/Library/Application Support/drmanhattan on macOS):
//!     history.jsonl             one line per finished run: the timings estimates learn from
//!     steps.jsonl               one line per finished ::start/::done command
//!     archive/<YYYY-MM>/<run id>.json   finished runs' final records
//!     config.json               the standing slot count and default activity patterns
//!     acknowledged.json         when the first start was confirmed (see `acknowledge`)
//!     drmanhattan.log           the daemon's own output, rotated at 5 MB (one older copy kept)
//!     workers/<name>.synced     paths mirrored to a worker, for its housekeeping
//!     mirrors.json              on a worker: the mirrors dispatchers sent it
//!
//! Inbox (~/.drmanhattan/inbox by default): job files and the results written beside them.
//! ```

use std::path::PathBuf;

pub(crate) fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

pub(crate) fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub(crate) fn default_inbox() -> PathBuf {
    env_path("DRMANHATTAN_INBOX").unwrap_or_else(|| home().join(".drmanhattan/inbox"))
}

#[derive(Clone, Debug)]
pub(crate) struct Paths {
    pub(crate) state: PathBuf,
    pub(crate) data: PathBuf,
}

impl Paths {
    /// The defaults, or `DRMANHATTAN_STATE_DIR` and `DRMANHATTAN_DATA_DIR`.
    pub(crate) fn from_env() -> Paths {
        let state = env_path("DRMANHATTAN_STATE_DIR").unwrap_or_else(|| {
            let runtime = env_path("XDG_RUNTIME_DIR").filter(|_| !cfg!(target_os = "macos"));
            runtime.map_or_else(|| PathBuf::from(format!("/tmp/drmanhattan-{}", uid())), |rt| rt.join("drmanhattan"))
        });
        let data = env_path("DRMANHATTAN_DATA_DIR").unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                home().join("Library/Application Support/drmanhattan")
            } else {
                env_path("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local/share")).join("drmanhattan")
            }
        });
        Paths { state, data }
    }

    pub(crate) fn runs(&self) -> PathBuf {
        self.state.join("runs")
    }
    pub(crate) fn run_record(&self, run_id: &str) -> PathBuf {
        self.runs().join(format!("{run_id}.json"))
    }
    pub(crate) fn daemon_lock(&self) -> PathBuf {
        self.state.join("daemon.lock")
    }
    pub(crate) fn heartbeat(&self) -> PathBuf {
        self.state.join("heartbeat")
    }
    pub(crate) fn daemon_info(&self) -> PathBuf {
        self.state.join("dashboard.json")
    }
    pub(crate) fn manual_order(&self) -> PathBuf {
        self.state.join("manual-order")
    }
    pub(crate) fn history(&self) -> PathBuf {
        self.data.join("history.jsonl")
    }
    pub(crate) fn steps(&self) -> PathBuf {
        self.data.join("steps.jsonl")
    }
    pub(crate) fn archive(&self) -> PathBuf {
        self.data.join("archive")
    }
    pub(crate) fn daemon_log(&self) -> PathBuf {
        self.data.join("drmanhattan.log")
    }
    pub(crate) fn acknowledgement(&self) -> PathBuf {
        self.data.join("acknowledged.json")
    }
    pub(crate) fn worker_states(&self) -> PathBuf {
        self.state.join("workers")
    }
    pub(crate) fn worker_state(&self, worker: &str) -> PathBuf {
        self.worker_states().join(format!("{worker}.json"))
    }
    pub(crate) fn worker_synced(&self, worker: &str) -> PathBuf {
        self.data.join("workers").join(format!("{worker}.synced"))
    }
    pub(crate) fn mirrors(&self) -> PathBuf {
        self.data.join("mirrors.json")
    }
    pub(crate) fn config(&self) -> PathBuf {
        self.data.join("config.json")
    }
}
