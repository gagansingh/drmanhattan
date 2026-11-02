//! Releases: which is the latest, and fetching and checking this platform's archive. Uses
//! `curl` and `shasum`/`sha256sum`, as the installer does; `DRMANHATTAN_RELEASES_URL` points
//! at another release server.

use std::path::Path;
use std::process::Command;

use crate::error::{invalid, Error, Result};

const DEFAULT_RELEASES: &str = "https://github.com/gagansingh/drmanhattan/releases";

pub(crate) fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub(super) fn releases_url() -> String {
    std::env::var("DRMANHATTAN_RELEASES_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_RELEASES.into())
        .trim_end_matches('/')
        .to_string()
}

/// `1.2.3` (or `v1.2.3`) as numbers; anything after the numbers is ignored.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok();
    Some((next()?, next()?, next()?))
}

/// Whether `latest` is newer than `than`.
pub(super) fn is_newer(latest: &str, than: &str) -> bool {
    matches!((parse_version(latest), parse_version(than)), (Some(a), Some(b)) if a > b)
}

/// The latest release's tag, from where `<releases>/latest` redirects to: no API token.
pub(super) fn latest_tag() -> Result<String> {
    let url = format!("{}/latest", releases_url());
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "20", "-o", "/dev/null", "-w", "%{url_effective}", &url])
        .output()
        .map_err(|e| Error::Invalid(format!("could not run curl: {e}")))?;
    let effective = String::from_utf8_lossy(&out.stdout).to_string();
    let tag = effective.rsplit_once("/tag/").map(|(_, t)| t.trim().to_string());
    match tag.filter(|t| out.status.success() && parse_version(t).is_some()) {
        Some(tag) => Ok(tag),
        None => invalid(format!("could not find the latest release at {url}")),
    }
}

/// This platform's archive name, as the release workflow names them.
pub(super) fn asset(tag: &str) -> Result<String> {
    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", _) => "macos-universal",
        ("linux", "x86_64") => "linux-x86_64",
        (os, arch) => return invalid(format!("no prebuilt drmanhattan for {os} {arch}; build it with cargo")),
    };
    Ok(format!("drmanhattan-{tag}-{platform}.tar.gz"))
}

pub(super) fn download(url: &str, to: &Path) -> Result<()> {
    let ok = Command::new("curl")
        .args(["-fsSL", "--max-time", "300", "-o"])
        .arg(to)
        .arg(url)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        Ok(())
    } else {
        invalid(format!("could not download {url}"))
    }
}

pub(super) fn sha256(path: &Path) -> Option<String> {
    for (program, args) in [("shasum", &["-a", "256"][..]), ("sha256sum", &[][..])] {
        if let Ok(out) = Command::new(program).args(args).arg(path).output() {
            if out.status.success() {
                return String::from_utf8_lossy(&out.stdout).split_whitespace().next().map(str::to_lowercase);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("v0.2.10", "0.2.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.2.4", "0.2.4"));
        assert!(!is_newer("0.2.3", "0.2.4"));
        assert!(!is_newer("garbage", "0.2.4"));
        assert_eq!(parse_version("v1.2.3-rc1"), Some((1, 2, 3)));
    }
}
