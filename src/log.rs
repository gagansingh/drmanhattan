//! What drmanhattan prints on the terminal.
//!
//! The daemon prints one line per event, in columns, so a run can be followed by eye and
//! grepped by its id:
//!
//! ```text
//! 14:52:34  queued     sleep 20              #094a70  sleep · ~/code/app · run 1793649754-96609-094a70
//! 14:52:34  started    sleep 20              #094a70  slot 1 · waited 0s
//! 14:52:54  succeeded  sleep 20              #094a70  20s
//! ```
//!
//! The event word is coloured when stderr is a terminal (and `NO_COLOR` is not set). The
//! daemon also keeps these lines, in plain text, in `drmanhattan.log` in the data directory
//! (`log_to_file`), which is rotated at 5 MB with one older copy kept: a service that runs
//! for months never fills the disk with its own output. Other commands print plain
//! messages: `drmanhattan ·` notes, and `drmanhattan: error:` for a failure that ends the
//! command.
//!
//! A write error is ignored rather than raised: `eprintln!` panics when stderr is a closed
//! pipe, and a daemon whose terminal went away must keep scheduling.

use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::job::RunId;
use crate::time::Timestamp;

/// The prefix of every line drmanhattan writes into a job's log or prints for `submit`.
pub(crate) const NOTE_PREFIX: &str = "drmanhattan · ";

/// What an event means, which decides its colour.
#[derive(Clone, Copy)]
pub(crate) enum Tone {
    /// Bookkeeping: queued, requeued.
    Quiet,
    /// Something began: started.
    Start,
    Good,
    Bad,
    Warning,
}

const LABEL_WIDTH: usize = 20;

fn wants_colour(terminal: bool) -> bool {
    terminal && std::env::var_os("NO_COLOR").is_none() && std::env::var("TERM").map_or(true, |t| t != "dumb")
}

/// Whether stderr, where the daemon's lines and the notes go, is coloured.
fn colour() -> bool {
    static COLOUR: OnceLock<bool> = OnceLock::new();
    *COLOUR.get_or_init(|| wants_colour(std::io::stderr().is_terminal()))
}

/// Whether stdout, where `submit --wait` copies the job's log, is coloured.
pub(crate) fn stdout_colour() -> bool {
    static COLOUR: OnceLock<bool> = OnceLock::new();
    *COLOUR.get_or_init(|| wants_colour(std::io::stdout().is_terminal()))
}

/// One of drmanhattan's own lines (`drmanhattan · …`) set apart from the command's output
/// around it: the prefix in cyan and the rest dimmed. Any other line is returned as it is.
pub(crate) fn mark_own_line(line: &str, colour: bool) -> std::borrow::Cow<'_, str> {
    match line.strip_prefix(NOTE_PREFIX) {
        Some(rest) if colour => format!("\x1b[36m{}\x1b[0m \x1b[2m{rest}\x1b[0m", NOTE_PREFIX.trim_end()).into(),
        _ => line.into(),
    }
}

fn paint(text: &str, code: &str) -> String {
    if colour() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn write_line(line: std::fmt::Arguments) {
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// The daemon's own log file, once `log_to_file` has opened it.
struct LogFile {
    path: PathBuf,
    file: File,
    size: u64,
}

static LOG_FILE: Mutex<Option<LogFile>> = Mutex::new(None);
/// Past this size the log file becomes `drmanhattan.log.1`, replacing any older one.
const LOG_FILE_LIMIT: u64 = 5 * 1024 * 1024;

fn open_log_file(path: &Path) -> std::io::Result<LogFile> {
    let file = OpenOptions::new().append(true).create(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)?;
    Ok(LogFile { size: file.metadata()?.len(), path: path.to_path_buf(), file })
}

/// From now on, the daemon's lines are also appended to `path`. Best effort: a log file
/// that cannot be opened leaves the terminal output as it is.
pub(crate) fn log_to_file(path: &Path) {
    match open_log_file(path) {
        Ok(log) => *crate::sync::lock(&LOG_FILE) = Some(log),
        Err(e) => write_line(format_args!("drmanhattan: warning: cannot keep a log in {}: {e}", path.display())),
    }
}

/// `line` without its colour codes.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("\x1b[") {
        out.push_str(&rest[..start]);
        rest = rest[start..].find('m').map_or("", |end| &rest[start + end + 1..]);
    }
    out + rest
}

