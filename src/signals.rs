//! Counting stop signals. The handler only counts, and the program notices on its next
//! check: doing anything more inside a signal handler is unsafe.

use std::sync::atomic::{AtomicUsize, Ordering};

static RECEIVED: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_signal(_: libc::c_int) {
    RECEIVED.fetch_add(1, Ordering::SeqCst);
}

/// Counts `signals` from now on, instead of their default action.
pub(crate) fn count(signals: &[libc::c_int]) {
    for &signal in signals {
        // SAFETY: the handler is async-signal-safe: it only increments an atomic counter.
        unsafe {
            libc::signal(signal, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
}

/// How many counted signals have arrived.
pub(crate) fn received() -> usize {
    RECEIVED.load(Ordering::SeqCst)
}
