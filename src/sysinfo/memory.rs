//! Memory as Activity Monitor reports it, read from the kernel directly.
//!
//! No child process is started (as `vm_stat` or `sysctl` would be): on macOS a pipe opened
//! for one can leak into a job spawned at the same moment and then hold the read open for
//! as long as that job runs.

use std::ffi::CStr;
use std::mem::{size_of, zeroed};

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub(crate) struct Memory {
    /// Bytes.
    pub(crate) total: u64,
    pub(crate) used: u64,
    pub(crate) swap_used: f64,
    pub(crate) swap_total: f64,
    /// The kernel's memory-pressure level: `normal`, `warn` or `critical`.
    pub(crate) pressure: &'static str,
}

/// A fixed-size value from `sysctlbyname`. `name` ends with a NUL byte.
fn sysctl<T: Copy>(name: &[u8]) -> Option<T> {
    let name = CStr::from_bytes_with_nul(name).ok()?;
    // SAFETY: `value` is a plain-old-data type the kernel fills with `size` bytes at most,
    // and the kernel reports the size it wrote, which must be exactly that of `T`.
    unsafe {
        let mut value: T = zeroed();
        let mut size = size_of::<T>();
        let ok =
            libc::sysctlbyname(name.as_ptr(), (&mut value as *mut T).cast(), &mut size, std::ptr::null_mut(), 0) == 0;
        (ok && size == size_of::<T>()).then_some(value)
    }
}

/// The page counts of `vm_stat`.
fn vm_statistics() -> Option<libc::vm_statistics64> {
    // SAFETY: the buffer is a `vm_statistics64` and `count` says how many 32-bit words it
    // holds, which is what HOST_VM_INFO64 writes at most.
    unsafe {
        let mut stats: libc::vm_statistics64 = zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // Each mach_host_self() call adds a reference to the port: take one and keep it.
        static HOST: std::sync::OnceLock<libc::mach_port_t> = std::sync::OnceLock::new();
        #[allow(deprecated)] // libc marks mach_host_self deprecated in favour of the mach2 crate
        let host = *HOST.get_or_init(|| libc::mach_host_self());
        let ok = libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            (&mut stats as *mut libc::vm_statistics64).cast(),
            &mut count,
        );
        (ok == libc::KERN_SUCCESS).then_some(stats)
    }
}

/// Activity Monitor's figures: used = wired + compressed + app memory (anonymous pages
/// less purgeable), swap from vm.swapusage, and the kernel's memory-pressure level.
pub(crate) fn read() -> Option<Memory> {
    let total: u64 = sysctl(b"hw.memsize\0")?;
    let swap: Option<libc::xsw_usage> = sysctl(b"vm.swapusage\0");
    let vm = vm_statistics()?;
    // SAFETY: sysconf has no preconditions.
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    let pages = |n: libc::natural_t| u64::from(n);
    let used = (pages(vm.wire_count) + pages(vm.compressor_page_count) + pages(vm.internal_page_count))
        .saturating_sub(pages(vm.purgeable_count))
        * page;
    Some(Memory {
        total,
        used: used.min(total),
        swap_used: swap.map_or(0.0, |s| s.xsu_used as f64),
        swap_total: swap.map_or(0.0, |s| s.xsu_total as f64),
        pressure: pressure(),
    })
}

/// The kernel's memory-pressure level: `normal`, `warn` or `critical`.
pub(crate) fn pressure() -> &'static str {
    match sysctl::<i32>(b"kern.memorystatus_vm_pressure_level\0") {
        Some(2) => "warn",
        Some(4) => "critical",
        _ => "normal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kernel_reports_plausible_memory() {
        let m = read().expect("memory figures on macOS");
        assert!(m.total > 1 << 30, "{m:?}");
        assert!(m.used > 0 && m.used <= m.total, "{m:?}");
        assert!(m.swap_used <= m.swap_total, "{m:?}");
        eprintln!(
            "used {:.1} of {:.1} GB, swap {:.1} GB",
            m.used as f64 / 1e9,
            m.total as f64 / 1e9,
            m.swap_used / 1e9
        );
    }
}
