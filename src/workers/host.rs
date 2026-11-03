//! The worker's side: what a dispatcher runs on it over SSH.
//!
//! - `drmanhattan machine-stats` prints its version, whether its daemon runs, how busy it
//!   is, its CPU, memory and temperature, and its free disk, as JSON. The dispatcher polls
//!   it for the dashboard and checks it before each dispatch.
//! - `drmanhattan __dispatch-probe` does the same after housekeeping: it removes the
//!   mirrors this dispatcher no longer needs and records the ones it is about to send.
//! - `drmanhattan __dispatch-submit --name NAME` writes the job on its stdin into the
//!   running daemon's inbox, as any local tool would.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::acknowledge;
use crate::daemon;
use crate::error::{invalid, Error, Result};
use crate::fsutil;
use crate::job::{is_job_name, JobFiles, JobSpec, State};
use crate::paths::{self, Paths};
use crate::process;
use crate::queue::read_records;
use crate::settings::effective_capacity;
use crate::sysinfo;
use crate::time::Timestamp;

/// Bumped only when what a dispatcher and a worker say to each other changes: the probe,
/// the job files a dispatcher writes into the worker's inbox, or how it follows and
/// cancels them there. Machines whose protocols match work together whatever their versions.
pub(crate) const PROTOCOL: u32 = 1;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct MachineStats {
    pub(crate) version: String,
    /// The running daemon's protocol, else this command's.
    pub(crate) protocol: u32,
    pub(crate) hostname: String,
    pub(crate) home: PathBuf,
    /// Whether a person at this machine confirmed its drmanhattan may run jobs.
    pub(crate) acknowledged: bool,
    /// Whether its daemon is running, and the inbox it watches.
    pub(crate) daemon: bool,
    pub(crate) inbox: Option<PathBuf>,
    /// Jobs running here now, and how many may.
    pub(crate) running: usize,
    pub(crate) capacity: u32,
    pub(crate) cpu_pct: Option<u32>,
    pub(crate) load: Option<[f64; 3]>,
    pub(crate) ncpu: usize,
    pub(crate) memory: Option<serde_json::Value>,
    pub(crate) thermals: Option<serde_json::Value>,
    /// Free space on the disk holding the home directory.
    pub(crate) free_gb: Option<f64>,
    /// Mirrors `__dispatch-probe` removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) pruned: Vec<PathBuf>,
}

pub(crate) fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let name = if ok {
        String::from_utf8_lossy(buf.split(|b| *b == 0).next().unwrap_or_default()).into_owned()
    } else {
        String::new()
    };
    let name = name.trim_end_matches(".local").to_string();
    if name.is_empty() {
        "unknown".into()
    } else {
        name
    }
}

/// Free space at `path`, in GB.
fn free_gb(path: &Path) -> Option<f64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs fills the struct it is given; zeroed is a valid initial value.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: c is a valid C string and s a valid statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    let bytes = s.f_bavail as f64 * s.f_frsize as f64;
    Some((bytes / 1e9 * 10.0).round() / 10.0)
}

fn stats(paths: &Paths) -> MachineStats {
    let running = daemon::running(paths);
    let slots_env = daemon::slots_env(running.as_ref());
    let readings = sysinfo::read_now(Duration::from_millis(300));
    let home = paths::home();
    MachineStats {
        // A daemon from before 0.6.2 does not say; those all spoke protocol 1.
        version: running
            .as_ref()
            .and_then(|d| d.info.as_ref()?.version.clone())
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into()),
        protocol: running.as_ref().map_or(PROTOCOL, |d| d.info.as_ref().and_then(|i| i.protocol).unwrap_or(1)),
        hostname: hostname(),
        acknowledged: acknowledge::is_acknowledged(paths),
        daemon: running.is_some(),
        inbox: running.and_then(|d| d.info).map(|i| i.inbox),
        running: read_records(paths).iter().filter(|r| r.state == State::Running).count(),
        capacity: effective_capacity(paths, slots_env.as_deref()).unwrap_or(1),
        cpu_pct: readings.cpu_pct,
        load: process::load_averages().map(|l| l.map(|x| (x * 100.0).round() / 100.0)),
        ncpu: process::cpu_count(),
        memory: readings.memory.and_then(|m| serde_json::to_value(m).ok()),
        thermals: readings.thermals.and_then(|t| serde_json::to_value(t).ok()),
        free_gb: free_gb(&home),
        home,
        pruned: Vec::new(),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<i32> {
    println!("{}", serde_json::to_string(value).map_err(|e| Error::Invalid(e.to_string()))?);
    Ok(0)
}

/// `drmanhattan machine-stats`.
pub(crate) fn machine_stats(rest: &[String]) -> Result<i32> {
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        println!("usage: drmanhattan machine-stats\n\nPrints this machine's readings as JSON, for a dispatcher using it as a worker.");
        return Ok(0);
    }
    print_json(&stats(&Paths::from_env()))
}

