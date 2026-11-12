//! `drmanhattan bb`: offers the plugin for bb (github.com/get-bb/bb), the editor that runs
//! coding agents in threads. The plugin lives in this repository, in `integrations/bb`, and is
//! listed in bb's Community marketplace as `drmanhattan`, whose entry installs it from this
//! repository's release tags of this major version.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::args::{self, Spec};
use crate::error::{invalid, Result};

const BB: Spec = Spec {
    usage: "\
usage: drmanhattan bb [install]

bb (github.com/get-bb/bb) is an editor that runs coding agents in threads. Its drmanhattan
plugin tells those agents to run builds and tests through this queue, and shows each
thread's runs in bb. Runs submitted from a bb thread link back to it on the dashboard
either way.

The plugin is in bb's Community marketplace: find drmanhattan in bb's plugin catalog, or
install it from here. With no argument, prints the command that installs it. `install` runs
it with the `bb` command, which shows what it installs and asks before it does.
",
    values: &[],
    switches: &[],
    positional: 1,
};

/// `bb plugin install drmanhattan`: the marketplace entry, which bb has reviewed.
fn install_args() -> Vec<String> {
    ["plugin", "install", "drmanhattan"].map(String::from).to_vec()
}

/// bb's own command: the one bb gives its threads (`BB_CLI`), else `bb` on PATH.
fn bb_command() -> Option<PathBuf> {
    let given = std::env::var_os("BB_CLI").map(PathBuf::from).filter(|p| p.is_file());
    given.or_else(|| {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).map(|dir| dir.join("bb")).find(|p| is_executable(p))
    })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

pub(super) fn bb(rest: &[String]) -> Result<i32> {
    let Some(a) = args::parse(&BB, rest)? else { return Ok(0) };
    let install = install_args();
    match a.positional.first().map(String::as_str) {
        None => {
            println!("bb {}", crate::naming::shell_words(&install));
            println!("installs drmanhattan's plugin for bb (`drmanhattan bb install` runs it)");
            Ok(0)
        }
        Some("install") => {
            let Some(bb) = bb_command() else {
                return invalid(
                    "no `bb` command here: in bb, Settings → General → Command line → Install puts it on PATH",
                );
            };
            let status = Command::new(&bb)
                .args(&install)
                .status()
                .map_err(|e| crate::error::Error::Invalid(format!("could not run {}: {e}", bb.display())))?;
            Ok(status.code().unwrap_or(1))
        }
        Some(other) => invalid(format!("bb takes install or nothing, not {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugin_installs_from_the_marketplace() {
        assert_eq!(install_args(), ["plugin", "install", "drmanhattan"]);
    }
}
