//! The line of a failed run's log to read first.

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::activity::OWN_LINE_PREFIX;
use crate::fsutil;

/// How much of the end of the log is searched.
const TAIL_BYTES: u64 = 64 * 1024;
const MAX_CHARS: usize = 200;

/// The last line in the tail of the log that names an error (`error`, `panicked`, a
/// traceback, `fatal`, `FAIL`, `✘`), else the last that says something failed. Summary lines
/// such as "1 failed, 119 passed" only win when nothing more specific was printed.
pub(crate) fn last_error_line(log: &Path) -> Option<String> {
    static STRONG: OnceLock<Regex> = OnceLock::new();
    static WEAK: OnceLock<Regex> = OnceLock::new();
    let strong = STRONG.get_or_init(|| {
        Regex::new(r"(?i)\b(error|errors|panic|panicked|fatal|exception|traceback)\b|^\s*(✘|FAIL\b)")
            .expect("the pattern compiles")
    });
    let weak = WEAK.get_or_init(|| Regex::new(r"(?i)\b(failed|failure|failing)\b").expect("the pattern compiles"));
    let text = fsutil::read_tail(log, TAIL_BYTES)?;
    let lines: Vec<&str> =
        text.split(['\n', '\r']).map(str::trim).filter(|l| !l.is_empty() && !l.starts_with(OWN_LINE_PREFIX)).collect();
    let pick = |re: &Regex| lines.iter().rev().find(|l| re.is_match(l)).map(|l| l.chars().take(MAX_CHARS).collect());
    pick(strong).or_else(|| pick(weak))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_line_is_the_last_one_that_looks_like_an_error() {
        let log = std::env::temp_dir().join(format!("drmanhattan-errline-{}.log", std::process::id()));
        let write = |text: &str| std::fs::write(&log, text).unwrap();
        write("compiling\nerror: first\nwarning: x\nerror: test_parse failed: expected 3, got 4\n1 failed, 119 passed\ndone\ndrmanhattan · failed in 3s, exit 1\n");
        assert_eq!(last_error_line(&log).as_deref(), Some("error: test_parse failed: expected 3, got 4"));
        write("ran 3 suites\n2 failed, 1 passed\n");
        assert_eq!(last_error_line(&log).as_deref(), Some("2 failed, 1 passed"), "a summary is the fallback");
        write("all good\n");
        assert_eq!(last_error_line(&log), None);
        let _ = std::fs::remove_file(&log);
    }
}
