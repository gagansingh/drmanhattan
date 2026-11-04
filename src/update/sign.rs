//! Signing a downloaded release before it replaces the installed one, when `config.json` has
//! `sign`: macOS ties privacy grants (such as access to removable volumes) to a binary's
//! signature, so each update re-signed with the same identity keeps them.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{invalid, Result};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct SignConfig {
    /// A code-signing identity in the login keychain, as `codesign --sign` takes it.
    pub(crate) identity: String,
    /// The signature's identifier; without it, codesign uses the file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) identifier: Option<String>,
}

/// Signs each of `programs` in `dir` and checks the result.
pub(super) fn sign(config: &SignConfig, dir: &Path, programs: &[&str]) -> Result<()> {
    for program in programs {
        let path = dir.join(program);
        let mut codesign = Command::new("codesign");
        codesign.args(["--force", "--options", "runtime", "--sign", &config.identity]);
        if let Some(identifier) = &config.identifier {
            codesign.args(["--identifier", identifier]);
        }
        run(codesign.arg(&path))?;
        run(Command::new("codesign").args(["--verify", "--strict"]).arg(&path))?;
    }
    Ok(())
}

fn run(command: &mut Command) -> Result<()> {
    match command.output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => invalid(format!(
            "could not sign the new version: {}; the installed version is unchanged",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => invalid(format!("could not run codesign: {e}; the installed version is unchanged")),
    }
}