/// One of the daemon's lines: to the terminal as it is, and to the log file without colour.
fn daemon_line(line: &str) {
    write_line(format_args!("{line}"));
    let mut guard = crate::sync::lock(&LOG_FILE);
    let Some(log) = guard.as_mut() else { return };
    let text = format!("{}\n", plain(line));
    if log.file.write_all(text.as_bytes()).is_ok() {
        log.size += text.len() as u64;
    }
    if log.size > LOG_FILE_LIMIT {
        let mut old = log.path.clone().into_os_string();
        old.push(".1");
        if std::fs::rename(&log.path, &old).is_ok() {
            // If a new file cannot be opened, stop writing rather than grow the old one.
            *guard = open_log_file(&log.path.clone()).ok();
        }
    }
}

/// The run id's last part (`#094a70`): enough to tell runs apart on screen.
fn short_id(id: &RunId) -> String {
    format!("#{}", id.as_str().rsplit('-').next().unwrap_or_default())
}

/// `label` cut or padded to the label column's width.
fn fit(label: &str) -> String {
    let n = label.chars().count();
    if n > LABEL_WIDTH {
        let cut: String = label.chars().take(LABEL_WIDTH - 1).collect();
        format!("{cut}…")
    } else {
        format!("{label}{}", " ".repeat(LABEL_WIDTH - n))
    }
}

/// One daemon event about a run (or a job file, when `id` is None): the time, `word`, the
/// run's label and short id, then `details` joined by ` · `.
pub(crate) fn event(tone: Tone, word: &str, label: &str, id: Option<&RunId>, details: &[String]) {
    let code = match tone {
        Tone::Quiet => "2",
        Tone::Start => "34",
        Tone::Good => "32",
        Tone::Bad => "31",
        Tone::Warning => "33",
    };
    let id = id.map(short_id).unwrap_or_default();
    let details = details.iter().filter(|d| !d.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
    let line = format!(
        "{}  {}  {} {}  {}",
        Timestamp::now().local_time(),
        paint(&format!("{word:<9}"), code),
        fit(label),
        paint(&format!("{id:<7}"), "2"),
        details
    );
    daemon_line(line.trim_end());
}

/// A daemon event that is about no run in particular: `14:52:34  stopping   …`.
pub(crate) fn notice(tone: Tone, word: &str, message: impl Display) {
    let code = match tone {
        Tone::Warning => "33",
        Tone::Bad => "31",
        Tone::Good => "32",
        Tone::Start => "34",
        Tone::Quiet => "2",
    };
    let line = format!("{}  {}  {message}", Timestamp::now().local_time(), paint(&format!("{word:<9}"), code));
    daemon_line(line.trim_end());
}

/// A daemon problem that does not stop anything.
pub(crate) fn problem(message: impl Display) {
    notice(Tone::Warning, "warning", message);
}

/// The daemon's start-up summary: its first line in bold, the rest as they are, then a
/// blank line before the events.
pub(crate) fn banner(lines: &[String]) {
    for (i, line) in lines.iter().enumerate() {
        daemon_line(&if i == 0 { paint(line, "1") } else { line.clone() });
    }
    daemon_line("");
}

/// A note from a command such as `submit`: `drmanhattan · …`.
pub(crate) fn note(message: impl Display) {
    write_line(format_args!("{}", mark_own_line(&format!("{NOTE_PREFIX}{message}"), colour())));
}

/// Something the user should know about but that does not stop the command.
pub(crate) fn warn(message: impl Display) {
    daemon_line(&format!("drmanhattan: warning: {message}"));
}

/// A failure that ends the command.
/// It also goes to the daemon's log file when that is open, so a service that fails to
/// start (which has no terminal) says why.
pub(crate) fn error(message: impl Display) {
    daemon_line(&format!("drmanhattan: error: {message}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_fill_their_column_exactly() {
        assert_eq!(fit("sleep 20").chars().count(), LABEL_WIDTH);
        let long = fit("an extremely long label that goes on");
        assert_eq!(long.chars().count(), LABEL_WIDTH);
        assert!(long.ends_with('…'));
        assert_eq!(short_id(&"1793649754-96609-094a70".parse().unwrap()), "#094a70");
        assert_eq!(plain("\x1b[32msucceeded\x1b[0m  x"), "succeeded  x");
    }
}