/// What a dispatcher sends with a probe.
#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProbeRequest {
    pub(crate) dispatcher: String,
    /// Paths it is about to mirror here.
    #[serde(default)]
    pub(crate) mark: Vec<PathBuf>,
    /// Paths it mirrored here before and no longer has.
    #[serde(default)]
    pub(crate) prune: Vec<PathBuf>,
    /// Mirrors unused this long are removed too.
    #[serde(default)]
    pub(crate) mirror_days: u64,
}

/// One mirror a dispatcher made here.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Mirror {
    dispatcher: String,
    last_used: Timestamp,
}

type Mirrors = BTreeMap<PathBuf, Mirror>;

fn read_stdin() -> Result<String> {
    let mut text = String::new();
    std::io::stdin()
        .take(1 << 20)
        .read_to_string(&mut text)
        .map_err(|e| Error::Invalid(format!("reading stdin: {e}")))?;
    Ok(text)
}

/// `drmanhattan __dispatch-probe`: housekeeping, then `machine-stats`.
pub(crate) fn dispatch_probe(_rest: &[String]) -> Result<i32> {
    let req: ProbeRequest =
        serde_json::from_str(&read_stdin()?).map_err(|e| Error::Invalid(format!("not a probe request: {e}")))?;
    let paths = Paths::from_env();
    let pruned = housekeeping(&paths, &paths::home(), &req);
    let mut s = stats(&paths);
    s.pruned = pruned;
    print_json(&s)
}

/// A path under the home directory, with no `..` and no symlink on the way.
fn safe_under_home(path: &Path, home: &Path) -> bool {
    path.is_absolute()
        && path.starts_with(home)
        && path != home
        && !path.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir))
        && path.ancestors().take_while(|a| *a != home).all(|a| !fsutil::is_symlink(a))
}

/// Removes this dispatcher's mirrors that it no longer has or that went unused past
/// `mirror_days`, unless a job here uses them; then records the paths it is about to
/// mirror. A path that existed here before any dispatcher mirrored it is this machine's
/// own and is never recorded, so never removed.
fn housekeeping(paths: &Paths, home: &Path, req: &ProbeRequest) -> Vec<PathBuf> {
    let mut mirrors: Mirrors = fsutil::read_json(&paths.mirrors()).unwrap_or_default();
    let in_use: Vec<PathBuf> = read_records(paths)
        .into_iter()
        .filter(|r| !r.state.is_terminal())
        .flat_map(|r| {
            let mut used = vec![r.job.cwd.clone()];
            used.extend(r.job.sync.iter().map(|s| PathBuf::from(&s.path)));
            used
        })
        .collect();
    let overlaps = |p: &Path| {
        in_use.iter().any(|u| u.starts_with(p) || p.starts_with(u))
            || req.mark.iter().any(|m| m.starts_with(p) || p.starts_with(m))
    };
    let max_age = Duration::from_secs(req.mirror_days.max(1) * 86_400);
    let mut pruned = Vec::new();
    let candidates: Vec<PathBuf> = mirrors
        .iter()
        .filter(|(path, m)| {
            m.dispatcher == req.dispatcher && (req.prune.contains(path) || m.last_used.elapsed() > max_age)
        })
        .map(|(path, _)| path.clone())
        .collect();
    for path in candidates {
        if overlaps(&path) || !safe_under_home(&path, home) {
            continue;
        }
        if path.exists() && remove_tree(&path).is_err() {
            continue;
        }
        mirrors.remove(&path);
        pruned.push(path);
    }
    for path in &req.mark {
        if !safe_under_home(path, home) {
            continue;
        }
        let recorded = mirrors.contains_key(path);
        if recorded || !path.exists() {
            mirrors.insert(path.clone(), Mirror { dispatcher: req.dispatcher.clone(), last_used: Timestamp::now() });
        }
    }
    let _ = fsutil::ensure_private_dir(&paths.data).and_then(|()| fsutil::write_json(&paths.mirrors(), &mirrors));
    pruned
}

