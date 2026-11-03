//! Pseudo-terminals: one to capture the login shell's environment, and one per `tty` job.
//! The child gets the terminal as stdout, stderr and controlling terminal, in a session of
//! its own; stdin stays at EOF. Output processing is off, so the log reads as from a pipe.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;

/// Every terminal's size: wide enough that tools do not wrap their progress lines.
const COLUMNS: u16 = 120;
const ROWS: u16 = 40;

pub(crate) struct Pty {
    pub(crate) master: File,
    slave: OwnedFd,
}

/// Opens a terminal with the fixed window size and output processing off.
pub(crate) fn open() -> std::io::Result<Pty> {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize { ws_row: ROWS, ws_col: COLUMNS, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: openpty writes two fresh descriptors, which we own from here on. The size is
    // `*mut` on macOS and `*const` on Linux; a raw pointer suits both.
    if unsafe {
        libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::addr_of_mut!(size))
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just opened and nothing else owns them.
    let (master, slave) = unsafe { (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: plain fcntl on descriptors we own; children get the slave through dup2.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: termios is plain data, filled by tcgetattr before use.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(slave.as_raw_fd(), &mut t) == 0 {
            t.c_oflag &= !libc::OPOST;
            libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &t);
        }
    }
    Ok(Pty { master, slave })
}

impl Pty {
    /// Gives `command` this terminal: stdout and stderr, and its controlling terminal in a
    /// new session, which also makes the child its own process group's leader.
    pub(crate) fn attach(&self, command: &mut Command) -> std::io::Result<()> {
        command.stdin(Stdio::null());
        command.stdout(Stdio::from(self.slave.try_clone()?));
        command.stderr(Stdio::from(self.slave.try_clone()?));
        // SAFETY: setsid and ioctl are async-signal-safe; fd 1 is the slave by now.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(1, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }

    /// Copies everything the terminal shows into `out` until every process holding it has
    /// gone. Consumes the parent's slave, so the copy sees the end.
    pub(crate) fn copy_to(self, mut out: impl Write + Send + 'static) -> JoinHandle<()> {
        let Pty { mut master, slave } = self;
        drop(slave);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            // Ends with EIO once the last holder closes the slave.
            while let Ok(n @ 1..) = master.read(&mut buf) {
                if out.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        })
    }
}
