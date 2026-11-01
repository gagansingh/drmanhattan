//! CPU die temperatures and the thermal state on Apple silicon, read through the private
//! IOHIDEventSystem API that `powermetrics` uses, with no sudo. The sensor names
//! (`PMU tdie*`, `gas gauge battery`) are what Apple's own tools match on.

use std::ffi::{c_char, c_void, CStr, CString};

use serde::Serialize;

/// Readings from the die sensors (degrees Celsius) and the system's thermal state.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub(crate) struct Thermals {
    pub(crate) cpu_max: f64,
    pub(crate) cpu_avg: f64,
    pub(crate) sensors: usize,
    pub(crate) battery: Option<f64>,
    /// `nominal`, `fair`, `serious` or `critical`, from NSProcessInfo.
    pub(crate) thermal_state: Option<&'static str>,
}

type CFTypeRef = *const c_void;
type CFIndex = isize;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDictionaryCreate(
        alloc: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        n: CFIndex,
        kcb: *const c_void,
        vcb: *const c_void,
    ) -> CFTypeRef;
    fn CFNumberCreate(alloc: CFTypeRef, kind: CFIndex, value: *const c_void) -> CFTypeRef;
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, encoding: u32) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: CFIndex, encoding: u32) -> bool;
    fn CFArrayGetCount(a: CFTypeRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: CFIndex) -> CFTypeRef;
    fn CFRelease(t: CFTypeRef);
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDEventSystemClientCreate(alloc: CFTypeRef) -> CFTypeRef;
    fn IOHIDEventSystemClientSetMatching(client: CFTypeRef, matching: CFTypeRef);
    fn IOHIDEventSystemClientCopyServices(client: CFTypeRef) -> CFTypeRef;
    fn IOHIDServiceClientCopyProperty(service: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn IOHIDServiceClientCopyEvent(service: CFTypeRef, kind: i64, options: i32, timestamp: i64) -> CFTypeRef;
    fn IOHIDEventGetFloatValue(event: CFTypeRef, field: i32) -> f64;
}

#[link(name = "Foundation", kind = "framework")]
extern "C" {}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

/// `objc_msgSend` must be called through a pointer of the exact method signature: on
/// arm64 a variadic declaration passes arguments differently and is undefined behaviour.
type MsgSendId = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
type MsgSendLong = unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize;

fn msg_send_id() -> MsgSendId {
    // SAFETY: objc_msgSend is a trampoline that adopts the caller's signature; this
    // signature (receiver, selector) -> id matches the methods it is used with.
    unsafe { std::mem::transmute::<unsafe extern "C" fn(), MsgSendId>(objc_msgSend) }
}

fn msg_send_long() -> MsgSendLong {
    // SAFETY: as above, for a method returning NSInteger.
    unsafe { std::mem::transmute::<unsafe extern "C" fn(), MsgSendLong>(objc_msgSend) }
}

const UTF8: u32 = 0x0800_0100;
const TEMP_EVENT: i64 = 15; // kIOHIDEventTypeTemperature
const STATES: [&str; 4] = ["nominal", "fair", "serious", "critical"];

/// A long-lived HID event-system client matched to Apple's temperature sensors. Creating
/// it is the expensive part, so one is made on first use and kept.
pub(crate) struct Sensors {
    client: CFTypeRef,
    product: CFTypeRef,
    process_info: *mut c_void,
    thermal_sel: *mut c_void,
}

fn cfstr(s: &str) -> CFTypeRef {
    let c = CString::new(s).unwrap_or_default();
    // SAFETY: `c` is a valid NUL-terminated string for the duration of the call.
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) }
}

fn cfnum(v: i32) -> CFTypeRef {
    // SAFETY: kCFNumberSInt32Type (3) reads exactly one i32 from the pointer.
    unsafe { CFNumberCreate(std::ptr::null(), 3, &v as *const i32 as *const c_void) }
}

