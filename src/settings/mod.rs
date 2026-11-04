//! Everything that tunes the queue: environment variables (`Settings`), the persistent
//! `config.json`, the slot capacity, and the manual queue order.

mod capacity;
mod config;
mod manual_order;

use std::str::FromStr;
use std::time::Duration;

pub(crate) use capacity::{
    effective_capacity, remove_timed_overrides, set_slots, set_worker_slots, standing_capacity, MAX_SLOTS,
};
pub(crate) use config::{load_config, save_config};
pub(crate) use manual_order::{read_manual_order, write_manual_order, ManualOrder};

use crate::error::{invalid, Result};

/// Tunables read from the environment once, at start-up.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    /// The scheduler's tick; file-system events wake it sooner.
    pub(crate) poll: Duration,
    /// How often a running job's load is sampled.
    pub(crate) heartbeat: Duration,
    /// Between TERM and KILL when stopping a job.
    pub(crate) term_grace: Duration,
    /// How long a graceful shutdown waits for running jobs before cancelling them.
    pub(crate) shutdown_grace: Duration,
    /// The anti-starvation floor: no waiter is passed over for longer than this, at least.
    pub(crate) max_skip: Duration,
    /// Finished runs, and their files in the inbox, leave after this long.
    pub(crate) retention: Duration,
    /// Archived run records leave after this long.
    pub(crate) archive_retention: Duration,
    /// The ceiling of the dashboard's + button.
    /// The most a running job's log may hold, in bytes. Past it, the earlier output is
    /// dropped and the newest kept.
    pub(crate) max_log_bytes: u64,
    /// `DRMANHATTAN_SLOTS`, which overrides the standing capacity in `config.json`. The
    /// daemon publishes its own in `dashboard.json`, so that other commands report the
    /// capacity the daemon uses rather than their own environment's.
    pub(crate) slots_env: Option<String>,
    /// The most `prepare` commands run at once.
    pub(crate) max_prepares: usize,
    /// How often each worker is asked for its readings.
    pub(crate) worker_poll: Duration,
    /// The most services that run at once; they take no build slots.
    pub(crate) max_services: usize,
    /// The most quick runs at once (`DRMANHATTAN_QUICK`, the number of cores by default);
    /// they take no slots either. 0 turns the quick lane off: every job waits for a slot.
    pub(crate) max_quick: usize,
    /// Give jobs the user's login shell environment (`DRMANHATTAN_SHELL_ENV`, on unless `0`).
    pub(crate) shell_env: bool,
    /// How long that shell may take to print it.
    pub(crate) shell_env_timeout: Duration,
}

const DAY: u64 = 86_400;

impl Settings {
    pub(crate) fn from_env() -> Result<Settings> {
        Ok(Settings {
            poll: Duration::from_millis(env_or(
                "DRMANHATTAN_POLL_MS",
                200,
                |v| (10..=60_000).contains(v),
                "a whole number of milliseconds from 10 to 60000",
            )?),
            heartbeat: seconds_env("DRMANHATTAN_HEARTBEAT_SECONDS", 30.0, 0.1)?,
            term_grace: seconds_env("DRMANHATTAN_TERM_GRACE_SECONDS", 5.0, 0.1)?,
            shutdown_grace: seconds_env("DRMANHATTAN_SHUTDOWN_GRACE_SECONDS", 30.0, 0.0)?,
            max_skip: seconds_env("DRMANHATTAN_MAX_SKIP_SECONDS", 600.0, 0.0)?,
            retention: days_env("DRMANHATTAN_LOG_RETENTION_DAYS", 7)?,
            archive_retention: days_env("DRMANHATTAN_ARCHIVE_DAYS", 90)?,
            slots_env: std::env::var("DRMANHATTAN_SLOTS").ok(),
            shell_env: std::env::var("DRMANHATTAN_SHELL_ENV").map_or(true, |v| v.trim() != "0"),
            shell_env_timeout: seconds_env("DRMANHATTAN_SHELL_ENV_TIMEOUT_SECONDS", 10.0, 0.1)?,
            max_services: env_or("DRMANHATTAN_SERVICES", 4, |v| *v <= 64, "a whole number from 0 to 64")?,
            max_quick: env_or(
                "DRMANHATTAN_QUICK",
                crate::process::cpu_count(),
                |v| *v <= 256,
                "a whole number from 0 to 256",
            )?,
            worker_poll: seconds_env("DRMANHATTAN_WORKER_POLL_SECONDS", 15.0, 0.1)?,
            max_prepares: env_or(
                "DRMANHATTAN_MAX_PREPARES",
                4,
                |v| (1..=64).contains(v),
                "a whole number from 1 to 64",
            )?,
            max_log_bytes: env_or(
                "DRMANHATTAN_MAX_LOG_MB",
                1024,
                |mb| (1..=1_048_576).contains(mb),
                "a whole number of megabytes from 1 to 1048576",
            )? * 1024
                * 1024,
        })
    }
}

/// An environment variable parsed as `T`, or `default` when it is unset.
fn env_or<T: FromStr>(name: &str, default: T, valid: impl Fn(&T) -> bool, expected: &str) -> Result<T> {
    let Ok(raw) = std::env::var(name) else { return Ok(default) };
    match raw.trim().parse::<T>() {
        Ok(v) if valid(&v) => Ok(v),
        _ => invalid(format!("{name} must be {expected}, not {raw:?}")),
    }
}

/// The longest duration a setting may give: settings are added to clock readings, which
/// must not overflow.
const MAX_SETTING: Duration = Duration::from_secs(365 * DAY);

fn seconds_env(name: &str, default: f64, minimum: f64) -> Result<Duration> {
    let expected = format!("a number of seconds from {minimum} to {}", MAX_SETTING.as_secs());
    let in_range = |v: &f64| (minimum..=MAX_SETTING.as_secs_f64()).contains(v);
    Ok(Duration::from_secs_f64(env_or(name, default, in_range, &expected)?))
}

fn days_env(name: &str, default: u64) -> Result<Duration> {
    let max_days = MAX_SETTING.as_secs() / DAY;
    let days = env_or(name, default, |d| *d <= max_days, &format!("a whole number of days, at most {max_days}"))?;
    Ok(Duration::from_secs(days * DAY))
}
