//! `config.json` in the data directory: the standing slot count (`drmanhattan slots N`), the
//! daemon-wide activity patterns, workers and the route to them, and how updates are installed. Unknown keys are
//! kept when the file is rewritten.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::activity::PatternConfig;
use crate::error::{invalid, IoContext, Result};
use crate::fsutil;
use crate::paths::Paths;
use crate::update::SignConfig;
use crate::workers::WorkerConfig;

#[derive(Serialize, Deserialize, Default, Debug)]
pub(crate) struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slots: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) activity: Option<PatternConfig>,
    /// Other machines that run jobs for this one, over SSH.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) workers: Vec<WorkerConfig>,
    /// Workers that every job which may run on a worker, or does not say, runs on and only
    /// on (`drmanhattan route`); empty: jobs run where they say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) route: Vec<String>,
    /// Takes no jobs from other machines, as their worker, until resumed (`drmanhattan pause`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) paused: bool,
    /// Install newer releases of this major version by itself, once the queue is idle.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) auto_update: bool,
    /// Sign each update with this identity before installing it (macOS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sign: Option<SignConfig>,
    #[serde(flatten)]
    pub(crate) other: BTreeMap<String, serde_json::Value>,
}

/// The config, or the defaults when there is no file. A file that exists but is invalid is
/// an error naming it, so a typo is never silently ignored.
pub(crate) fn load_config(paths: &Paths) -> Result<Config> {
    let path = paths.config();
    if !path.exists() {
        return Ok(Config::default());
    }
    if !fsutil::is_own_file(&path) {
        return invalid(format!("{} is not a regular file of yours", path.display()));
    }
    let text = std::fs::read_to_string(&path).context("read", &path)?;
    let mut config: Config =
        serde_json::from_str(&text).or_else(|e| invalid(format!("{} is not valid: {e}", path.display())))?;
    crate::workers::validate(&mut config.workers)
        .and_then(|()| crate::workers::validate_route(&config.route, &config.workers))
        .or_else(|e| invalid(format!("{} is not valid: {e}", path.display())))?;
    Ok(config)
}

/// Changes `config.json` with `change`, holding a lock on `config.json.lock` from reading it
/// to writing it back, so two writers (the dashboard's threads, `slots N`, `workers add`)
/// never lose each other's change. Nothing is written when `change` fails.
pub(crate) fn update_config<T>(paths: &Paths, change: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    fsutil::ensure_private_dir(&paths.data)?;
    let lock_path = paths.data.join("config.json.lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock_path)
        .context("open the config lock", &lock_path)?;
    // SAFETY: the descriptor belongs to `lock`, which outlives the call; the kernel releases
    // the lock when the file is closed at the end of this function.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error()).context("lock", &lock_path);
    }
    let mut config = load_config(paths)?;
    let out = change(&mut config)?;
    fsutil::write_json(&paths.config(), &config)?;
    Ok(out)
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn concurrent_changes_are_all_kept() {
        let root = std::env::temp_dir().join(format!("drmanhattan-config-{}", std::process::id()));
        let paths = Paths { state: root.join("state"), data: root.join("data") };
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let paths = paths.clone();
                std::thread::spawn(move || {
                    update_config(&paths, |c| {
                        c.other.insert(format!("k{i}"), serde_json::json!(i));
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        Ok(())
                    })
                    .unwrap()
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        let config = load_config(&paths).unwrap();
        assert_eq!(config.other.len(), 8, "{:?}", config.other.keys());
        let _ = std::fs::remove_dir_all(root);
    }
}
