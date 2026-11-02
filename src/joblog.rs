//! A job's log: kept to a size by cutting its start, and read as it grows.
//!
//! A cut rewrites the log as a note line followed by its newest bytes. The note says how
//! many bytes of output were dropped in all, so readers (`submit --wait`, the activity
//! scanner) count in output bytes rather than file positions: output byte `n` is at
//! `note + n - dropped` in the file. A reader keeps its place across any number of cuts,
//! and one that fell behind what was kept resumes at the note.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::log::NOTE_PREFIX;

const NOTE_START: &str = "the log passed ";
const DROPPED_BEFORE: &str = "so its first ";
const DROPPED_AFTER: &str = " bytes were dropped";

fn note(limit: u64, dropped: u64) -> String {
    format!(
        "{NOTE_PREFIX}{NOTE_START}{} MB, {DROPPED_BEFORE}{dropped}{DROPPED_AFTER}; the newest output follows\n",
        limit >> 20
    )
}

/// Where a log's output starts: how many bytes of it were dropped, and the length of the
/// note that says so (both 0 for a log never cut).
#[derive(Clone, Copy, Default)]
struct Cut {
    dropped: u64,
    note: u64,
}

impl Cut {
    fn of(file: &mut File) -> std::io::Result<Cut> {
        let mut head = Vec::new();
        file.seek(SeekFrom::Start(0))?;
        file.take(512).read_to_end(&mut head)?;
        Ok(read_note(&head).map_or_else(Cut::default, |(dropped, note)| Cut { dropped, note }))
    }
}

/// The bytes a cut dropped and the length of its note, when `head` starts with a note.
fn read_note(head: &[u8]) -> Option<(u64, u64)> {
    let end = head.iter().position(|b| *b == b'\n')?;
    let line = std::str::from_utf8(&head[..end]).ok()?.strip_prefix(NOTE_PREFIX)?.strip_prefix(NOTE_START)?;
    let (_, rest) = line.split_once(DROPPED_BEFORE)?;
    let (dropped, _) = rest.split_once(DROPPED_AFTER)?;
    Some((dropped.parse().ok()?, end as u64 + 1))
}

/// Cuts a running job's log back once it passes `limit` bytes, so one runaway job cannot
/// fill the disk. The newest quarter of the limit, where errors usually are, is kept (from
/// its first whole line), after the note. The job writes in append mode, so its next line
/// lands after what was kept; only output written while the cut is being made can be lost.
pub(crate) fn keep_bounded(log: &Path, limit: u64) {
    let Ok(size) = std::fs::metadata(log).map(|m| m.len()) else { return };
    if size > limit {
        cut(log, size, limit);
    }
}

/// The cut, from where the newest quarter starts when the log was `size` bytes. The job
/// may have written more since; what it wrote is kept too.
fn cut(log: &Path, size: u64, limit: u64) {
    let kept = (|| -> std::io::Result<(Cut, u64, Vec<u8>)> {
        let mut f = File::open(log)?;
        let before = Cut::of(&mut f)?;
        let seek = size - limit / 4;
        f.seek(SeekFrom::Start(seek))?;
        let mut tail = Vec::new();
        f.read_to_end(&mut tail)?;
        // Start at a line: the cut point is usually in the middle of one.
        let start = tail.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
        Ok((before, seek + start as u64, tail.split_off(start)))
    })();
    let Ok((before, cut_at, tail)) = kept else { return };
    // Output dropped by earlier cuts, and by this one: the file up to `cut_at`, less the
    // earlier note.
    let dropped = before.dropped + cut_at.saturating_sub(before.note);
    let note = note(limit, dropped);
    let _ = OpenOptions::new().write(true).open(log).and_then(|f| f.set_len(0)).and_then(|()| {
        let mut f = OpenOptions::new().append(true).open(log)?;
        f.write_all(note.as_bytes())?;
        f.write_all(&tail)
    });
}

