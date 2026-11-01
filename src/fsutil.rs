//! File helpers with the guarantees the rest of the crate relies on: private directories,
//! atomic writes (temp file and rename, so a reader never sees half a file), refusal to
//! follow symlinks planted in our directories, and bounded reads of growing logs.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{invalid, IoContext, Result};
use crate::paths;

pub(crate) fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())
}

/// Creates `path` if needed and makes sure it is a directory of ours that nobody else can
/// read: mode `0700`, owned by us, not a symlink.
pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    if is_symlink(path) {
        return invalid(format!("refusing a symlinked directory: {}", path.display()));
    }
    if !path.exists() {
        fs::create_dir_all(path).context("create", path)?;
    }
    let meta = path.symlink_metadata().context("read", path)?;
    if !meta.is_dir() {
        return invalid(format!("not a directory: {}", path.display()));
    }
    if meta.uid() != paths::uid() {
        return invalid(format!("owned by another user: {}", path.display()));
    }
    if meta.mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).context("set permissions on", path)?;
    }
    Ok(())
}

pub(crate) fn is_private_dir(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.is_dir() && m.uid() == paths::uid() && m.mode() & 0o777 == 0o700)
}

/// A regular file of ours: not a symlink, not another user's.
pub(crate) fn is_own_file(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.is_file() && m.uid() == paths::uid())
}

/// A file's identity: its device, inode and modification time, which all survive a rename.
/// Two job files with the same name are told apart by it; the time is there because a file
/// system may give a new file the inode of one deleted earlier.
#[derive(Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileId {
    dev: u64,
    ino: u64,
    /// Modification time: seconds since the epoch, and nanoseconds.
    mtime: i64,
    mtime_nsec: i64,
}

pub(crate) fn file_id(path: &Path) -> Option<FileId> {
    let m = path.symlink_metadata().ok()?;
    Some(FileId { dev: m.dev(), ino: m.ino(), mtime: m.mtime(), mtime_nsec: m.mtime_nsec() })
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `text` to `path` through a temp file and a rename. No fsync: these files are
/// rewritten constantly and readers rely on the rename's atomicity, not on durability.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".tmp.{}.{n}", std::process::id()));
    let result = (|| {
        let mut file =
            fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).context("create", &tmp)?;
        file.write_all(text.as_bytes()).context("write", &tmp)?;
        if is_symlink(path) {
            return invalid(format!("refusing to replace a symlink: {}", path.display()));
        }
        fs::rename(&tmp, path).context("write", path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub(crate) fn write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_string(value).map_err(|e| crate::error::Error::Invalid(e.to_string()))?;
    text.push('\n');
    write_atomic(path, &text)
}

/// A JSON file of ours, or None if it is missing, not ours, or does not parse.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    if !is_own_file(path) {
        return None;
    }
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

pub(crate) fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).context("remove", path),
        _ => Ok(()),
    }
}

/// How long ago a file was last modified.
pub(crate) fn age(meta: &fs::Metadata) -> Option<Duration> {
    SystemTime::now().duration_since(meta.modified().ok()?).ok()
}

/// Serialises appends to, and trims of, the line-per-record files (`history.jsonl`,
/// `steps.jsonl`), so a trim's rewrite can never drop a line appended meanwhile.
static JOURNAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn append_line(path: &Path, line: &str) -> Result<()> {
    let _journal = crate::sync::lock(&JOURNAL);
    let mut file = fs::OpenOptions::new().append(true).create(true).mode(0o600).open(path).context("open", path)?;
    file.write_all(format!("{line}\n").as_bytes()).context("append to", path)
}

/// The last `max_bytes` of a file, as text.
pub(crate) fn read_tail(path: &Path, max_bytes: u64) -> Option<String> {
    let mut f = fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(size.saturating_sub(max_bytes))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Keeps a line-per-record file bounded: past `max` lines, only the newest `keep` stay.
pub(crate) fn trim_lines(path: &Path, max: usize, keep: usize) {
    let _journal = crate::sync::lock(&JOURNAL);
    if !is_own_file(path) {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else { return };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > max {
        let kept: String = lines[lines.len() - keep..].iter().map(|l| format!("{l}\n")).collect();
        let _ = write_atomic(path, &kept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("drmanhattan-fsutil-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn atomic_writes_refuse_symlinks_and_leave_no_temp_files() {
        let dir = scratch("atomic");
        let target = dir.join("real");
        write_atomic(&target, "one").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "one");
        std::os::unix::fs::symlink(&target, dir.join("link")).unwrap();
        assert!(write_atomic(&dir.join("link"), "two").is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "one");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2, "no temp file left behind");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn private_dirs_are_tightened() {
        let dir = scratch("private").join("p");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert!(is_private_dir(&dir));
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }
}
