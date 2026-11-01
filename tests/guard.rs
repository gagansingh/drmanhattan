//! The guard beside each running job (`drmanhattan __guard`), driven directly.

mod common;

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::{bin, group_exists, wait_for};

const T: Duration = Duration::from_secs(10);

/// A process group of its own, standing in for a job.
fn job() -> Child {
    Command::new("sleep").arg("60").process_group(0).spawn().unwrap()
}

#[test]
fn done_stops_the_guard_even_while_the_pipe_stays_open() {
    let mut job = job();
    let me = std::process::id().to_string();
    let mut guard =
        Command::new(bin()).args(["__guard", &job.id().to_string(), "500", &me]).stdin(Stdio::piped()).spawn().unwrap();
    // Write `done` but keep our end open, as a leaked copy elsewhere would.
    let mut stdin = guard.stdin.take().unwrap();
    writeln!(stdin, "done").unwrap();
    let exited = wait_for(T, || guard.try_wait().unwrap());
    assert_eq!(exited.and_then(|s| s.code()), Some(0), "the guard exits on `done`");
    assert!(group_exists(job.id() as i32), "and leaves the job alone");
    drop(stdin);
    let _ = job.kill();
    let _ = job.wait();
}

#[test]
fn the_guard_ends_its_job_when_its_parent_dies_even_with_the_pipe_open() {
    let mut job = job();
    let pgid = job.id() as i32;
    // The guard's parent is a shell that exits after a moment, standing in for a daemon
    // that dies. The guard's stdin (explicitly the shell's, not /dev/null) stays open on our
    // side throughout, so only the parent's death can set it off.
    let script = format!("'{}' __guard {pgid} 500 $$ <&0 & sleep 1", bin().display());
    let mut parent = Command::new("sh").args(["-c", &script]).stdin(Stdio::piped()).spawn().unwrap();
    let held_open = parent.stdin.take();
    let _ = parent.wait();
    // The job is our child, so it is reaped here rather than checked by its group.
    let ended = wait_for(T, || job.try_wait().unwrap());
    assert!(ended.is_some(), "the guard ends the job in group {pgid}");
    drop(held_open);
}
