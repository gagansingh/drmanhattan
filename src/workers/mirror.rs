//! Mirroring a job's files to a worker, at the same absolute paths.
//!
//! Each `sync` path (by default the job's `cwd`) goes with `rsync -a --delete`. Left out:
//! everything git ignores there (build products, secrets such as `.env`), a `.git`
//! directory, `.DS_Store`, and the entry's own `exclude` patterns. Ignored paths are left
//! out, not deleted, on the worker, so it keeps its own build products and inputs. A path
//! that is not in a git checkout leaves out what its `.gitignore` files name. With
//! `sync_git`, the git metadata of each synced checkout goes first, additively.
//!
//! After a job succeeds there, each of its `sync_back` paths comes the other way
//! (`copy_back`), so its products are here when the run is marked finished.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::job::{Job, SyncPath};
use crate::paths;
use crate::workers::ssh::{self, rsync_program, ssh_program};
use crate::workers::WorkerConfig;

/// The longest one rsync may take.
const RSYNC_LIMIT: Duration = Duration::from_secs(1800);
const ALWAYS_EXCLUDED: [&str; 2] = ["/.git/", ".DS_Store"];

/// One path to mirror, checked: it exists, it is under the home directory, and it is given
/// with no symlink or `..` on the way.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub(crate) path: PathBuf,
    pub(crate) exclude: Vec<String>,
}

/// The job's `sync` entries, or its `cwd`, checked.
pub(crate) fn entries(job: &Job) -> Result<Vec<Entry>, String> {
    let given = if job.sync.is_empty() {
        vec![SyncPath { path: job.cwd.to_string_lossy().into_owned(), exclude: Vec::new() }]
    } else {
        job.sync.clone()
    };
    let home = paths::home().canonicalize().unwrap_or_else(|_| paths::home());
    given
        .into_iter()
        .map(|s| {
            let path = Path::new(&s.path).canonicalize().map_err(|e| format!("cannot mirror {}: {e}", s.path))?;
            if !path.starts_with(&home) || path == home {
                return Err(format!(
                    "cannot mirror {}: only paths inside {} are mirrored",
                    path.display(),
                    home.display()
                ));
            }
            Ok(Entry { path, exclude: s.exclude })
        })
        .collect()
}