/// Removes a directory tree; build products hold read-only directories, so a failed first
/// attempt makes the tree writable and tries again.
fn remove_tree(path: &Path) -> std::io::Result<()> {
    if !path.is_dir() {
        return std::fs::remove_file(path);
    }
    std::fs::remove_dir_all(path).or_else(|_| {
        let _ = std::process::Command::new("chmod").arg("-R").arg("u+w").arg(path).status();
        std::fs::remove_dir_all(path)
    })
}

/// `drmanhattan __dispatch-submit --name NAME`: queues the job on stdin here.
pub(crate) fn dispatch_submit(rest: &[String]) -> Result<i32> {
    let name = match rest {
        [flag, name] if flag == "--name" && is_job_name(name) => name,
        _ => return invalid("usage: drmanhattan __dispatch-submit --name NAME < job.json"),
    };
    let text = read_stdin()?;
    let spec = JobSpec::parse(&text)?;
    let paths = Paths::from_env();
    let Some(inbox) = daemon::running(&paths).and_then(|d| d.info).map(|i| i.inbox) else {
        return invalid("drmanhattan is not running on this machine");
    };
    let files = JobFiles::new(&inbox, name);
    if files.job().exists() || (files.claimed().exists() && !files.exit().exists()) {
        return invalid(format!("a job named {name} is already queued or running here"));
    }
    let text = serde_json::to_string_pretty(&spec).map_err(|e| Error::Invalid(e.to_string()))?;
    fsutil::write_atomic(&files.job(), &format!("{text}\n"))?;
    println!("{}", files.job().display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_paths_under_home_are_safe() {
        let home = Path::new("/Users/someone");
        assert!(safe_under_home(&home.join("code/x"), home));
        assert!(!safe_under_home(home, home));
        assert!(!safe_under_home(&home.join("code/../../etc"), home));
        assert!(!safe_under_home(Path::new("/etc/passwd"), home));
        assert!(!safe_under_home(Path::new("relative"), home));
    }

    #[test]
    fn housekeeping_removes_only_mirrors_it_recorded_that_the_dispatcher_no_longer_has() {
        let root = std::env::temp_dir().join(format!("drmanhattan-mirrors-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let paths = Paths { state: root.join("state"), data: root.join("data") };
        let (own, gone, kept) = (home.join("own"), home.join("wt/gone"), home.join("wt/kept"));
        std::fs::create_dir_all(&own).unwrap();
        let req = |mark: Vec<PathBuf>, prune: Vec<PathBuf>| ProbeRequest {
            dispatcher: "mac".into(),
            mark,
            prune,
            mirror_days: 7,
        };
        // Marked before they exist: mirrors. `own` existed already: this machine's own.
        assert!(housekeeping(&paths, &home, &req(vec![own.clone(), gone.clone(), kept.clone()], vec![])).is_empty());
        for dir in [&gone, &kept] {
            std::fs::create_dir_all(dir.join("ro")).unwrap();
            std::fs::set_permissions(dir.join("ro"), std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap();
        }
        let pruned = housekeeping(&paths, &home, &req(vec![kept.clone()], vec![gone.clone(), own.clone()]));
        assert_eq!(pruned, std::slice::from_ref(&gone), "only a recorded mirror goes, read-only parts and all");
        assert!(!gone.exists() && kept.exists() && own.exists());
        let other = ProbeRequest { dispatcher: "other".into(), ..req(vec![], vec![kept.clone()]) };
        assert!(housekeeping(&paths, &home, &other).is_empty(), "another dispatcher's mirrors are not its to prune");
        let _ = std::process::Command::new("chmod").arg("-R").arg("u+w").arg(&root).status();
        let _ = std::fs::remove_dir_all(&root);
    }
}
