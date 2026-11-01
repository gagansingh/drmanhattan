//! CPU usage: cumulative busy and total ticks, read twice and differenced.

/// (busy, total) cumulative CPU ticks from host_statistics(HOST_CPU_LOAD_INFO).
#[cfg(target_os = "macos")]
pub(crate) fn ticks() -> Option<(u64, u64)> {
    extern "C" {
        fn mach_host_self() -> u32;
        fn host_statistics(host: u32, flavor: i32, info: *mut u32, count: *mut u32) -> i32;
    }
    const HOST_CPU_LOAD_INFO: i32 = 3;
    // Each mach_host_self() call takes a new port reference; take one and keep it.
    static HOST: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    // SAFETY: mach_host_self has no preconditions.
    let host = *HOST.get_or_init(|| unsafe { mach_host_self() });
    let mut ticks = [0u32; 4]; // user, system, idle, nice
    let mut count: u32 = 4;
    // SAFETY: `ticks` has room for the 4 counters HOST_CPU_LOAD_INFO writes; `count` says so.
    if unsafe { host_statistics(host, HOST_CPU_LOAD_INFO, ticks.as_mut_ptr(), &mut count) } != 0 {
        return None;
    }
    let [user, system, idle, nice] = ticks.map(u64::from);
    Some((user + system + nice, user + system + idle + nice))
}

/// Linux: the first line of /proc/stat; idle and iowait count as not busy.
#[cfg(not(target_os = "macos"))]
pub(crate) fn ticks() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let vals: Vec<u64> = text.lines().next()?.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if vals.len() < 4 {
        return None;
    }
    let total: u64 = vals.iter().take(8).sum();
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0);
    Some((total.saturating_sub(idle), total))
}

/// CPU busy percentage between two tick readings. macOS's counters are 32-bit and wrap,
/// hence the wrapping subtraction there.
pub(crate) fn percent(prev: (u64, u64), now: (u64, u64)) -> Option<u32> {
    let delta = |a: u64, b: u64| {
        if cfg!(target_os = "macos") {
            u64::from((a as u32).wrapping_sub(b as u32))
        } else {
            a.saturating_sub(b)
        }
    };
    let (busy, total) = (delta(now.0, prev.0), delta(now.1, prev.1));
    (total > 0).then(|| ((100 * busy + total / 2) / total).min(100) as u32)
}
