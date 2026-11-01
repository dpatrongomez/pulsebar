//! Thin, safe wrappers over macOS system interfaces (libproc, mach host statistics) plus a
//! bounded subprocess helper for the few tools we shell out to (lsof, codesign, ioreg…).
//! Non-macOS builds compile with stubs so the pure logic stays testable anywhere.
#![allow(deprecated)] // libc's mach_* items point at the mach2 crate; they are fine here.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::model::ListeningPort;

pub fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn now_s() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Run a command with a timeout; returns stdout (and stderr appended when `merge_stderr`).
pub fn run(cmd: &str, args: &[&str], timeout: Duration) -> Option<String> {
    run_full(cmd, args, timeout).map(|(o, _)| o)
}

pub fn run_full(cmd: &str, args: &[&str], timeout: Duration) -> Option<(String, String)> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    // Readers finish when the child closes its pipes; wait on that (no polling), kill on timeout.
    let (tx, rx) = std::sync::mpsc::channel();
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.read_to_end(&mut s);
        let _ = tx.send((0u8, s));
    });
    std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = err.read_to_end(&mut s);
        let _ = tx2.send((1u8, s));
    });
    let deadline = Instant::now() + timeout;
    let (mut o, mut e) = (None, None);
    while o.is_none() || e.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok((0, v)) => o = Some(v),
            Ok((_, v)) => e = Some(v),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let _ = child.wait();
    Some((String::from_utf8_lossy(&o.unwrap_or_default()).into_owned(), String::from_utf8_lossy(&e.unwrap_or_default()).into_owned()))
}

// ---------------------------------------------------------------------------------------------
// Per-process resource usage (proc_pid_rusage v4)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct RUsage {
    pub user_s: f64,
    pub system_s: f64,
    pub phys_footprint: u64,
    pub peak_footprint: u64,
    pub energy_nj: u64,
    pub disk_read: u64,
    pub disk_written: u64,
}

#[cfg(target_os = "macos")]
fn timebase_ns_per_tick() -> f64 {
    use std::sync::OnceLock;
    static TB: OnceLock<f64> = OnceLock::new();
    *TB.get_or_init(|| {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        unsafe { libc::mach_timebase_info(&mut info) };
        if info.denom == 0 { 1.0 } else { info.numer as f64 / info.denom as f64 }
    })
}

