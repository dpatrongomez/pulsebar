//! Raw FFI for CoreFoundation, CoreAudio, IOKit (AppleSMC) and the private-but-stable
//! IOReport library. All unsafe code for these frameworks is contained here.
#![allow(non_upper_case_globals, non_snake_case, dead_code, deprecated)]

use std::ffi::{CString, c_char, c_void};

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFMutableDictionaryRef = *mut c_void;
pub type CFArrayRef = *const c_void;
pub type CFIndex = isize;

const kCFStringEncodingUTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, enc: u32) -> CFStringRef;
    fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: CFIndex, enc: u32) -> u8;
    pub fn CFRelease(cf: CFTypeRef);
    fn CFDictionaryGetValue(d: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
    fn CFDictionaryGetCount(d: CFDictionaryRef) -> CFIndex;
    fn CFDictionaryCreateMutableCopy(alloc: CFTypeRef, cap: CFIndex, d: CFDictionaryRef) -> CFMutableDictionaryRef;
    fn CFArrayGetCount(a: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(a: CFArrayRef, i: CFIndex) -> CFTypeRef;
}

pub struct CfString(pub CFStringRef);
impl CfString {
    pub fn new(s: &str) -> Self {
        let c = CString::new(s).unwrap_or_default();
        CfString(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), kCFStringEncodingUTF8) })
    }
}
impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

