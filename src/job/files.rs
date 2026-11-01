//! The files one job has in the inbox, all named after it: `build.json` is the job;
//! `build.json.claimed` or `build.json.rejected` once picked up; `build.log`,
//! `build.status.json` and `build.exit` its results; `build.cancel` a request to stop it.
//! This module is the only place those names are built or recognised.

use std::path::{Path, PathBuf};

/// The longest job name: the longest file name file systems allow (255 bytes), less the
/// longest suffix a job's files get (`.json.rejected`).
pub(crate) const MAX_NAME: usize = 255 - ".json.rejected".len();

/// Whether `name` can name a job: a plain file name that its own `<name>.json` file is read
/// back as. (`x.status` cannot be one: `x.status.json` is job `x`'s status file.)
pub(crate) fn is_job_name(name: &str) -> bool {
    name.len() <= MAX_NAME && !name.contains('/') && classify(&format!("{name}.json")) == Some((InboxFile::Job, name))
}

/// What a file in the inbox is to the queue.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InboxFile {
    /// A job waiting to be picked up.
    Job,
    /// A request to cancel the job with this name.
    Cancel,
    /// A claimed or rejected job file, or one of a job's results.
    Result,
}

/// Suffixes and what they mean, longest first, so `x.status.json` is never read as a job.
const KINDS: [(&str, InboxFile); 7] = [
    (".json.claimed", InboxFile::Result),
    (".json.rejected", InboxFile::Result),
    (".status.json", InboxFile::Result),
    (".json", InboxFile::Job),
    (".cancel", InboxFile::Cancel),
    (".exit", InboxFile::Result),
    (".log", InboxFile::Result),
];

/// What `file_name` is, and the job name it belongs to. Hidden files (temp files, a `submit`
/// in progress) and anything else are not the queue's.
pub(crate) fn classify(file_name: &str) -> Option<(InboxFile, &str)> {
    if file_name.starts_with('.') {
        return None;
    }
    KINDS.into_iter().find_map(|(suffix, kind)| Some((kind, file_name.strip_suffix(suffix).filter(|n| !n.is_empty())?)))
}

/// The paths of one job's files.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct JobFiles {
    base: PathBuf,
}

impl JobFiles {
    pub(crate) fn new(inbox: &Path, name: &str) -> JobFiles {
        JobFiles { base: inbox.join(name) }
    }

    /// The files of the job that `path` (any of its files) belongs to.
    pub(crate) fn of(path: &Path) -> Option<JobFiles> {
        let (_, name) = classify(path.file_name()?.to_str()?)?;
        Some(JobFiles::new(path.parent()?, name))
    }

    pub(crate) fn name(&self) -> &str {
        self.base.file_name().and_then(|n| n.to_str()).unwrap_or_default()
    }

    fn with(&self, suffix: &str) -> PathBuf {
        let mut path = self.base.clone().into_os_string();
        path.push(suffix);
        path.into()
    }

    pub(crate) fn job(&self) -> PathBuf {
        self.with(".json")
    }
    pub(crate) fn claimed(&self) -> PathBuf {
        self.with(".json.claimed")
    }
    pub(crate) fn rejected(&self) -> PathBuf {
        self.with(".json.rejected")
    }
    pub(crate) fn status(&self) -> PathBuf {
        self.with(".status.json")
    }
    pub(crate) fn log(&self) -> PathBuf {
        self.with(".log")
    }
    pub(crate) fn exit(&self) -> PathBuf {
        self.with(".exit")
    }
    pub(crate) fn cancel(&self) -> PathBuf {
        self.with(".cancel")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_of_a_job_is_recognised() {
        assert_eq!(classify("build.json"), Some((InboxFile::Job, "build")));
        assert_eq!(classify("build.cancel"), Some((InboxFile::Cancel, "build")));
        for result in ["build.json.claimed", "build.json.rejected", "build.status.json", "build.log", "build.exit"] {
            assert_eq!(classify(result), Some((InboxFile::Result, "build")), "{result}");
        }
        for other in [".build.json", ".tmp.1.2", "notes.txt", "x.json.tmp", ".json"] {
            assert_eq!(classify(other), None, "{other}");
        }
        assert!(is_job_name("build.v2") && is_job_name("check.py-1-2-ab"));
        for bad in ["", "x.status", "a/b", ".hidden"] {
            assert!(!is_job_name(bad), "{bad}");
        }
    }

    #[test]
    fn names_with_dots_keep_them() {
        let files = JobFiles::new(Path::new("/in"), "check.py-1-2-ab");
        assert_eq!(files.exit(), Path::new("/in/check.py-1-2-ab.exit"));
        assert_eq!(files.status(), Path::new("/in/check.py-1-2-ab.status.json"));
        assert_eq!(JobFiles::of(Path::new("/in/check.py-1-2-ab.json.claimed")), Some(files));
    }
}