#[cfg(target_os = "macos")]
pub fn rusage(pid: i32) -> Option<RUsage> {
    let mut ri: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, &mut ri as *mut _ as *mut libc::rusage_info_t)
    };
    if r != 0 {
        return None;
    }
    // ri_user_time / ri_system_time are in mach absolute time units.
    let tb = timebase_ns_per_tick();
    Some(RUsage {
        user_s: ri.ri_user_time as f64 * tb / 1e9,
        system_s: ri.ri_system_time as f64 * tb / 1e9,
        phys_footprint: ri.ri_phys_footprint,
        peak_footprint: ri.ri_lifetime_max_phys_footprint,
        energy_nj: ri.ri_billed_energy,
        disk_read: ri.ri_diskio_bytesread,
        disk_written: ri.ri_diskio_byteswritten,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn rusage(_pid: i32) -> Option<RUsage> {
    None
}

/// Number of open file descriptors (PROC_PIDLISTFDS).
#[cfg(target_os = "macos")]
pub fn open_fd_count(pid: i32) -> Option<usize> {
    const PROC_PIDLISTFDS: libc::c_int = 1;
    const FDINFO_SIZE: i32 = 8; // struct proc_fdinfo { int32 fd; uint32 type; }
    let n = unsafe { libc::proc_pidinfo(pid, PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if n <= 0 {
        return None;
    }
    let mut buf = vec![0u8; n as usize];
    let got = unsafe { libc::proc_pidinfo(pid, PROC_PIDLISTFDS, 0, buf.as_mut_ptr() as *mut _, n) };
    (got > 0).then_some((got / FDINFO_SIZE) as usize)
}

#[cfg(not(target_os = "macos"))]
pub fn open_fd_count(_pid: i32) -> Option<usize> {
    None
}

// ---------------------------------------------------------------------------------------------
// Host statistics
// ---------------------------------------------------------------------------------------------

/// Aggregate CPU ticks: (user, system, idle, nice).
#[cfg(target_os = "macos")]
pub fn host_cpu_ticks() -> Option<[u64; 4]> {
    let mut info: libc::host_cpu_load_info = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    #[allow(deprecated)]
    let host = unsafe { libc::mach_host_self() };
    let r = unsafe {
        libc::host_statistics(host, libc::HOST_CPU_LOAD_INFO, &mut info as *mut _ as *mut libc::integer_t, &mut count)
    };
    if r != 0 {
        return None;
    }
    let t = info.cpu_ticks;
    Some([
        t[libc::CPU_STATE_USER as usize] as u64,
        t[libc::CPU_STATE_SYSTEM as usize] as u64,
        t[libc::CPU_STATE_IDLE as usize] as u64,
        t[libc::CPU_STATE_NICE as usize] as u64,
    ])
}

#[cfg(not(target_os = "macos"))]
pub fn host_cpu_ticks() -> Option<[u64; 4]> {
    None
}

#[derive(Debug, Clone, Copy, Default)]
pub struct VmBreakdown {
    pub app: u64,
    pub wired: u64,
    pub compressed: u64,
    pub cached: u64,
    pub free: u64,
}

/// Activity-Monitor-style memory categories from `host_statistics64(HOST_VM_INFO64)`.
#[cfg(target_os = "macos")]
pub fn vm_breakdown() -> Option<VmBreakdown> {
    let mut vm: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    #[allow(deprecated)]
    let host = unsafe { libc::mach_host_self() };
    let r = unsafe {
        libc::host_statistics64(host, libc::HOST_VM_INFO64, &mut vm as *mut _ as *mut libc::integer_t, &mut count)
    };
    if r != 0 {
        return None;
    }
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let p = |n: u32| n as u64 * page;
    Some(VmBreakdown {
        app: p(vm.internal_page_count.saturating_sub(vm.purgeable_count)),
        wired: p(vm.wire_count),
        compressed: p(vm.compressor_page_count),
        cached: p(vm.external_page_count + vm.purgeable_count),
        free: p(vm.free_count.saturating_sub(vm.speculative_count)),
    })
}

#[cfg(not(target_os = "macos"))]
pub fn vm_breakdown() -> Option<VmBreakdown> {
    None
}

// ---------------------------------------------------------------------------------------------
// Bundles
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct BundleInfo {
    pub path: String,
    pub bundle_id: Option<String>,
    pub name: String,
    pub version: Option<String>,
}

/// Outermost `.app` bundle containing `exe` ("…/Google Chrome.app/Contents/Frameworks/…/Google
/// Chrome Helper (Renderer).app/…" → ".../Google Chrome.app").
pub fn outermost_bundle(exe: &str) -> Option<String> {
    let idx = exe.find(".app/").or_else(|| exe.ends_with(".app").then(|| exe.len() - 4))?;
    Some(exe[..idx + 4].to_string())
}

pub fn read_bundle(bundle_path: &str) -> BundleInfo {
    let plist_path = Path::new(bundle_path).join("Contents/Info.plist");
    let fallback_name = Path::new(bundle_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| bundle_path.to_string());
    let mut info = BundleInfo { path: bundle_path.to_string(), name: fallback_name, ..Default::default() };
    if let Ok(plist::Value::Dictionary(d)) = plist::Value::from_file(&plist_path) {
        let s = |k: &str| d.get(k).and_then(|v| v.as_string()).map(str::to_string);
        info.bundle_id = s("CFBundleIdentifier");
        info.version = s("CFBundleShortVersionString").or_else(|| s("CFBundleVersion"));
        if let Some(n) = s("CFBundleDisplayName").or_else(|| s("CFBundleName")).filter(|n| !n.is_empty()) {
            // Prefer the on-disk name when it is more descriptive ("Visual Studio Code" vs "Code").
            if n.len() >= info.name.len() || info.name.contains('.') {
                info.name = n;
            }
        }
    }
    info
}

/// Cache of bundle lookups (Info.plist reads happen once per bundle).
#[derive(Default)]
pub struct BundleCache {
    map: HashMap<String, BundleInfo>,
}

impl BundleCache {
    pub fn get(&mut self, bundle_path: &str) -> &BundleInfo {
        self.map.entry(bundle_path.to_string()).or_insert_with(|| read_bundle(bundle_path))
    }
}

// ---------------------------------------------------------------------------------------------
// Listening ports
// ---------------------------------------------------------------------------------------------

/// Parse `lsof -nP -iTCP -sTCP:LISTEN -FpcnPu` style output (field mode).
pub fn parse_lsof_listen(out: &str) -> Vec<ListeningPort> {
    let mut v = Vec::new();
    let (mut pid, mut cmd, mut proto) = (0i32, String::new(), String::from("TCP"));
    for line in out.lines() {
        let (tag, rest) = line.split_at(line.len().min(1));
        match tag {
            "p" => pid = rest.parse().unwrap_or(0),
            "c" => cmd = rest.to_string(),
            "P" => proto = rest.to_string(),
            "n" => {
                if let Some((addr, port)) = rest.rsplit_once(':')
                    && let Ok(port) = port.parse::<u16>() {
                        v.push(ListeningPort { pid, port, address: addr.to_string(), protocol: proto.clone(), command: cmd.clone() });
                    }
            }
            _ => {}
        }
    }
    v.sort_by_key(|p| (p.port, p.pid));
    v.dedup_by_key(|p| (p.port, p.pid));
    v
}

/// TCP listening sockets of the current user's processes.
pub fn listening_ports() -> Vec<ListeningPort> {
    let uid = current_uid().to_string();
    run("/usr/sbin/lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-u", &uid, "-FpcnP"], Duration::from_secs(4))
        .map(|o| parse_lsof_listen(&o))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Code signing
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CodeSignInfo {
    pub signed: bool,
    pub identifier: Option<String>,
    pub team_id: Option<String>,
    pub authority: Option<String>,
    pub hardened_runtime: bool,
    pub sandboxed: bool,
    pub entitlements: Vec<String>,
    pub notarized_or_apple: bool,
}

pub fn parse_codesign(stderr: &str) -> CodeSignInfo {
    let mut c = CodeSignInfo::default();
    for line in stderr.lines() {
        if let Some(v) = line.strip_prefix("Identifier=") {
            c.identifier = Some(v.to_string());
            c.signed = true;
        } else if let Some(v) = line.strip_prefix("TeamIdentifier=") {
            if v != "not set" {
                c.team_id = Some(v.to_string());
            }
        } else if let Some(v) = line.strip_prefix("Authority=") {
            if c.authority.is_none() {
                c.authority = Some(v.to_string());
            }
            if v.contains("Apple") {
                c.notarized_or_apple = true;
            }
        } else if line.contains("flags=") && line.contains("runtime") {
            c.hardened_runtime = true;
        } else if line.contains("Signature=adhoc") {
            c.signed = true;
        }
    }
    c
}

pub fn codesign_info(path: &str) -> CodeSignInfo {
    let mut info = run_full("/usr/bin/codesign", &["-dv", "--verbose=2", path], Duration::from_secs(5))
        .map(|(_, e)| parse_codesign(&e))
        .unwrap_or_default();
    if let Some((out, _)) =
        run_full("/usr/bin/codesign", &["-d", "--entitlements", "-", "--xml", path], Duration::from_secs(5))
        && let Some(start) = out.find("<?xml").or_else(|| out.find("<plist"))
            && let Ok(plist::Value::Dictionary(d)) = plist::Value::from_reader_xml(&out.as_bytes()[start..]) {
                info.sandboxed = d.get("com.apple.security.app-sandbox").and_then(|v| v.as_boolean()).unwrap_or(false);
                info.entitlements = d.keys().cloned().collect();
                info.entitlements.sort();
            }
    info
}

pub fn executable_archs(path: &str) -> Vec<String> {
    run("/usr/bin/lipo", &["-archs", path], Duration::from_secs(3))
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn username(uid: u32) -> String {
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() || (*pw).pw_name.is_null() {
            return uid.to_string();
        }
        std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned()
    }
}

/// Send a signal. Only ever called through `actions::execute` with a confirmation token.
pub(crate) fn signal(pid: i32, sig: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    unsafe { libc::kill(pid, sig) == 0 }
}

/// `responsibility_get_pid_responsible_for_pid` (resolved at runtime via dlsym; 0 if missing).
pub fn responsible_pid(pid: i32) -> i32 {
    #[cfg(target_os = "macos")]
    {
        use std::sync::OnceLock;
        type F = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;
        static SYM: OnceLock<Option<F>> = OnceLock::new();
        let f = SYM.get_or_init(|| {
            let name = c"responsibility_get_pid_responsible_for_pid";
            let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            (!p.is_null()).then(|| unsafe { std::mem::transmute::<*mut libc::c_void, F>(p) })
        });
        match f {
            Some(f) => {
                let r = unsafe { f(pid) };
                if r > 0 { r } else { 0 }
            }
            None => 0,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        0
    }
}

pub fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_paths() {
        assert_eq!(
            outermost_bundle("/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/1/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)").as_deref(),
            Some("/Applications/Google Chrome.app")
        );
        assert_eq!(outermost_bundle("/usr/local/bin/node"), None);
    }

    #[test]
    fn lsof_parse() {
        let out = "p4242\ncnode\nPTCP\nn*:3000\nn[::1]:3000\np99\ncpostgres\nPTCP\nn127.0.0.1:5432\n";
        let v = parse_lsof_listen(out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].port, 3000);
        assert_eq!(v[0].pid, 4242);
        assert_eq!(v[1].command, "postgres");
    }

    #[test]
    fn codesign_parse() {
        let e = "Executable=/x\nIdentifier=com.example.app\nCodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1\nAuthority=Developer ID Application: Example (ABCDE12345)\nAuthority=Developer ID Certification Authority\nAuthority=Apple Root CA\nTeamIdentifier=ABCDE12345\n";
        let c = parse_codesign(e);
        assert!(c.signed && c.hardened_runtime);
        assert_eq!(c.team_id.as_deref(), Some("ABCDE12345"));
    }
}
