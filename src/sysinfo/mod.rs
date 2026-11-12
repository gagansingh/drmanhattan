//! Machine readings for the dashboard header: CPU usage, memory, CPU temperature and the
//! battery.
//!
//! One background thread takes every reading every two seconds and caches it, so a page
//! poll never spawns a process or touches IOKit, however many pages are open. CPU usage
//! reads on macOS and Linux; memory, temperature and the battery are macOS-only and need no
//! sudo or extra tools. A reading that is unavailable is None and the page hides its chip.

#[cfg(target_os = "macos")]
mod battery;
mod cpu;
#[cfg(target_os = "macos")]
mod memory;
#[cfg(target_os = "macos")]
mod thermals;

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

#[cfg(target_os = "macos")]
pub(crate) use battery::Battery;
#[cfg(target_os = "macos")]
pub(crate) use memory::Memory;
#[cfg(target_os = "macos")]
pub(crate) use thermals::Thermals;

#[cfg(not(target_os = "macos"))]
pub(crate) type Battery = ();
#[cfg(not(target_os = "macos"))]
pub(crate) type Memory = ();
#[cfg(not(target_os = "macos"))]
pub(crate) type Thermals = ();

/// The kernel's memory-pressure level now: `normal`, `warn` or `critical`. None where it has
/// none to give (anywhere but macOS).
pub(crate) fn memory_pressure() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    return Some(memory::pressure());
    #[cfg(not(target_os = "macos"))]
    None
}

/// The latest readings.
#[derive(Serialize, Default, Clone, Debug)]
pub(crate) struct Snapshot {
    pub(crate) cpu_pct: Option<u32>,
    pub(crate) memory: Option<Memory>,
    pub(crate) thermals: Option<Thermals>,
    pub(crate) battery: Option<Battery>,
}

static SNAPSHOT: Mutex<Snapshot> = Mutex::new(Snapshot { cpu_pct: None, memory: None, thermals: None, battery: None });

pub(crate) fn snapshot() -> Snapshot {
    crate::sync::lock(&SNAPSHOT).clone()
}

/// How far back the sampler's CPU readings go: the longest window `cpu_busy_over` answers.
pub(crate) const CPU_HISTORY: Duration = Duration::from_secs(300);

/// The sampler's CPU tick readings, oldest first, for `CPU_HISTORY` back.
static CPU_TICKS: Mutex<VecDeque<(Instant, (u64, u64))>> = Mutex::new(VecDeque::new());

fn remember_ticks(at: Instant, ticks: (u64, u64)) {
    let mut readings = crate::sync::lock(&CPU_TICKS);
    readings.push_back((at, ticks));
    // One reading older than the history stays, so a window of all of it has its start.
    while readings.len() > 2 && readings.get(1).is_some_and(|(t, _)| at.duration_since(*t) >= CPU_HISTORY) {
        readings.pop_front();
    }
}

/// The CPU's busy share, in percent, over the last `window`: from the sampler's reading of
/// that long ago to one taken now. None without a sampler, where the machine gives no
/// reading, and until the sampler's readings go back at least half the window (a daemon
/// that has just started).
pub(crate) fn cpu_busy_over(window: Duration) -> Option<u32> {
    let now = (Instant::now(), cpu::ticks()?);
    busy_over(&crate::sync::lock(&CPU_TICKS), now, window)
}

fn busy_over(
    readings: &VecDeque<(Instant, (u64, u64))>,
    (now, ticks): (Instant, (u64, u64)),
    window: Duration,
) -> Option<u32> {
    // The newest reading at least the window old, else the oldest there is.
    let (at, then) = *readings.iter().rev().find(|(t, _)| now.duration_since(*t) >= window).or(readings.front())?;
    (now.duration_since(at) >= window / 2).then(|| cpu::percent(then, ticks)).flatten()
}

/// Readings taken now, for a process that has no sampler running: CPU usage over `window`.
pub(crate) fn read_now(window: Duration) -> Snapshot {
    let before = cpu::ticks();
    std::thread::sleep(window);
    let cpu_pct = before.zip(cpu::ticks()).and_then(|(p, n)| cpu::percent(p, n));
    #[cfg(target_os = "macos")]
    let (memory, thermals, battery) =
        (memory::read(), thermals::Sensors::new().as_ref().and_then(thermals::Sensors::read), battery::read());
    #[cfg(not(target_os = "macos"))]
    let (memory, thermals, battery) = (None, None, None);
    Snapshot { cpu_pct, memory, thermals, battery }
}

/// Starts the sampler: a first CPU figure half a second after start, then every reading
/// every two seconds.
pub(crate) fn start_sampler() {
    let _ = std::thread::Builder::new().name("sampler".into()).spawn(|| {
        #[cfg(target_os = "macos")]
        let sensors = thermals::Sensors::new();
        let mut prev = cpu::ticks();
        if let Some(ticks) = prev {
            remember_ticks(Instant::now(), ticks);
        }
        std::thread::sleep(Duration::from_millis(500));
        loop {
            let now = cpu::ticks();
            if let Some(ticks) = now {
                remember_ticks(Instant::now(), ticks);
            }
            let cpu_pct = prev.zip(now).and_then(|(p, n)| cpu::percent(p, n));
            prev = now.or(prev);
            #[cfg(target_os = "macos")]
            let (memory, thermals, battery) =
                (memory::read(), sensors.as_ref().and_then(thermals::Sensors::read), battery::read());
            #[cfg(not(target_os = "macos"))]
            let (memory, thermals, battery) = (None, None, None);
            {
                let mut s = crate::sync::lock(&SNAPSHOT);
                s.cpu_pct = cpu_pct.or(s.cpu_pct);
                s.memory = memory;
                s.thermals = thermals;
                s.battery = battery;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_busy_share_is_taken_from_the_reading_a_window_ago() {
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        // Idle for ten seconds, then every tick busy: (busy, total) ticks, 100 a second.
        let readings: VecDeque<_> =
            [(0, (0, 0)), (2, (0, 200)), (4, (0, 400)), (6, (0, 600)), (8, (0, 800)), (10, (0, 1000))]
                .map(|(secs, ticks)| (at(secs), ticks))
                .into();
        let over = |now_secs, ticks, window| busy_over(&readings, (at(now_secs), ticks), Duration::from_secs(window));
        assert_eq!(over(11, (100, 1100), 5), Some(20), "from the reading at 6 s: 100 busy of 500");
        assert_eq!(over(14, (400, 1400), 4), Some(100), "from the reading at 10 s");
        assert_eq!(over(14, (400, 1400), 20), Some(29), "all there is, when it is half the window or more");
        assert_eq!(over(14, (400, 1400), 300), None, "too little to speak for the window");
        assert_eq!(busy_over(&VecDeque::new(), (at(1), (1, 1)), Duration::from_secs(1)), None);
    }
}
