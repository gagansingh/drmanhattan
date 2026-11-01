//! The one-time confirmation before the first start.
//!
//! Starting drmanhattan means agreeing that it runs, as you, whatever is placed in its inbox.
//! The first `drmanhattan` (or `serve`) says so plainly and asks once. The answer is kept in
//! `acknowledged.json` in the data directory and never asked for again, across upgrades and
//! inbox changes. `--yes`, or `DRMANHATTAN_ACCEPT=1`, confirms without asking, for services
//! that have no terminal to ask in. The other commands never ask.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{invalid, Result};
use crate::fsutil;
use crate::naming::tilde_path;
use crate::paths::Paths;
use crate::time::Timestamp;

pub(crate) const ACCEPT_ENV: &str = "DRMANHATTAN_ACCEPT";

/// `acknowledged.json`.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Acknowledgement {
    acknowledged_at: Timestamp,
    /// The version that asked.
    version: String,
    /// "terminal", "--yes" or the environment variable.
    how: String,
}

/// Whether to go on starting the daemon: true once confirmed (now or before), false when
/// the person declined. Without a terminal to ask in and no confirmation, an error that
/// says how to give one.
pub(crate) fn confirm(paths: &Paths, inbox: &Path, yes: bool) -> Result<bool> {
    if fsutil::read_json::<Acknowledgement>(&paths.acknowledgement()).is_some() {
        return Ok(true);
    }
    let how = if yes {
        "--yes"
    } else if std::env::var(ACCEPT_ENV).is_ok_and(|v| v == "1") {
        ACCEPT_ENV
    } else if std::io::stdin().is_terminal() {
        let mut stderr = std::io::stderr();
        if !ask(&mut std::io::stdin().lock(), &mut stderr, inbox) {
            return Ok(false);
        }
        "terminal"
    } else {
        return invalid(format!(
            "Run `drmanhattan` once in a terminal to confirm, or pass --yes (or set {ACCEPT_ENV}=1)."
        ));
    };
    fsutil::ensure_private_dir(&paths.data)?;
    let record = Acknowledgement {
        acknowledged_at: Timestamp::now(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        how: how.to_string(),
    };
    fsutil::write_json(&paths.acknowledgement(), &record)?;
    Ok(true)
}

/// The question itself: true only for an answer of y or yes. End of input is a no.
fn ask(input: &mut impl BufRead, output: &mut impl Write, inbox: &Path) -> bool {
    let _ = write!(output, "{}", notice(inbox));
    let _ = output.flush();
    let mut answer = String::new();
    if input.read_line(&mut answer).unwrap_or(0) == 0 {
        let _ = writeln!(output);
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn notice(inbox: &Path) -> String {
    format!(
        "drmanhattan will run any command placed in {}, as you, with your permissions.\n\
         Only start it if you're fine with that. Continue? [y/N] ",
        tilde_path(inbox)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(input: &str) -> (bool, String) {
        let mut out = Vec::new();
        let yes = ask(&mut input.as_bytes(), &mut out, Path::new("/srv/inbox"));
        (yes, String::from_utf8(out).unwrap())
    }

    #[test]
    fn only_yes_continues() {
        let (yes, shown) = answer("y\n");
        assert!(yes);
        assert_eq!(
            shown,
            "drmanhattan will run any command placed in /srv/inbox, as you, with your permissions.\n\
             Only start it if you're fine with that. Continue? [y/N] "
        );
        assert!(answer("YES\n").0);
        for no in ["\n", "n\n", "no\n", "sure\n", ""] {
            assert!(!answer(no).0, "{no:?}");
        }
    }
}
