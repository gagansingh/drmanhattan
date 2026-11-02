//! Following one running job's log.
//!
//! Each scan reads only what was appended since the last one, so following a log costs
//! almost nothing however large it grows. A carriage return ends a line just as a newline
//! does (progress bars redraw themselves with `\r`), and an unfinished last line is kept for
//! the next scan, up to a cap, so output with no line breaks cannot grow memory.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use super::{Patterns, StepRow, Typicals};
use crate::joblog;
use crate::time::Timestamp;

/// The most a single scan reads; the rest waits for the next.
const MAX_READ: u64 = 4 * 1024 * 1024;
/// The longest unfinished line kept between scans.
const MAX_PARTIAL: usize = 4096;
/// The most commands followed in flight at once; past this, new `::start`s are ignored
/// until some finish, so a job printing endless unique markers costs nothing.
const MAX_ACTIVE: usize = 32;
/// The most notes a run keeps, and the longest each may be; later ones are ignored.
pub(crate) const MAX_NOTES: usize = 8;
const MAX_NOTE_CHARS: usize = 60;
/// Lines the daemon writes into a job's log itself.
pub(crate) const OWN_LINE_PREFIX: &str = crate::log::NOTE_PREFIX;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct Step {
    pub(crate) n: u64,
    /// 0 when the job did not say how many steps there are.
    pub(crate) of: u64,
    pub(crate) text: String,
}

/// What the log says so far.
#[derive(Debug, Default, Clone)]
pub(crate) struct Activity {
    offset: u64,
    partial: String,
    step: Option<Step>,
    /// Commands started and not yet done, in the order they started.
    active: Vec<(String, Timestamp)>,
    last: Option<String>,
    /// `::note` badges, each once, in the order printed.
    notes: Vec<String>,
}

/// One command in flight, as the dashboard shows it.
#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct CommandView {
    pub(crate) label: String,
    pub(crate) elapsed: u64,
    pub(crate) typical: Option<u64>,
}

#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct ActivityView {
    pub(crate) step: Option<Step>,
    pub(crate) active: Vec<CommandView>,
    pub(crate) last: Option<String>,
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
        } else if chars.peek() == Some(&'[') {
            chars.next();
            // A CSI sequence runs to its final byte, a letter.
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// The last `max` bytes of `s`, cut at a character boundary.
fn tail(s: &str, max: usize) -> &str {
    let mut start = s.len().saturating_sub(max);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

fn shorten(s: &str) -> String {
    s.trim().chars().take(160).collect()
}

fn group<'h>(c: &regex::Captures<'h>, name: &str) -> &'h str {
    c.name(name).map_or("", |m| m.as_str().trim())
}

impl Activity {
    /// Reads what the log gained since the last scan. Returns the commands that finished
    /// (`::done`) in it, with their durations, for the caller to learn from.
    pub(crate) fn scan(&mut self, log: &Path, patterns: &Patterns, now: Timestamp) -> Vec<StepRow> {
        let Some(joblog::Gained { bytes, offset, replaced }) = joblog::read_from(log, self.offset, MAX_READ) else {
            return Vec::new();
        };
        if replaced {
            *self = Activity::default();
        }
        self.offset = offset;
        if bytes.is_empty() {
            return Vec::new();
        }
        let text = format!("{}{}", self.partial, String::from_utf8_lossy(&bytes));
        let mut lines: Vec<&str> = text.split(['\n', '\r']).collect();
        self.partial = tail(lines.pop().unwrap_or_default(), MAX_PARTIAL).to_string();
        let mut finished = Vec::new();
        for raw in lines {
            let line = strip_ansi(raw);
            let line = line.trim_end();
            if !line.trim().is_empty() && !line.starts_with(OWN_LINE_PREFIX) {
                finished.extend(self.take_line(line, patterns, now));
            }
        }
        finished
    }