/// # Safety
/// `s` must be null or a valid `CFStringRef`.
pub(crate) unsafe fn cfstring_to_string(s: CFStringRef) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let mut buf = vec![0 as c_char; 512];
    let ok = unsafe { CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as CFIndex, kCFStringEncodingUTF8) };
    if ok == 0 {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------------------------
// CoreAudio
// ---------------------------------------------------------------------------------------------

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

#[repr(C)]
pub struct AudioObjectPropertyAddress {
    pub selector: u32,
    pub scope: u32,
    pub element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(id: u32, addr: *const AudioObjectPropertyAddress, qs: u32, q: *const c_void, out: *mut u32) -> i32;
    fn AudioObjectGetPropertyData(id: u32, addr: *const AudioObjectPropertyAddress, qs: u32, q: *const c_void, size: *mut u32, data: *mut c_void) -> i32;
    fn AudioObjectSetPropertyData(id: u32, addr: *const AudioObjectPropertyAddress, qs: u32, q: *const c_void, size: u32, data: *const c_void) -> i32;
    fn AudioObjectHasProperty(id: u32, addr: *const AudioObjectPropertyAddress) -> u8;
}

pub const kAudioObjectSystemObject: u32 = 1;
pub const SCOPE_GLOBAL: u32 = fourcc(b"glob");
pub const SCOPE_OUTPUT: u32 = fourcc(b"outp");
pub const SCOPE_INPUT: u32 = fourcc(b"inpt");
const DEFAULT_OUTPUT: u32 = fourcc(b"dOut");
const DEFAULT_INPUT: u32 = fourcc(b"dIn ");
const OBJECT_NAME: u32 = fourcc(b"lnam");
const VOLUME_SCALAR: u32 = fourcc(b"volm");
const MUTE: u32 = fourcc(b"mute");
const PROCESS_LIST: u32 = fourcc(b"prs#");
const PROCESS_PID: u32 = fourcc(b"ppid");
const PROCESS_RUNNING_OUTPUT: u32 = fourcc(b"piro");
const PROCESS_RUNNING_INPUT: u32 = fourcc(b"piri");

fn addr(selector: u32, scope: u32, element: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { selector, scope, element }
}

fn get_u32(id: u32, a: &AudioObjectPropertyAddress) -> Option<u32> {
    let mut v: u32 = 0;
    let mut size = 4u32;
    let r = unsafe { AudioObjectGetPropertyData(id, a, 0, std::ptr::null(), &mut size, &mut v as *mut _ as *mut c_void) };
    (r == 0).then_some(v)
}

fn get_i32(id: u32, a: &AudioObjectPropertyAddress) -> Option<i32> {
    get_u32(id, a).map(|v| v as i32)
}

fn get_f32(id: u32, a: &AudioObjectPropertyAddress) -> Option<f32> {
    let mut v: f32 = 0.0;
    let mut size = 4u32;
    if unsafe { AudioObjectHasProperty(id, a) } == 0 {
        return None;
    }
    let r = unsafe { AudioObjectGetPropertyData(id, a, 0, std::ptr::null(), &mut size, &mut v as *mut _ as *mut c_void) };
    (r == 0).then_some(v)
}

pub fn default_device(input: bool) -> Option<u32> {
    get_u32(kAudioObjectSystemObject, &addr(if input { DEFAULT_INPUT } else { DEFAULT_OUTPUT }, SCOPE_GLOBAL, 0))
        .filter(|d| *d != 0)
}

pub fn device_name(dev: u32) -> Option<String> {
    let a = addr(OBJECT_NAME, SCOPE_GLOBAL, 0);
    let mut s: CFStringRef = std::ptr::null();
    let mut size = std::mem::size_of::<CFStringRef>() as u32;
    let r = unsafe { AudioObjectGetPropertyData(dev, &a, 0, std::ptr::null(), &mut size, &mut s as *mut _ as *mut c_void) };
    if r != 0 || s.is_null() {
        return None;
    }
    let out = unsafe { cfstring_to_string(s) };
    unsafe { CFRelease(s) };
    out
}

/// Device volume 0..1: main element, or the average of channels 1/2.
pub fn device_volume(dev: u32, input: bool) -> Option<f64> {
    let scope = if input { SCOPE_INPUT } else { SCOPE_OUTPUT };
    if let Some(v) = get_f32(dev, &addr(VOLUME_SCALAR, scope, 0)) {
        return Some(v as f64);
    }
    let l = get_f32(dev, &addr(VOLUME_SCALAR, scope, 1));
    let r = get_f32(dev, &addr(VOLUME_SCALAR, scope, 2));
    match (l, r) {
        (Some(l), Some(r)) => Some(((l + r) / 2.0) as f64),
        (Some(x), None) | (None, Some(x)) => Some(x as f64),
        _ => None,
    }
}

pub fn set_device_volume(dev: u32, input: bool, v: f64) -> bool {
    let scope = if input { SCOPE_INPUT } else { SCOPE_OUTPUT };
    let val = v.clamp(0.0, 1.0) as f32;
    let mut ok = false;
    for el in [0u32, 1, 2] {
        let a = addr(VOLUME_SCALAR, scope, el);
        if unsafe { AudioObjectHasProperty(dev, &a) } != 0 {
            let r = unsafe { AudioObjectSetPropertyData(dev, &a, 0, std::ptr::null(), 4, &val as *const _ as *const c_void) };
            ok |= r == 0;
            if el == 0 && r == 0 {
                break;
            }
        }
    }
    ok
}

pub fn device_muted(dev: u32) -> bool {
    get_u32(dev, &addr(MUTE, SCOPE_OUTPUT, 0)).map(|v| v != 0).unwrap_or(false)
}

pub fn set_device_muted(dev: u32, muted: bool) -> bool {
    let v: u32 = muted as u32;
    let a = addr(MUTE, SCOPE_OUTPUT, 0);
    unsafe { AudioObjectSetPropertyData(dev, &a, 0, std::ptr::null(), 4, &v as *const _ as *const c_void) == 0 }
}

/// (pid, running_output, running_input) for every process CoreAudio knows (macOS 14+).
pub fn audio_processes() -> Vec<(i32, bool, bool)> {
    let a = addr(PROCESS_LIST, SCOPE_GLOBAL, 0);
    let mut size = 0u32;
    if unsafe { AudioObjectGetPropertyDataSize(kAudioObjectSystemObject, &a, 0, std::ptr::null(), &mut size) } != 0 {
        return vec![];
    }
    let n = size as usize / 4;
    let mut ids = vec![0u32; n];
    if unsafe {
        AudioObjectGetPropertyData(kAudioObjectSystemObject, &a, 0, std::ptr::null(), &mut size, ids.as_mut_ptr() as *mut c_void)
    } != 0
    {
        return vec![];
    }
    ids.into_iter()
        .filter_map(|id| {
            let pid = get_i32(id, &addr(PROCESS_PID, SCOPE_GLOBAL, 0))?;
            let out = get_u32(id, &addr(PROCESS_RUNNING_OUTPUT, SCOPE_GLOBAL, 0)).unwrap_or(0) != 0;
            let inp = get_u32(id, &addr(PROCESS_RUNNING_INPUT, SCOPE_GLOBAL, 0)).unwrap_or(0) != 0;
            Some((pid, out, inp))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// IOKit / AppleSMC (fans)
// ---------------------------------------------------------------------------------------------

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingService(main_port: u32, matching: CFMutableDictionaryRef) -> u32;
    fn IOServiceOpen(service: u32, owning_task: u32, typ: u32, connect: *mut u32) -> i32;
    fn IOServiceClose(connect: u32) -> i32;
    fn IOObjectRelease(obj: u32) -> i32;
    fn IOConnectCallStructMethod(conn: u32, selector: u32, input: *const c_void, in_size: usize, output: *mut c_void, out_size: *mut usize) -> i32;
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SmcVers {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SmcPLimit {
    version: u16,
    length: u16,
    cpu: u32,
    gpu: u32,
    mem: u32,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SmcKeyInfo {
    data_size: u32,
    data_type: u32,
    data_attributes: u8,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SmcKeyData {
    key: u32,
    vers: SmcVers,
    p_limit: SmcPLimit,
    key_info: SmcKeyInfo,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<SmcKeyData>() == 80);

pub struct Smc {
    conn: u32,
}

// The SMC connection is only used from the sensors worker thread.
unsafe impl Send for Smc {}

impl Smc {
    pub fn open() -> Option<Smc> {
        let name = CString::new("AppleSMC").ok()?;
        let svc = unsafe { IOServiceGetMatchingService(0, IOServiceMatching(name.as_ptr())) };
        if svc == 0 {
            return None;
        }
        let mut conn = 0u32;
        let r = unsafe { IOServiceOpen(svc, libc::mach_task_self(), 0, &mut conn) };
        unsafe { IOObjectRelease(svc) };
        (r == 0).then_some(Smc { conn })
    }

    fn call(&self, input: &SmcKeyData) -> Option<SmcKeyData> {
        let mut out = SmcKeyData::default();
        let mut size = std::mem::size_of::<SmcKeyData>();
        let r = unsafe {
            IOConnectCallStructMethod(
                self.conn,
                2,
                input as *const _ as *const c_void,
                std::mem::size_of::<SmcKeyData>(),
                &mut out as *mut _ as *mut c_void,
                &mut size,
            )
        };
        (r == 0 && out.result == 0).then_some(out)
    }

    /// Read a key and decode it as a number.
    pub fn read(&self, key: &str) -> Option<f64> {
        let k: [u8; 4] = key.as_bytes().try_into().ok()?;
        let input = SmcKeyData { key: fourcc(&k), data8: 9, ..Default::default() };
        let info = self.call(&input)?.key_info;
        let input = SmcKeyData { key: fourcc(&k), key_info: info, data8: 5, ..Default::default() };
        let out = self.call(&input)?;
        let b = out.bytes;
        let t = info.data_type;
        Some(if t == fourcc(b"flt ") {
            f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64
        } else if t == fourcc(b"fpe2") {
            u16::from_be_bytes([b[0], b[1]]) as f64 / 4.0
        } else if t == fourcc(b"ui8 ") {
            b[0] as f64
        } else if t == fourcc(b"ui16") {
            u16::from_be_bytes([b[0], b[1]]) as f64
        } else if t == fourcc(b"ui32") {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64
        } else if t == fourcc(b"sp78") {
            i16::from_be_bytes([b[0], b[1]]) as f64 / 256.0
        } else {
            return None;
        })
    }
}

impl Drop for Smc {
    fn drop(&mut self) {
        unsafe { IOServiceClose(self.conn) };
    }
}

// ---------------------------------------------------------------------------------------------
// IOReport (Energy Model: CPU / GPU / ANE power on Apple silicon)
// ---------------------------------------------------------------------------------------------

#[link(name = "IOReport", kind = "dylib")]
unsafe extern "C" {
    fn IOReportCopyChannelsInGroup(group: CFStringRef, subgroup: CFStringRef, a: u64, b: u64, c: u64) -> CFDictionaryRef;
    fn IOReportCreateSubscription(a: *const c_void, desired: CFMutableDictionaryRef, subbed: *mut CFMutableDictionaryRef, id: u64, b: CFTypeRef) -> *const c_void;
    fn IOReportCreateSamples(sub: *const c_void, subbed: CFMutableDictionaryRef, a: CFTypeRef) -> CFDictionaryRef;
    fn IOReportCreateSamplesDelta(prev: CFDictionaryRef, cur: CFDictionaryRef, a: CFTypeRef) -> CFDictionaryRef;
    fn IOReportChannelGetChannelName(ch: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetUnitLabel(ch: CFDictionaryRef) -> CFStringRef;
    fn IOReportSimpleGetIntegerValue(ch: CFDictionaryRef, idx: i32) -> i64;
}

pub struct EnergyReporter {
    sub: *const c_void,
    subbed: CFMutableDictionaryRef,
    prev: Option<(CFDictionaryRef, std::time::Instant)>,
}

unsafe impl Send for EnergyReporter {}

#[derive(Debug, Default, Clone, Copy)]
pub struct EnergyWatts {
    pub cpu: f64,
    pub gpu: f64,
    pub ane: f64,
    pub dram: f64,
}

impl EnergyReporter {
    pub fn new() -> Option<Self> {
        let group = CfString::new("Energy Model");
        let chans = unsafe { IOReportCopyChannelsInGroup(group.0, std::ptr::null(), 0, 0, 0) };
        if chans.is_null() {
            return None;
        }
        let mutable = unsafe { CFDictionaryCreateMutableCopy(std::ptr::null(), CFDictionaryGetCount(chans), chans) };
        unsafe { CFRelease(chans) };
        let mut subbed: CFMutableDictionaryRef = std::ptr::null_mut();
        let sub = unsafe { IOReportCreateSubscription(std::ptr::null(), mutable, &mut subbed, 0, std::ptr::null()) };
        if sub.is_null() || subbed.is_null() {
            return None;
        }
        Some(Self { sub, subbed, prev: None })
    }

    /// Average power since the previous call (None on the first call).
    pub fn sample(&mut self) -> Option<EnergyWatts> {
        let cur = unsafe { IOReportCreateSamples(self.sub, self.subbed, std::ptr::null()) };
        if cur.is_null() {
            return None;
        }
        let now = std::time::Instant::now();
        let result = match self.prev.take() {
            Some((prev, t0)) => {
                let dt = now.duration_since(t0).as_secs_f64().max(1e-3);
                let delta = unsafe { IOReportCreateSamplesDelta(prev, cur, std::ptr::null()) };
                unsafe { CFRelease(prev) };
                if delta.is_null() {
                    None
                } else {
                    let w = Self::parse(delta, dt);
                    unsafe { CFRelease(delta) };
                    Some(w)
                }
            }
            None => None,
        };
        self.prev = Some((cur, now));
        result
    }

    fn parse(delta: CFDictionaryRef, dt: f64) -> EnergyWatts {
        let mut w = EnergyWatts::default();
        let key = CfString::new("IOReportChannels");
        let arr = unsafe { CFDictionaryGetValue(delta, key.0) };
        if arr.is_null() {
            return w;
        }
        let n = unsafe { CFArrayGetCount(arr) };
        for i in 0..n {
            let ch = unsafe { CFArrayGetValueAtIndex(arr, i) };
            let name = unsafe { cfstring_to_string(IOReportChannelGetChannelName(ch)) }.unwrap_or_default();
            let unit = unsafe { cfstring_to_string(IOReportChannelGetUnitLabel(ch)) }.unwrap_or_default();
            let val = unsafe { IOReportSimpleGetIntegerValue(ch, 0) } as f64;
            let joules = match unit.trim() {
                "mJ" => val / 1e3,
                "uJ" | "µJ" => val / 1e6,
                "nJ" => val / 1e9,
                _ => continue,
            };
            let watts = joules / dt;
            if name.contains("CPU Energy") {
                w.cpu += watts;
            } else if name.ends_with("GPU Energy") {
                w.gpu += watts;
            } else if name.starts_with("ANE") {
                w.ane += watts;
            } else if name.starts_with("DRAM") {
                w.dram += watts;
            }
        }
        w
    }
}
