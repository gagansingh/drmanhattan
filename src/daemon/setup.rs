//! Getting ready to serve: directories, the inbox, the dashboard.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::daemon::lock::RunningDaemon;
use crate::daemon::ServeOptions;
use crate::dashboard;
use crate::error::{invalid, Error, IoContext, Result};
use crate::fsutil;
use crate::log;
use crate::naming::tilde_path;
use crate::paths::{self, Paths};
use crate::queue::Shared;

/// The state and data directories, private, with an error naming any that cannot be used.
pub(super) fn prepare_dirs(paths: &Paths) -> Result<()> {
    let dirs = [
        ("state directory", paths.state.clone()),
        ("state directory", paths.runs()),
        ("data directory", paths.data.clone()),
        ("data directory", paths.archive()),
    ];
    for (what, dir) in dirs {
        fsutil::ensure_private_dir(&dir).map_err(|e| Error::Invalid(format!("cannot use the {what}: {e}")))?;
    }
    Ok(())
}

/// The inbox: created private if missing; refused if it is a symlink, someone else's,
/// writable by others (anyone who can drop a file there can run commands as you), or not
/// writable by us.
pub(super) fn prepare_inbox(inbox: &Path) -> Result<PathBuf> {
    if fsutil::is_symlink(inbox) {
        return invalid(format!("refusing a symlinked inbox: {}", inbox.display()));
    }
    if !inbox.exists() {
        fs::create_dir_all(inbox).context("create the inbox", inbox)?;
        fs::set_permissions(inbox, fs::Permissions::from_mode(0o700)).context("set permissions on", inbox)?;
    }
    let meta = inbox.metadata().context("read the inbox", inbox)?;
    if !meta.is_dir() {
        return invalid(format!("the inbox is not a directory: {}", inbox.display()));
    }
    if meta.uid() != paths::uid() {
        return invalid(format!("the inbox is owned by another user: {}", inbox.display()));
    }
    if meta.mode() & 0o022 != 0 {
        return invalid(format!(
            "the inbox {} is writable by other users (mode {:o}), and anything dropped there runs as you; fix it with: chmod 700 {}",
            inbox.display(),
            meta.mode() & 0o777,
            inbox.display()
        ));
    }
    let probe = inbox.join(".drmanhattan-write-test");
    fs::write(&probe, b"").context("write to the inbox", inbox)?;
    let _ = fs::remove_file(&probe);
    Ok(inbox.canonicalize().unwrap_or_else(|_| inbox.to_path_buf()))
}

/// Binds the dashboard and returns its URL. A busy default port falls back to any free one;
/// a busy port the user chose is an error.
pub(super) fn start_dashboard(shared: &Shared, opts: &ServeOptions) -> Result<String> {
    let start = |port| dashboard::start(shared.clone(), &opts.bind, port, opts.session_links);
    let url = match start(opts.port) {
        Ok(url) => url,
        Err(e) if !opts.port_explicit => {
            log::problem(format_args!("{e}; using a free port instead (--port chooses one)"));
            start(0)?
        }
        Err(e) => return invalid(format!("{e}; pass --port <n> to use another port")),
    };
    if opts.open_browser {
        let url = url.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            open_in_browser(&url);
        });
    }
    Ok(url)
}

pub(crate) fn open_in_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(opener).arg(url).status();
}

/// "drmanhattan is already running (pid 4242, since 3:05 PM) — dashboard …, jobs in …"
pub(super) fn already_running_message(running: Option<&RunningDaemon>, inbox: &Path) -> String {
    let info = running.and_then(|r| r.info.as_ref());
    let mut line = String::from("drmanhattan is already running");
    match (running.map(|r| r.pid), info.map(|i| i.started_at.local_clock())) {
        (Some(pid), Some(since)) => line += &format!(" (pid {pid}, since {since})"),
        (Some(pid), None) => line += &format!(" (pid {pid})"),
        _ => {}
    }
    if let Some(url) = info.and_then(|i| i.url.as_deref()) {
        line += &format!(" — dashboard {url}");
    }
    let inbox = info.map_or(inbox, |i| i.inbox.as_path());
    line + &format!(", jobs in {}. Stop it with `drmanhattan stop`.", tilde_path(inbox))
}
