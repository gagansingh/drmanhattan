//! The installed command and the running daemon can differ: an update replaces the file,
//! then restarts the daemon. Until it does, `status` and the dashboard say so.

use std::sync::Mutex;
use std::time::SystemTime;

/// The installed version when it is not the running one; read again only when the file changes.
pub(crate) fn installed_version() -> Option<String> {
    static SEEN: Mutex<Option<(SystemTime, Option<String>)>> = Mutex::new(None);
    let exe = std::env::current_exe().ok()?;
    // Linux names a replaced executable "… (deleted)"; the new file has the plain name.
    let exe = std::path::PathBuf::from(exe.to_string_lossy().trim_end_matches(" (deleted)"));
    let modified = exe.metadata().and_then(|m| m.modified()).ok()?;
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, version)) = seen.as_ref().filter(|(at, _)| *at == modified) {
        return version.clone();
    }
    let out = std::process::Command::new(&exe).arg("--version").output().ok()?;
    let said = String::from_utf8_lossy(&out.stdout);
    let version = said.trim().strip_prefix("drmanhattan ").map(str::to_string).filter(|v| v != super::current());
    *seen = Some((modified, version.clone()));
    version
}

/// The running daemon's version when it is behind this command's. A daemon from before
/// 0.6.2 does not say its version: it is behind (`older`) when this file is newer than it.
pub(crate) fn daemon_behind(daemon: Option<&crate::daemon::RunningDaemon>) -> Option<String> {
    let info = daemon?.info.as_ref()?;
    match &info.version {
        Some(v) => (v != super::current()).then(|| v.clone()),
        None => {
            let installed_at = std::env::current_exe().and_then(|e| e.metadata()?.modified()).ok()?;
            (crate::time::Timestamp::from(installed_at) > info.started_at).then(|| "older".into())
        }
    }
}
