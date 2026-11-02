//! `fail_patterns`: a run whose command exited 0 still fails when a line of its log matches
//! one of the job's patterns. For commands that report a problem in their output but not
//! in their exit status, such as a test suite that skips itself.

use std::io::{BufRead, BufReader};
use std::path::Path;

use regex::Regex;

use crate::activity::OWN_LINE_PREFIX;

const MAX_CHARS: usize = 200;

/// The first line of the log that matches a pattern, with the pattern it matched.
/// drmanhattan's own lines are not the command's output and are never matched.
pub(super) fn first_match(log: &Path, patterns: &[Regex]) -> Option<(String, String)> {
    if patterns.is_empty() {
        return None;
    }
    let reader = BufReader::new(std::fs::File::open(log).ok()?);
    for line in reader.split(b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line);
        for part in line.split('\r') {
            let part = part.trim();
            if part.is_empty() || part.starts_with(OWN_LINE_PREFIX) {
                continue;
            }
            if let Some(re) = patterns.iter().find(|re| re.is_match(part)) {
                return Some((part.chars().take(MAX_CHARS).collect(), re.as_str().to_string()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_matching_line_of_the_commands_output_is_found() {
        let log = std::env::temp_dir().join(format!("drmanhattan-failpat-{}.log", std::process::id()));
        std::fs::write(
            &log,
            "drmanhattan · Skipped 2 suites\ncompiling\rlinking\nSkipped 3 suites: no database\nSkipped 1 suite\n",
        )
        .unwrap();
        let re = |p: &str| vec![Regex::new(p).unwrap()];
        assert_eq!(
            first_match(&log, &re(r"^Skipped \d+ suites?")),
            Some(("Skipped 3 suites: no database".into(), r"^Skipped \d+ suites?".into())),
            "drmanhattan's own lines are skipped"
        );
        assert_eq!(first_match(&log, &re("^linking$")).map(|m| m.0).as_deref(), Some("linking"), "\\r ends a line");
        assert_eq!(first_match(&log, &re("nothing")), None);
        assert_eq!(first_match(&log, &[]), None);
        let _ = std::fs::remove_file(&log);
    }
}
