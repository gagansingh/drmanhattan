//! `config.json` in the data directory: the standing slot count (`drmanhattan slots N`) and
//! the daemon-wide activity patterns. Unknown keys are kept when the file is rewritten.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::activity::PatternConfig;
use crate::error::{invalid, IoContext, Result};
use crate::fsutil;
use crate::paths::Paths;

#[derive(Serialize, Deserialize, Default, Debug)]
pub(crate) struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slots: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) activity: Option<PatternConfig>,
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
    serde_json::from_str(&text).or_else(|e| invalid(format!("{} is not valid: {e}", path.display())))
}

pub(crate) fn save_config(paths: &Paths, config: &Config) -> Result<()> {
    fsutil::ensure_private_dir(&paths.data)?;
    fsutil::write_json(&paths.config(), config)
}
