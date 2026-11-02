//! A job: what an inbox file asks to run (`spec`), the files it has in the inbox (`files`),
//! the id of one run of it (`run_id`), and the record of that run (`record`).

mod files;
mod record;
mod run_id;
mod spec;

pub(crate) use files::{classify, is_job_name, InboxFile, JobFiles, MAX_NAME};
pub(crate) use record::{FileStatus, LoadStats, PrepareState, Record, State};
pub(crate) use run_id::RunId;
pub(crate) use spec::{compile_fail_patterns, CommandSpec, Job, JobSpec, Source, SourceSpec};