    fn take_line(&mut self, line: &str, patterns: &Patterns, now: Timestamp) -> Option<StepRow> {
        self.last = Some(shorten(line));
        if let Some(c) = patterns.note.as_ref().and_then(|re| re.captures(line)) {
            let text: String = group(&c, "text").chars().take(MAX_NOTE_CHARS).collect();
            if !text.is_empty() && self.notes.len() < MAX_NOTES && !self.notes.contains(&text) {
                self.notes.push(text);
            }
        } else if let Some(c) = patterns.step.as_ref().and_then(|re| re.captures(line)) {
            let number = |name| group(&c, name).parse().unwrap_or(0);
            self.step = Some(Step { n: number("n"), of: number("of"), text: group(&c, "text").to_string() });
        } else if let Some(c) = patterns.done.as_ref().and_then(|re| re.captures(line)) {
            let label = group(&c, "label");
            let pos = self.active.iter().position(|(l, _)| l == label)?;
            let (label, started) = self.active.remove(pos);
            return Some(StepRow { finished_at: now, label, seconds: started.until(now).as_secs() });
        } else if let Some(c) = patterns.start.as_ref().and_then(|re| re.captures(line)) {
            let label = group(&c, "label");
            if !label.is_empty() && self.active.len() < MAX_ACTIVE && !self.active.iter().any(|(l, _)| l == label) {
                self.active.push((label.to_string(), now));
            }
        }
        None
    }

    /// The `::note` badges so far.
    pub(crate) fn notes(&self) -> &[String] {
        &self.notes
    }

    pub(crate) fn view(&self, typicals: &Typicals) -> Option<ActivityView> {
        if self.step.is_none() && self.active.is_empty() && self.last.is_none() {
            return None;
        }
        let active = self
            .active
            .iter()
            .map(|(label, started)| CommandView {
                label: label.clone(),
                elapsed: started.elapsed().as_secs(),
                typical: typicals.typical(label).map(|d: Duration| d.as_secs()),
            })
            .collect();
        Some(ActivityView { step: self.step.clone(), active, last: self.last.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_with(name: &str, text: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("drmanhattan-scan-{}-{name}.log", std::process::id()));
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn carriage_returns_end_lines_and_the_buffer_stays_small() {
        let progress: String = (0..200_000).map(|i| format!("progress {i}%\r")).collect();
        let log = log_with("cr", &progress);
        let mut a = Activity::default();
        while a.offset < progress.len() as u64 {
            a.scan(&log, Patterns::defaults(), Timestamp::now());
        }
        assert!(a.partial.len() <= MAX_PARTIAL);
        assert_eq!(a.last.as_deref(), Some("progress 199999%"));
        std::fs::write(&log, "x".repeat(1_000_000)).unwrap();
        let mut b = Activity::default();
        b.scan(&log, Patterns::defaults(), Timestamp::now());
        assert!(b.partial.len() <= MAX_PARTIAL, "output with no line break at all is capped too");
        let _ = std::fs::remove_file(&log);
    }

    #[test]
    fn markers_set_the_step_and_the_commands_in_flight() {
        let log = log_with(
            "markers",
            "::step 2/5 Build\n::note on the spare\n::start cc\n::start ld\n::done cc\n::note on the spare\n\x1b[1mwarming up\x1b[0m\n",
        );
        let mut a = Activity::default();
        let finished = a.scan(&log, Patterns::defaults(), Timestamp::now());
        assert_eq!(a.step, Some(Step { n: 2, of: 5, text: "Build".into() }));
        assert_eq!(a.active.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(), ["ld"]);
        assert_eq!(a.last.as_deref(), Some("warming up"), "colour codes are stripped");
        assert_eq!(a.notes(), ["on the spare"], "a note is kept once");
        assert_eq!(finished.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), ["cc"]);
        std::io::Write::write_all(
            &mut std::fs::OpenOptions::new().append(true).open(&log).unwrap(),
            b"::step 3 Running\n",
        )
        .unwrap();
        a.scan(&log, Patterns::defaults(), Timestamp::now());
        assert_eq!(a.step, Some(Step { n: 3, of: 0, text: "Running".into() }), "the total is optional");
        let flood: String = (0..1000).map(|i| format!("::start job{i}\n")).collect();
        std::fs::write(&log, flood).unwrap();
        let mut b = Activity::default();
        b.scan(&log, Patterns::defaults(), Timestamp::now());
        assert_eq!(b.active.len(), MAX_ACTIVE);
        let _ = std::fs::remove_file(&log);
    }
}
