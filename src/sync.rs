//! Locking that survives a panic elsewhere.

use std::sync::{Mutex, MutexGuard};

/// Locks `m`, recovering it if a thread panicked while holding it. Every structure behind
/// these locks stays consistent between statements, so the data is still good.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