/// rsync's `-e`: ssh with the same options as every other connection to the worker.
fn remote_shell() -> String {
    std::iter::once(ssh_program())
        .chain(ssh::options())
        .map(|w| if w.contains(' ') { format!("'{w}'") } else { w })
        .collect::<Vec<_>>()
        .join(" ")
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What git ignores under `dir`, as anchored rsync patterns; None outside a git checkout.
fn ignored(dir: &Path) -> Option<Vec<String>> {
    let text = git(dir, &["ls-files", "-o", "-i", "--exclude-standard", "--directory"])?;
    Some(text.lines().filter(|l| !l.is_empty()).map(|l| format!("/{l}")).collect())
}

/// Makes the parents of these paths exist on the worker: rsync creates only the last part.
pub(crate) fn make_parents(worker: &WorkerConfig, paths: &[PathBuf]) -> Result<(), String> {
    let mut argv = vec!["mkdir".to_string(), "-p".to_string()];
    argv.extend(paths.iter().filter_map(|p| p.parent()).map(|p| p.to_string_lossy().into_owned()));
    let out = ssh::run(worker, &argv, None, Duration::from_secs(60));
    (out.code == 0)
        .then_some(())
        .ok_or_else(|| format!("could not create folders on {}: {}", worker.name, out.first_line()))
}

/// Mirrors one entry with `rsync -a --delete`.
pub(crate) fn mirror(worker: &WorkerConfig, entry: &Entry) -> Result<(), String> {
    let is_dir = entry.path.is_dir();
    let mut c = Command::new(rsync_program());
    c.args(["-a", "--delete", "-e"]).arg(remote_shell());
    if is_dir {
        let excluded = ignored(&entry.path);
        if excluded.is_none() {
            c.arg("--filter=:- .gitignore");
        }
        for pattern in ALWAYS_EXCLUDED
            .iter()
            .map(|p| p.to_string())
            .chain(excluded.unwrap_or_default())
            .chain(entry.exclude.iter().cloned())
        {
            c.arg(format!("--exclude={pattern}"));
        }
    }
    let path = entry.path.to_string_lossy();
    let (from, to) = if is_dir {
        (format!("{path}/"), format!("{}:{path}/", worker.ssh))
    } else {
        (path.to_string(), format!("{}:{path}", worker.ssh))
    };
    c.arg(from).arg(to);
    let out = ssh::run_command(c, None, RSYNC_LIMIT);
    (out.code == 0).then_some(()).ok_or_else(|| format!("could not mirror {path}: {}", out.first_line()))
}

/// Copies `path` back from the worker with `rsync -a --delete`, so it is here as the job
/// left it there (files only here inside it are removed). On the worker it is at the same
/// path, or the one `args_path_map` gives; here it must lie inside the home directory, and
/// its parents are made.
pub(crate) fn copy_back(worker: &WorkerConfig, path: &str) -> Result<(), String> {
    let here = Path::new(path);
    let home = paths::home();
    let inside = |h: &Path| here.starts_with(h) && here != h;
    if !inside(&home) && !home.canonicalize().is_ok_and(|h| inside(&h)) {
        return Err(format!("cannot copy back {path}: only paths inside {} are copied", home.display()));
    }
    let (Some(parent), Some(name)) = (here.parent(), here.file_name()) else {
        return Err(format!("cannot copy back {path}: it has no parent folder"));
    };
    let there = worker.map_args(&[path.to_string()]).remove(0);
    if Path::new(&there).file_name() != Some(name) {
        return Err(format!("cannot copy back {path}: args_path_map gives it another name there ({there})"));
    }
    std::fs::create_dir_all(parent).map_err(|e| format!("cannot copy back {path}: {e}"))?;
    let mut c = Command::new(rsync_program());
    c.args(["-a", "--delete", "-e"]).arg(remote_shell());
    c.arg(format!("{}:{there}", worker.ssh)).arg(format!("{}/", parent.display()));
    let out = ssh::run_command(c, None, RSYNC_LIMIT);
    (out.code == 0).then_some(()).ok_or_else(|| format!("could not copy back {path}: {}", out.first_line()))
}

/// A checkout's common git directory, when `dir` is in one.
pub(crate) fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let common = git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    Path::new(common.trim()).canonicalize().ok()
}

/// The git metadata a worker needs to resolve `HEAD`, branches and commits in a mirrored
/// checkout as here: objects, refs and config of its common git directory, plus the
/// checkout's own `worktrees/<name>` entry when it is a linked worktree. Additive (never
/// `--delete`), and the main worktree's `HEAD` and `index` go only when it is the one
/// mirrored. Nothing for a path outside a git checkout.
pub(crate) fn mirror_git(worker: &WorkerConfig, checkout: &Path) -> Result<(), String> {
    let resolve = |args: &[&str]| -> Option<PathBuf> {
        let dir = git(checkout, args)?;
        Path::new(dir.trim()).canonicalize().ok()
    };
    let Some(common) = resolve(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) else { return Ok(()) };
    let git_dir = resolve(&["rev-parse", "--path-format=absolute", "--git-dir"]).unwrap_or_else(|| common.clone());
    let home = paths::home().canonicalize().unwrap_or_else(|_| paths::home());
    if !common.starts_with(&home) {
        return Err(format!("cannot mirror {}: only paths inside {} are mirrored", common.display(), home.display()));
    }
    let mut c = Command::new(rsync_program());
    c.args(["-a", "-e"]).arg(remote_shell()).args(["--exclude=*.lock", "--exclude=/gc.pid"]);
    match git_dir.strip_prefix(common.join("worktrees")).ok().and_then(|n| n.to_str()).filter(|n| !n.is_empty()) {
        Some(name) => {
            c.arg("--include=/worktrees/").arg(format!("--include=/worktrees/{name}/***"));
            c.args(["--exclude=/worktrees/*", "--exclude=/HEAD", "--exclude=/index"]);
        }
        None => {
            c.arg("--exclude=/worktrees/*");
        }
    }
    let path = common.to_string_lossy();
    c.arg(format!("{path}/")).arg(format!("{}:{path}/", worker.ssh));
    let out = ssh::run_command(c, None, RSYNC_LIMIT);
    (out.code == 0)
        .then_some(())
        .ok_or_else(|| format!("could not mirror the git metadata in {path}: {}", out.first_line()))
}
