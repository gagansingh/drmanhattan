//! The battery, as the menu bar shows it: charge, whether it charges, time left and the
//! system's view of its health, from IOKit's power-source API. Like the other readings it
//! starts no child process (`pmset` would), and a Mac without a battery reads as None.

use std::ffi::{c_char, c_void, CStr, CString};

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub(crate) struct Battery {
    /// Charge, 0 to 100.
    pub(crate) percent: u32,
    /// Plugged in.
    pub(crate) on_power: bool,
    pub(crate) charging: bool,
    /// Minutes until empty while on battery, or until full while charging, when known.
    pub(crate) minutes: Option<u32>,
    /// The system's word for it, such as `Good` or `Check Battery`, when it gives one.
    pub(crate) health: Option<String>,
}

type CFTypeRef = *const c_void;
type CFIndex = isize;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, encoding: u32) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: CFIndex, encoding: u32) -> bool;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(number: CFTypeRef, kind: CFIndex, value: *mut c_void) -> bool;
    fn CFBooleanGetValue(boolean: CFTypeRef) -> bool;
    fn CFGetTypeID(t: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetCount(a: CFTypeRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: CFIndex) -> CFTypeRef;
    fn CFRelease(t: CFTypeRef);
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
    fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFTypeRef;
    fn IOPSGetPowerSourceDescription(blob: CFTypeRef, source: CFTypeRef) -> CFTypeRef;
}

const UTF8: u32 = 0x0800_0100;

/// A value of `dict` by key, borrowed from the dictionary: null when absent.
fn get(dict: CFTypeRef, key: &str) -> CFTypeRef {
    let Ok(c) = CString::new(key) else { return std::ptr::null() };
    // SAFETY: `c` is NUL-terminated; the key string is created, used and released here.
    unsafe {
        let k = CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8);
        if k.is_null() {
            return std::ptr::null();
        }
        let v = CFDictionaryGetValue(dict, k);
        CFRelease(k);
        v
    }
}

fn number(dict: CFTypeRef, key: &str) -> Option<i64> {
    let v = get(dict, key);
    let mut out: i64 = 0;
    // SAFETY: `v` is checked to be a CFNumber before it is read as a 64-bit integer
    // (kCFNumberSInt64Type, 4), which writes exactly one i64.
    (!v.is_null()
        && unsafe { CFGetTypeID(v) == CFNumberGetTypeID() }
        && unsafe { CFNumberGetValue(v, 4, &mut out as *mut i64 as *mut c_void) })
    .then_some(out)
}

fn boolean(dict: CFTypeRef, key: &str) -> Option<bool> {
    let v = get(dict, key);
    // SAFETY: `v` is checked to be a CFBoolean before it is read.
    (!v.is_null() && unsafe { CFGetTypeID(v) == CFBooleanGetTypeID() }).then(|| unsafe { CFBooleanGetValue(v) })
}

fn string(dict: CFTypeRef, key: &str) -> Option<String> {
    let v = get(dict, key);
    // SAFETY: `v` is checked to be a CFString; the buffer is large enough for these short
    // values, and CFStringGetCString NUL-terminates what it writes or fails.
    if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = [0 as c_char; 128];
    unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as CFIndex, UTF8) }
        .then(|| unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
}

/// The internal battery now, or None on a Mac without one.
pub(crate) fn read() -> Option<Battery> {
    // SAFETY: the info blob and the list are owned here and released at the end; the
    // descriptions are borrowed from the blob and used only before it is released.
    unsafe {
        let blob = IOPSCopyPowerSourcesInfo();
        if blob.is_null() {
            return None;
        }
        let list = IOPSCopyPowerSourcesList(blob);
        let mut found = None;
        if !list.is_null() {
            for i in 0..CFArrayGetCount(list) {
                let desc = IOPSGetPowerSourceDescription(blob, CFArrayGetValueAtIndex(list, i));
                if desc.is_null() || string(desc, "Type").as_deref() != Some("InternalBattery") {
                    continue;
                }
                found = describe(desc);
                break;
            }
            CFRelease(list);
        }
        CFRelease(blob);
        found
    }
}

fn describe(desc: CFTypeRef) -> Option<Battery> {
    let current = number(desc, "Current Capacity")?;
    let max = number(desc, "Max Capacity").filter(|m| *m > 0).unwrap_or(100);
    let percent = ((current * 100 + max / 2) / max).clamp(0, 100) as u32;
    let on_power = string(desc, "Power Source State").as_deref() == Some("AC Power");
    let charging = boolean(desc, "Is Charging").unwrap_or(false);
    // -1 while the system is still working it out.
    let minutes = number(desc, if charging { "Time to Full Charge" } else { "Time to Empty" })
        .filter(|m| *m > 0 && (charging || !on_power))
        .map(|m| m as u32);
    let health = string(desc, "BatteryHealth").or_else(|| string(desc, "BatteryHealthCondition"));
    Some(Battery { percent, on_power, charging, minutes, health })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_battery_reads_sensibly_or_not_at_all() {
        // A Mac without a battery (or a CI runner) reads None; one with a battery, a charge.
        if let Some(b) = super::read() {
            assert!(b.percent <= 100, "{b:?}");
            assert!(!(b.charging && !b.on_power), "charging without power: {b:?}");
        }
    }
}
