//! drmanhattan: a local job queue for heavy commands. A watched folder takes JSON job files,
//! slots with fair admission run them one (or a few) at a time, and a dashboard shows the
//! queue. See the README for the job-file contract.
//!
//! How the pieces fit:
//!
//! - `cli` parses the command line; `daemon` is the long-running process, which
//!   `acknowledge` has the person confirm once before its first start.
//! - `queue` holds every run and decides what runs next; `runner` runs one job.
//! - `job` is the job file format and a run's record; `history` and `estimate` turn past
//!   runs into expected durations; `activity` follows a running job's log.
//! - `dashboard` serves the web page; `sources` links a run back to the tool that sent it;
//!   `sysinfo` reads the machine's CPU, memory and temperature; `workers` hands jobs to other
//!   machines over SSH.
//! - `settings`, `paths`, `fsutil`, `process`, `time`, `naming`, `error`, `log`, `signals` and
//!   `sync`
//!   are the plumbing.

mod acknowledge;
mod activity;
mod cli;
mod daemon;
mod dashboard;
mod error;
mod estimate;
mod fsutil;
mod history;
mod job;
mod joblog;
mod log;
mod naming;
mod paths;
mod process;
mod queue;
mod runner;
mod settings;
mod signals;
mod sources;
mod sync;
mod sysinfo;
mod time;
mod workers;

pub use cli::run_cli;