/// How many bytes of output the log holds now, counting any a cut dropped: where a reader
/// that wants only what comes next starts.
pub(crate) fn output_len(log: &Path) -> u64 {
    let Ok(mut f) = File::open(log) else { return 0 };
    let size = f.metadata().map_or(0, |m| m.len());
    let cut = Cut::of(&mut f).unwrap_or_default();
    cut.dropped + size.saturating_sub(cut.note)
}

/// What a read of the log gained.
#[derive(Debug, PartialEq)]
pub(crate) struct Gained {
    pub(crate) bytes: Vec<u8>,
    /// Where the next read starts, in bytes of output.
    pub(crate) offset: u64,
    /// The log was replaced by another, which is read from its start.
    pub(crate) replaced: bool,
}

/// The output appended to the log after its first `offset` bytes, at most `max_bytes` of
/// it. A reader whose place was cut away gets the note, then what was kept. A log that
/// holds less than the reader already had, with no cut to explain it, was replaced.
pub(crate) fn read_from(log: &Path, offset: u64, max_bytes: u64) -> Option<Gained> {
    let mut f = File::open(log).ok()?;
    let size = f.metadata().ok()?.len();
    let cut = Cut::of(&mut f).ok()?;
    let at = |offset: u64| cut.note + offset - cut.dropped;
    let (start, replaced) = if offset < cut.dropped {
        (0, false)
    } else if at(offset) <= size {
        (at(offset), false)
    } else if cut.dropped > 0 {
        (size, false) // output lost while the cut was made
    } else {
        (0, true)
    };
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    f.take(max_bytes).read_to_end(&mut bytes).ok()?;
    let end = start + bytes.len() as u64;
    Some(Gained { offset: cut.dropped + end.saturating_sub(cut.note), bytes, replaced })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_keeps_its_place_across_a_cut() {
        let log = std::env::temp_dir().join(format!("drmanhattan-joblog-{}.log", std::process::id()));
        let line = |n: usize| format!("line {n:07}\n");
        std::fs::write(&log, (0..400_000).map(line).collect::<String>()).unwrap();
        let read = |offset| read_from(&log, offset, u64::MAX).unwrap();
        let caught_up = read(0).offset;
        let behind = 1000;
        keep_bounded(&log, 1 << 20);
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.starts_with("drmanhattan · the log passed 1 MB, so its first "), "{}", &text[..120]);
        assert!(text.ends_with(&line(399_999)) && text.len() < (1 << 18) + 200);

        let after = read(caught_up);
        assert_eq!((after.bytes.len(), after.replaced), (0, false), "a reader that had it all reads nothing again");
        std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(b"new\n").unwrap();
        let new = read(after.offset);
        assert_eq!(new.bytes, b"new\n", "and then only what is new");

        let more = (400_000..800_000).map(line).collect::<String>();
        std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(more.as_bytes()).unwrap();
        let caught_up_again = read(new.offset).offset;
        keep_bounded(&log, 1 << 20);
        assert!(std::fs::read_to_string(&log).unwrap().ends_with(&line(799_999)));
        assert!(read(caught_up_again).bytes.is_empty(), "the place survives a second cut");

        // The job writes between the size check and the cut: what it wrote is kept, and a
        // reader that had everything before reads exactly that.
        let append =
            |text: &[u8]| std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(text).unwrap();
        append((800_000..900_000).map(line).collect::<String>().as_bytes());
        let had = read(caught_up_again).offset;
        let checked = std::fs::metadata(&log).unwrap().len();
        append(b"late\n");
        cut(&log, checked, 1 << 20);
        assert_eq!(read(had).bytes, b"late\n", "output written during the cut is not skipped");

        let late = read(behind);
        assert!(
            late.bytes.starts_with(NOTE_PREFIX.as_bytes()) && !late.replaced,
            "one that was behind starts at the note"
        );

        std::fs::write(&log, "other\n").unwrap();
        assert_eq!(read(caught_up), Gained { bytes: b"other\n".to_vec(), offset: 6, replaced: true });
        let _ = std::fs::remove_file(&log);
    }
}
