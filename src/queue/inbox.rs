//! Reading the inbox: finding job files and cancel requests, and claiming jobs.
//!
//! All of this happens without the queue lock: listing a large inbox and reading job files
//! must never hold up the dashboard or a runner.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{invalid, IoContext, Result};
use crate::fsutil;
use crate::job::{classify, FileStatus, InboxFile, Job, JobFiles, JobSpec, MAX_NAME};
use crate::log;
use crate::paths;

const MAX_JOB_BYTES: u64 = 256 * 1024;
/// A job file younger than this may still be being written; it waits for the next pass.
const SETTLE: Duration = Duration::from_millis(100);

/// What one look at the inbox found.
#[derive(Default, Debug)]
pub(crate) struct InboxScan {
    /// Settled job files, oldest first.
    pub(crate) jobs: Vec<JobFiles>,
    /// The name of every job file present, settled or not.
    pub(crate) waiting: HashSet<String>,
    /// Names with a `.cancel` file.
    pub(crate) cancels: Vec<String>,
    /// Job files that are never run: another user's, or named too long to claim.
    pub(crate) refused: Vec<(PathBuf, &'static str)>,
}

pub(crate) fn scan_inbox(inbox: &Path) -> InboxScan {
    let mut scan = InboxScan::default();
    let mut jobs: Vec<(Duration, JobFiles)> = Vec::new();
    for entry in fs::read_dir(inbox).into_iter().flatten().flatten() {
        let file_name = entry.file_name();
        let Some((kind, name)) = file_name.to_str().and_then(classify) else { continue };
        // Result files are most of the inbox; they are recognised by name alone, unstatted.
        if kind == InboxFile::Result {
            continue;
        }
        let Ok(meta) = entry.path().symlink_metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        match kind {
            InboxFile::Cancel => scan.cancels.push(name.to_string()),
            InboxFile::Job if meta.uid() != paths::uid() => {
                scan.refused.push((entry.path(), "it belongs to another user"))
            }
            InboxFile::Job if name.len() > MAX_NAME => {
                scan.refused.push((entry.path(), "its name is too long to add the result suffixes to"));
            }
            InboxFile::Job => {
                scan.waiting.insert(name.to_string());
                // A modification time in the future (a clock change) counts as settled.
                let age = fsutil::age(&meta).unwrap_or(Duration::MAX);
                if age >= SETTLE {
                    jobs.push((age, JobFiles::new(inbox, name)));
                }
            }
            InboxFile::Result => {}
        }
    }
    jobs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name().cmp(b.1.name())));
    scan.jobs = jobs.into_iter().map(|(_, files)| files).collect();
    scan
}

/// Takes ownership of a job file and validates it. The rename to `.json.claimed` is the
/// claim: it is atomic, it stops the file being picked up again (after a restart too), and
/// it leaves the job's text in place for anyone debugging. A file that is not a valid job
/// becomes `.json.rejected`, with the reason in `.status.json` and 2 in `.exit`.
///
/// The results of an earlier job with the same name are removed first, so a poller that
/// waits for `<name>.exit` never mistakes the previous run's result for this one's.
///
/// Ok(None) means the file was rejected; Err means it could not be claimed at all.
pub(crate) fn claim_job(files: &JobFiles, inbox: &Path) -> Result<Option<Job>> {
    for old in [files.exit(), files.status(), files.log()] {
        fsutil::remove_if_present(&old)?;
    }
    fs::rename(files.job(), files.claimed()).context("claim", &files.job())?;
    match read_job(files, inbox) {
        Ok(job) => Ok(Some(Job { claim: fsutil::file_id(&files.claimed()), ..job })),
        Err(e) => {
            reject(files, &e.to_string());
            Ok(None)
        }
    }
}

fn read_job(files: &JobFiles, inbox: &Path) -> Result<Job> {
    let claimed = files.claimed();
    if fs::metadata(&claimed).context("read", &claimed)?.len() > MAX_JOB_BYTES {
        return invalid(format!("the job file is larger than {} KB", MAX_JOB_BYTES / 1024));
    }
    JobSpec::parse(&fs::read_to_string(&claimed).context("read", &claimed)?)?.resolve(files, inbox)
}

fn reject(files: &JobFiles, reason: &str) {
    let rejected = files.rejected();
    let job_file = if fs::rename(files.claimed(), &rejected).is_ok() { rejected } else { files.claimed() };
    let _ = fsutil::write_json(&files.status(), &FileStatus::rejected(reason, &job_file));
    let _ = fsutil::write_atomic(&files.exit(), "2\n");
    log::event(log::Tone::Bad, "rejected", files.name(), None, &[reason.to_string()]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("drmanhattan-inbox-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_scan_sorts_files_into_jobs_cancels_and_refusals() {
        let dir = inbox("scan");
        for name in ["a.json", "a.log", "a.status.json", "b.cancel", ".hidden.json", "notes.txt"] {
            fs::write(dir.join(name), "{}").unwrap();
        }
        let long = format!("{}.json", "x".repeat(245));
        fs::write(dir.join(&long), "{}").unwrap();
        std::thread::sleep(SETTLE);
        let scan = scan_inbox(&dir);
        assert_eq!(scan.jobs.iter().map(JobFiles::name).collect::<Vec<_>>(), ["a"]);
        assert_eq!(scan.cancels, ["b"]);
        assert_eq!(scan.refused.len(), 1, "a name too long for its result files is refused, not ignored");
        let _ = fs::remove_dir_all(&dir);
    }
}
