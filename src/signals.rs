//! Counting signals. A handler only counts, and the program notices on its next check:
//! doing anything more inside a signal handler is unsafe.

use std::sync::atomic::{AtomicUsize, Ordering};

static STOPS: AtomicUsize = AtomicUsize::new(0);
static RELOADS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_stop(_: libc::c_int) {
    STOPS.fetch_add(1, Ordering::SeqCst);
}

extern "C" fn on_reload(_: libc::c_int) {
    RELOADS.fetch_add(1, Ordering::SeqCst);
}

/// Counts `signals` as stop signals from now on, instead of their default action.
pub(crate) fn count(signals: &[libc::c_int]) {
    for &signal in signals {
        // SAFETY: the handler is async-signal-safe: it only increments an atomic counter.
        unsafe {
            libc::signal(signal, on_stop as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
}

/// How many stop signals have arrived.
pub(crate) fn received() -> usize {
    STOPS.load(Ordering::SeqCst)
}

/// Counts `SIGUSR1`, which `drmanhattan reload-env` sends, from now on.
pub(crate) fn count_reloads() {
    // SAFETY: as in `count`.
    unsafe {
        libc::signal(libc::SIGUSR1, on_reload as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
}

/// How many reload signals have arrived.
pub(crate) fn reloads_received() -> usize {
    RELOADS.load(Ordering::SeqCst)
}
