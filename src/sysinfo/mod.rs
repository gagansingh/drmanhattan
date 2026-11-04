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

use std::sync::Mutex;
use std::time::Duration;

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
        std::thread::sleep(Duration::from_millis(500));
        loop {
            let now = cpu::ticks();
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
