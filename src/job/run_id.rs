//! Run ids: `<epoch seconds>-<daemon pid>-<6 hex digits>`, sortable by time and unique
//! across daemons. Only well-formed ids are ever accepted from files or the dashboard.

use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct RunId(String);

static COUNTER: AtomicU64 = AtomicU64::new(0);

impl RunId {
    pub(crate) fn new() -> RunId {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = u64::from(std::process::id());
        let mix = (now.as_nanos() as u64) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (pid << 32);
        RunId(format!("{}-{pid}-{:06x}", now.as_secs(), mix & 0xff_ffff))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    fn is_valid(text: &str) -> bool {
        let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
        let mut parts = text.split('-');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(a), Some(b), Some(c), None) => {
                digits(a) && digits(b) && !c.is_empty() && c.bytes().all(|x| x.is_ascii_hexdigit())
            }
            _ => false,
        }
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RunId {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if RunId::is_valid(text) {
            Ok(RunId(text.to_string()))
        } else {
            Err(format!("not a run id: {text:?}"))
        }
    }
}

impl TryFrom<String> for RunId {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<RunId> for String {
    fn from(id: RunId) -> String {
        id.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_well_formed_ids_parse() {
        let id = RunId::new();
        assert_eq!(id.as_str().parse::<RunId>().unwrap(), id);
        assert!("1793568534-37174-15387a".parse::<RunId>().is_ok());
        for bad in ["1-2", "a-2-3", "1-2-xyz", "1--3", "1-2-3-4", "", "../x"] {
            assert!(bad.parse::<RunId>().is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<RunId>("\"nope\"").is_err());
    }
}