impl Sensors {
    pub(crate) fn new() -> Option<Sensors> {
        let keys = [cfstr("PrimaryUsagePage"), cfstr("PrimaryUsage")];
        let vals = [cfnum(0xFF00), cfnum(5)]; // Apple vendor page, temperature sensor
        if keys.iter().chain(vals.iter()).any(|p| p.is_null()) {
            return None;
        }
        // SAFETY: two valid keys and values; the dictionary retains them, so ours are
        // released right after. The client copies the matching dictionary.
        unsafe {
            let matching = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                vals.as_ptr(),
                2,
                &kCFTypeDictionaryKeyCallBacks as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const c_void,
            );
            for p in keys.iter().chain(vals.iter()) {
                CFRelease(*p);
            }
            if matching.is_null() {
                return None;
            }
            let client = IOHIDEventSystemClientCreate(std::ptr::null());
            if client.is_null() {
                CFRelease(matching);
                return None;
            }
            IOHIDEventSystemClientSetMatching(client, matching);
            CFRelease(matching);
            let cls = objc_getClass(b"NSProcessInfo\0".as_ptr() as *const c_char);
            let process_info = if cls.is_null() {
                std::ptr::null_mut()
            } else {
                msg_send_id()(cls, sel_registerName(b"processInfo\0".as_ptr() as *const c_char))
            };
            Some(Sensors {
                client,
                product: cfstr("Product"),
                process_info,
                thermal_sel: sel_registerName(b"thermalState\0".as_ptr() as *const c_char),
            })
        }
    }

    pub(crate) fn read(&self) -> Option<Thermals> {
        let mut die: Vec<f64> = Vec::new();
        let mut battery: Option<f64> = None;
        // SAFETY: `client` and `product` are valid for our lifetime; every object we
        // copy is checked for null and released exactly once; `buf` outlives its use.
        unsafe {
            let services = IOHIDEventSystemClientCopyServices(self.client);
            if services.is_null() {
                return None;
            }
            for i in 0..CFArrayGetCount(services) {
                let svc = CFArrayGetValueAtIndex(services, i);
                let name_ref = IOHIDServiceClientCopyProperty(svc, self.product);
                if name_ref.is_null() {
                    continue;
                }
                let mut buf = [0 as c_char; 128];
                let ok = CFStringGetCString(name_ref, buf.as_mut_ptr(), 128, UTF8);
                CFRelease(name_ref);
                if !ok {
                    continue;
                }
                let name = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
                if !(name.starts_with("PMU tdie") || name == "gas gauge battery") {
                    continue;
                }
                let event = IOHIDServiceClientCopyEvent(svc, TEMP_EVENT, 0, 0);
                if event.is_null() {
                    continue;
                }
                let value = IOHIDEventGetFloatValue(event, (TEMP_EVENT << 16) as i32);
                CFRelease(event);
                if !(value > 0.0 && value < 150.0) {
                    continue; // unpopulated sensors read 0 or garbage
                }
                if name == "gas gauge battery" {
                    battery = Some((value * 10.0).round() / 10.0);
                } else {
                    die.push(value);
                }
            }
            CFRelease(services);
        }
        if die.is_empty() {
            return None;
        }
        let state = if self.process_info.is_null() {
            -1
        } else {
            // SAFETY: NSProcessInfo's thermalState takes no arguments and returns NSInteger.
            unsafe { msg_send_long()(self.process_info, self.thermal_sel) }
        };
        let max = die.iter().copied().fold(f64::MIN, f64::max);
        let avg = die.iter().sum::<f64>() / die.len() as f64;
        Some(Thermals {
            cpu_max: (max * 10.0).round() / 10.0,
            cpu_avg: (avg * 10.0).round() / 10.0,
            sensors: die.len(),
            battery,
            thermal_state: usize::try_from(state).ok().and_then(|s| STATES.get(s)).copied(),
        })
    }
}
