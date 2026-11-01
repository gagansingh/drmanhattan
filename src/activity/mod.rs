//! What a running job is doing, read from its log as it grows.
//!
//! A job can say where it is by printing marker lines:
//!
//! ```text
//! ::step 2/9 Building the server image
//! ::start swift build
//! ::done swift build
//! ```
//!
//! The dashboard then shows the current step, each command that has started and not
//! finished with its elapsed time against its usual time (learned from earlier runs), and
//! otherwise the last line of output. Tools that print their own progress can be matched
//! instead of changed: a job's `activity` field, or `config.json`, gives regular expressions
//! with named groups `n`, `of` and `text` (the step) and `label` (start and done).

mod patterns;
mod scan;
mod typicals;

pub(crate) use patterns::{PatternConfig, Patterns};
pub(crate) use scan::{Activity, ActivityView, OWN_LINE_PREFIX};
pub(crate) use typicals::{append as append_steps, StepRow, Typicals};
