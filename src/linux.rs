//! Linux data sources: `/proc`, `/sys`, `.desktop` files and the few tools every distro has for
//! what the kernel doesn't expose (`ss`, `pactl`, `bluetoothctl`, `nvidia-smi`).
//!
//! Every parser is a pure function compiled on every platform, so its tests run on any machine.
//! Anything missing (no battery, no `pactl`, a counter only root can read) degrades to "not
//! available" rather than an error.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::model::{AudioApp, AudioMetrics, AudioState, BatteryMetrics, BluetoothDevice, BluetoothMetrics, Fan, GpuMetrics, ListeningPort};
use crate::sys::{self, BundleInfo, RUsage, VmBreakdown};

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Output of a tool, in the C locale so the text we parse isn't translated.
fn tool(cmd: &str, args: &[&str], timeout_s: u64) -> Option<String> {
    sys::run_env(cmd, args, Duration::from_secs(timeout_s), &[("LC_ALL", "C")])
}

// ---------------------------------------------------------------------------------------------
// CPU and memory
// ---------------------------------------------------------------------------------------------

/// (user, system, idle, nice) from the first line of `/proc/stat`. I/O wait counts as idle and
/// IRQ time as system, as Activity Monitor-style totals do.
pub fn parse_cpu_ticks(stat: &str) -> Option<[u64; 4]> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let f: Vec<u64> = line.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
    let at = |i: usize| f.get(i).copied().unwrap_or(0);
    (f.len() >= 4).then(|| [at(0), at(2) + at(5) + at(6) + at(7), at(3) + at(4), at(1)])
}

pub fn host_cpu_ticks() -> Option<[u64; 4]> {
    parse_cpu_ticks(&read("/proc/stat")?)
}

/// Memory categories from `/proc/meminfo`, in the terms the app uses everywhere: app = anonymous
/// and shared pages, wired = kernel memory that can't be reclaimed, compressed = the zswap pool,
/// cached = reclaimable page cache.
pub fn parse_meminfo(text: &str) -> Option<VmBreakdown> {
    let m: HashMap<&str, u64> = text
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k, v.split_whitespace().next()?.parse::<u64>().ok()? * 1024))
        })
        .collect();
    let get = |k: &str| m.get(k).copied().unwrap_or(0);
    m.get("MemTotal")?;
    let shmem = get("Shmem");
    Some(VmBreakdown {
        app: get("AnonPages") + shmem,
        wired: get("SUnreclaim") + get("KernelStack") + get("PageTables"),
        compressed: get("Zswap"),
        cached: (get("Buffers") + get("Cached") + get("SReclaimable")).saturating_sub(shmem),
        free: get("MemFree"),
    })
}

pub fn vm_breakdown() -> Option<VmBreakdown> {
    parse_meminfo(&read("/proc/meminfo")?)
}

// ---------------------------------------------------------------------------------------------
// Per-process usage
// ---------------------------------------------------------------------------------------------

/// (utime, stime) in clock ticks. The command name may hold spaces and parentheses, so fields are
/// counted from the last `)`.
pub fn parse_stat_times(stat: &str) -> Option<(u64, u64)> {
    let f: Vec<&str> = stat.get(stat.rfind(')')? + 1..)?.split_whitespace().collect();
    Some((f.get(11)?.parse().ok()?, f.get(12)?.parse().ok()?))
}

/// (bytes read, bytes written) that actually reached the block layer.
pub fn parse_proc_io(io: &str) -> (u64, u64) {
    let field = |name: &str| io.lines().find_map(|l| l.strip_prefix(name)?.trim().parse::<u64>().ok()).unwrap_or(0);
    (field("read_bytes:"), field("write_bytes:"))
}

/// Peak resident memory (`VmHWM`) in bytes.
pub fn parse_vm_hwm(status: &str) -> u64 {
    status_kb(status, "VmHWM:") * 1024
}

fn status_kb(status: &str, key: &str) -> u64 {
    status.lines().find_map(|l| l.strip_prefix(key)?.split_whitespace().next()?.parse().ok()).unwrap_or(0)
}

/// What a process alone keeps in memory: anonymous pages, resident or swapped out. Resident size
/// would count every shared library once per process, so summed over an app it can exceed the
/// machine's RAM. This is the closest match to macOS's physical footprint.
pub fn parse_footprint(status: &str) -> u64 {
    (status_kb(status, "RssAnon:") + status_kb(status, "VmSwap:")) * 1024
}

/// `/proc/<pid>/io` is only readable for our own processes (unless root): others report no I/O.
pub fn rusage(pid: i32) -> Option<RUsage> {
    let (user, system) = parse_stat_times(&read(format!("/proc/{pid}/stat"))?)?;
    let hz = (unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).max(1) as f64;
    let (disk_read, disk_written) = read(format!("/proc/{pid}/io")).map(|s| parse_proc_io(&s)).unwrap_or_default();
    let status = read(format!("/proc/{pid}/status")).unwrap_or_default();
    Some(RUsage {
        user_s: user as f64 / hz,
        system_s: system as f64 / hz,
        phys_footprint: parse_footprint(&status),
        peak_footprint: parse_vm_hwm(&status),
        // The kernel has no per-process energy counter.
        energy_nj: 0,
        disk_read,
        disk_written,
    })
}

pub fn open_fd_count(pid: i32) -> Option<usize> {
    Some(std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?.count())
}

/// Machine name of an ELF executable, the Linux counterpart of `lipo -archs`.
pub fn elf_arch(header: &[u8]) -> Option<&'static str> {
    if header.len() < 20 || &header[..4] != b"\x7fELF" {
        return None;
    }
    let raw = [header[18], header[19]];
    let machine = if header[5] == 2 { u16::from_be_bytes(raw) } else { u16::from_le_bytes(raw) };
    match machine {
        3 => Some("i386"),
        40 => Some("arm"),
        62 => Some("x86_64"),
        183 => Some("arm64"),
        243 => Some("riscv64"),
        _ => None,
    }
}

pub fn executable_archs(path: &str) -> Vec<String> {
    use std::io::Read;
    let mut header = [0u8; 20];
    let ok = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut header)).is_ok();
    ok.then(|| elf_arch(&header)).flatten().map(|a| vec![a.to_string()]).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Listening ports
// ---------------------------------------------------------------------------------------------

/// Decode a `/proc/net/tcp` address: each 32-bit word is in host (little-endian) byte order.
fn decode_addr(hex: &str) -> Option<String> {
    let words: Vec<u32> = (0..hex.len() / 8).map(|i| u32::from_str_radix(hex.get(i * 8..i * 8 + 8)?, 16).ok()).collect::<Option<_>>()?;
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    match bytes.len() {
        4 => {
            let ip = std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]);
            Some(if ip.is_unspecified() { "*".to_string() } else { ip.to_string() })
        }
        16 => {
            let mut b = [0u8; 16];
            b.copy_from_slice(&bytes);
            let ip = std::net::Ipv6Addr::from(b);
            Some(match ip.to_ipv4_mapped() {
                Some(v4) => v4.to_string(),
                None if ip.is_unspecified() => "*".to_string(),
                None => format!("[{ip}]"),
            })
        }
        _ => None,
    }
}

/// Listening sockets of `uid` in a `/proc/net/tcp{,6}` table: socket inode → (address, port).
pub fn parse_net_tcp(table: &str, uid: u32) -> HashMap<u64, (String, u16)> {
    table
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            // sl local rem st queues timer retrnsmt uid timeout inode
            if f.len() < 10 || f[3] != "0A" || f[7].parse::<u32>().ok()? != uid {
                return None;
            }
            let (addr, port) = f[1].split_once(':')?;
            Some((f[9].parse().ok()?, (decode_addr(addr)?, u16::from_str_radix(port, 16).ok()?)))
        })
        .collect()
}

/// TCP listening sockets of the current user's processes.
pub fn listening_ports() -> Vec<ListeningPort> {
    let uid = sys::current_uid();
    let mut sockets = HashMap::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Some(t) = read(table) {
            sockets.extend(parse_net_tcp(&t, uid));
        }
    }
    let mut out = Vec::new();
    if sockets.is_empty() {
        return out;
    }
    let Ok(procs) = std::fs::read_dir("/proc") else { return out };
    for entry in procs.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            let Ok(link) = std::fs::read_link(fd.path()) else { continue };
            let inode = link.to_str().and_then(|s| s.strip_prefix("socket:[")?.strip_suffix(']')?.parse::<u64>().ok());
            if let Some((address, port)) = inode.and_then(|i| sockets.get(&i)) {
                let command = read(entry.path().join("comm")).unwrap_or_default().trim().to_string();
                out.push(ListeningPort { pid, port: *port, address: address.clone(), protocol: "TCP".into(), command });
            }
        }
    }
    out.sort_by_key(|p| (p.port, p.pid));
    out.dedup_by_key(|p| (p.port, p.pid));
    out
}

// ---------------------------------------------------------------------------------------------
// Disk and network throughput
// ---------------------------------------------------------------------------------------------

/// Total (read, written) bytes across the disks `is_disk` accepts. `/proc/diskstats` also lists
/// every partition, so counting all its rows would double the figures.
pub fn parse_diskstats(text: &str, is_disk: &dyn Fn(&str) -> bool) -> (u64, u64) {
    const SECTOR: u64 = 512;
    let (mut r, mut w) = (0, 0);
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() >= 10 && is_disk(f[2]) {
            r += f[5].parse::<u64>().unwrap_or(0) * SECTOR;
            w += f[9].parse::<u64>().unwrap_or(0) * SECTOR;
        }
    }
    (r, w)
}

/// System-wide (read, written) bytes since boot. Unlike the per-process counters this includes
/// other users' processes, which `/proc/<pid>/io` hides.
pub fn disk_counters() -> Option<(u64, u64)> {
    let is_disk = |name: &str| {
        !["loop", "ram", "zram", "dm-", "md", "sr", "fd"].iter().any(|p| name.starts_with(p)) && Path::new("/sys/block").join(name).exists()
    };
    Some(parse_diskstats(&read("/proc/diskstats")?, &is_disk))
}

/// `pid=N` of the first process in an `ss -p` `users:((...))` column.
fn ss_pid(line: &str) -> Option<i32> {
    let rest = line.split("pid=").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

fn ss_counter(line: &str, name: &str) -> u64 {
    line.split_whitespace().find_map(|t| t.strip_prefix(name)?.parse().ok()).unwrap_or(0)
}

/// Per-pid (received, sent) byte totals over the open sockets in `ss -t -i -p -n` output, where
/// each socket's counters sit on the indented line under it.
pub fn parse_ss(text: &str) -> HashMap<i32, (u64, u64)> {
    let mut out: HashMap<i32, (u64, u64)> = HashMap::new();
    let mut cur = None;
    for l in text.lines() {
        if l.starts_with([' ', '\t']) {
            if let Some(pid) = cur.take() {
                let e = out.entry(pid).or_default();
                e.0 += ss_counter(l, "bytes_received:");
                e.1 += ss_counter(l, "bytes_sent:");
            }
        } else {
            cur = ss_pid(l);
        }
    }
    out
}

/// Per-process network totals. Only our own processes' sockets are visible without root, and a
/// closed socket drops out of the totals, so this is a close estimate rather than an exact count.
pub fn socket_counters() -> Option<HashMap<i32, (u64, u64)>> {
    Some(parse_ss(&tool("ss", &["-H", "-t", "-i", "-p", "-n", "state", "established"], 5)?))
}

// ---------------------------------------------------------------------------------------------
// Desktop entries: what a `.app` bundle is on macOS
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DesktopEntry {
    pub name: String,
    pub exec: String,
    pub icon: String,
    pub hidden: bool,
}

/// The `[Desktop Entry]` group of a `.desktop` file; `None` unless it describes an application.
pub fn parse_desktop(text: &str) -> Option<DesktopEntry> {
    let mut e = DesktopEntry::default();
    let (mut in_group, mut is_app) = (false, false);
    for l in text.lines().map(str::trim) {
        if l.starts_with('[') {
            in_group = l == "[Desktop Entry]";
        } else if in_group && let Some((k, v)) = l.split_once('=') {
            let v = v.trim();
            match k.trim() {
                "Type" => is_app = v == "Application",
                "Name" => e.name = v.to_string(),
                "Exec" => e.exec = v.to_string(),
                "Icon" => e.icon = v.to_string(),
                "NoDisplay" | "Hidden" if v == "true" => e.hidden = true,
                _ => {}
            }
        }
    }
    (is_app && !e.name.is_empty()).then_some(e)
}

/// Split an `Exec=` line on spaces, honouring double quotes.
fn exec_tokens(exec: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    for c in exec.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The program an `Exec=` line runs, past `env VAR=value` prefixes.
pub fn exec_program(exec: &str) -> Option<String> {
    let tokens = exec_tokens(exec);
    let mut it = tokens.iter().peekable();
    if it.peek().is_some_and(|t| Path::new(t.as_str()).file_name().is_some_and(|n| n == "env")) {
        it.next();
        while it.peek().is_some_and(|t| t.contains('=') || t.starts_with('-')) {
            it.next();
        }
    }
    it.next().cloned()
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Runtimes and launchers: many unrelated apps start through these, so a matching executable says
/// nothing about which app it is.
const GENERIC_PROGRAMS: [&str; 17] =
    ["sh", "bash", "dash", "zsh", "env", "python", "python3", "java", "node", "electron", "flatpak", "snap", "wine", "mono", "perl", "ruby", "gjs"];

/// Directories shared by unrelated programs: an executable living here is no sign of belonging.
fn is_shared_dir(dir: &str) -> bool {
    matches!(dir, "/" | "/bin" | "/sbin" | "/usr" | "/usr/bin" | "/usr/sbin" | "/usr/lib" | "/usr/lib64" | "/usr/libexec" | "/usr/local/bin" | "/usr/local/lib" | "/opt" | "/snap/bin" | "/usr/games")
        || dir.ends_with("/.local/bin")
        || dir.starts_with("/usr/lib/") && dir.matches('/').count() <= 3 && dir.ends_with("-linux-gnu")
}

/// Finds the desktop entry a process belongs to. Paths are the "bundle paths" of the app model.
#[derive(Debug, Default)]
pub struct DesktopIndex {
    by_id: HashMap<String, String>,
    by_program: HashMap<String, String>,
    by_dir: HashMap<String, String>,
}

impl DesktopIndex {
    /// `entries` are in priority order: the first file wins, as the XDG lookup rules say.
    pub fn from_entries(entries: impl IntoIterator<Item = (String, DesktopEntry, Option<String>)>) -> Self {
        let mut ix = Self::default();
        for (path, e, resolved) in entries {
            if e.hidden {
                continue;
            }
            let id = base_name(&path).trim_end_matches(".desktop").to_string();
            ix.by_id.entry(id).or_insert_with(|| path.clone());
            if let Some(program) = exec_program(&e.exec) {
                let name = base_name(&program).to_string();
                if !GENERIC_PROGRAMS.contains(&name.as_str()) {
                    ix.by_program.entry(name).or_insert_with(|| path.clone());
                }
                // An app's real binary often differs from its launcher (google-chrome → chrome),
                // but they share a private install directory.
                if let Some(dir) = resolved.as_deref().unwrap_or(&program).rsplit_once('/').map(|x| x.0).filter(|d| !is_shared_dir(d)) {
                    ix.by_dir.entry(dir.to_string()).or_insert_with(|| path.clone());
                }
            }
        }
        ix
    }

    /// The app that owns `exe`, by program name or by install directory.
    pub fn match_exe(&self, exe: &str) -> Option<&str> {
        let exe = exe.strip_suffix(" (deleted)").unwrap_or(exe);
        if exe.is_empty() {
            return None;
        }
        if let Some(p) = self.by_program.get(base_name(exe)) {
            return Some(p);
        }
        let mut dir = exe;
        for _ in 0..4 {
            dir = dir.rsplit_once('/')?.0;
            if is_shared_dir(dir) {
                return None;
            }
            if let Some(p) = self.by_dir.get(dir) {
                return Some(p);
            }
        }
        None
    }

    /// The app named by a [`process_hint`].
    pub fn match_hint(&self, hint: &str) -> Option<&str> {
        match hint.strip_prefix(APPIMAGE_HINT) {
            Some(image) => self.by_program.get(image).map(String::as_str),
            None => self.match_unit(hint),
        }
    }

    /// The app named by a cgroup unit (see [`unit_candidates`]).
    pub fn match_unit(&self, unit: &str) -> Option<&str> {
        unit_candidates(unit).iter().find_map(|c| self.by_id.get(c)).map(String::as_str)
    }

    /// Every `.desktop` file on this machine.
    pub fn scan() -> Self {
        Self::from_entries(application_dirs().into_iter().flat_map(|d| desktop_files(&d)).filter_map(|path| {
            let e = parse_desktop(&read(&path)?)?;
            let resolved = exec_program(&e.exec).and_then(|p| resolve_program(&p));
            Some((path, e, resolved))
        }))
    }
}

fn desktop_files(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<String> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "desktop")).map(|p| p.to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

/// Base data directories, most specific first ($XDG_DATA_HOME, then $XDG_DATA_DIRS, then Flatpak
/// and Snap exports).
fn data_dirs() -> Vec<PathBuf> {
    let home = crate::paths::home();
    let mut v = vec![std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"))];
    let dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    v.extend(dirs.split(':').map(PathBuf::from));
    v.push(home.join(".local/share/flatpak/exports/share"));
    v.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    v.push(PathBuf::from("/var/lib/snapd/desktop"));
    v
}

fn application_dirs() -> Vec<PathBuf> {
    data_dirs().into_iter().map(|d| d.join("applications")).collect()
}

/// `program` with symlinks resolved, looked up on `$PATH` when it has no directory.
fn resolve_program(program: &str) -> Option<String> {
    let path = if program.contains('/') {
        PathBuf::from(program)
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(program)).find(|p| p.is_file())?
    };
    Some(std::fs::canonicalize(path).ok()?.to_string_lossy().into_owned())
}

/// Lets the app model treat a `.desktop` file like a bundle.
pub fn read_desktop_info(path: &str) -> BundleInfo {
    let id = base_name(path).trim_end_matches(".desktop").to_string();
    let name = read(path).and_then(|t| parse_desktop(&t)).map(|e| e.name).unwrap_or_else(|| id.clone());
    BundleInfo { path: path.to_string(), bundle_id: Some(id), name, version: None }
}

/// The systemd unit a process runs in: the last part of its cgroup path.
pub fn parse_cgroup_unit(cgroup: &str) -> Option<String> {
    let line = cgroup.lines().find(|l| l.starts_with("0::")).or_else(|| cgroup.lines().next())?;
    let unit = line.rsplit('/').next()?;
    (unit.ends_with(".scope") || unit.ends_with(".service")).then(|| unit.to_string())
}

/// Desktop ids a unit name may refer to, longest first. Desktops start apps in units like
/// `app-gnome-org.gnome.Nautilus-1234.scope`, `app-flatpak-org.foo.Bar-1.scope`,
/// `app-firefox@ab12.service` or `snap.firefox.firefox-ab12.scope`, and D-Bus activated ones in
/// `dbus-:1.2-org.gnome.Terminal@0.service`. systemd escapes dashes inside a name as `\x2d`.
pub fn unit_candidates(unit: &str) -> Vec<String> {
    let name = unit.trim_end_matches(".scope").trim_end_matches(".service");
    let name = name.split('@').next().unwrap_or(name);
    if let Some(rest) = name.strip_prefix("snap.") {
        let mut parts = rest.split('.');
        return match (parts.next(), parts.next()) {
            (Some(snap), Some(app)) => vec![format!("{snap}_{}", app.split('-').next().unwrap_or(app))],
            _ => vec![],
        };
    }
    let parts: Vec<String> = name.split('-').map(|p| p.replace("\\x2d", "-")).collect();
    let mut out = Vec::new();
    for len in (1..=parts.len()).rev() {
        for w in parts.windows(len) {
            out.push(w.join("-"));
        }
    }
    out.retain(|c| !c.is_empty() && c != "app");
    out
}

/// The unit a process runs in, when it could name an app.
pub fn process_unit(pid: i32) -> Option<String> {
    parse_cgroup_unit(&read(format!("/proc/{pid}/cgroup"))?).filter(|u| !u.starts_with("vte-spawn") && !u.starts_with("session-"))
}

/// The file name of the AppImage whose mount holds `exe`, from `/proc/self/mountinfo`. An AppImage's
/// binaries live in a temporary FUSE mount the desktop entry knows nothing about, while the entry
/// names the image. (`$APPIMAGE` won't do: apps clear it, and their children inherit it.)
pub fn parse_appimage(exe: &str, mountinfo: &str) -> Option<String> {
    mountinfo.lines().find_map(|line| {
        let (mount, rest) = line.split_once(" - ")?;
        let point = mount.split_whitespace().nth(4)?.replace("\\040", " ");
        let mut rest = rest.split_whitespace();
        let (fstype, source) = (rest.next()?, rest.next()?);
        (point.rsplit('/').next()?.starts_with(".mount_") && fstype.starts_with("fuse.") && exe.strip_prefix(point.as_str())?.starts_with('/')).then(|| source.replace("\\040", " "))
    })
}

const APPIMAGE_HINT: &str = "appimage:";

/// What names a process's app beyond its executable: its AppImage or its unit.
pub fn process_hint(pid: i32) -> Option<String> {
    let appimage = || {
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        let exe = exe.to_str()?;
        if !exe.contains("/.mount_") {
            return None;
        }
        parse_appimage(exe, &read("/proc/self/mountinfo")?).map(|image| format!("{APPIMAGE_HINT}{image}"))
    };
    appimage().or_else(|| process_unit(pid))
}

/// Icon of a desktop entry as a PNG path. SVG-only icons are skipped: the app decodes PNG only.
pub fn desktop_icon_png(desktop: &str) -> Option<PathBuf> {
    let icon = parse_desktop(&read(desktop)?)?.icon;
    if icon.is_empty() {
        return None;
    }
    if icon.starts_with('/') {
        return icon.ends_with(".png").then(|| PathBuf::from(&icon)).filter(|p| p.exists());
    }
    let file = format!("{icon}.png");
    for base in data_dirs() {
        for size in ["256x256", "128x128", "96x96", "64x64", "48x48", "512x512", "32x32"] {
            let p = base.join("icons/hicolor").join(size).join("apps").join(&file);
            if p.exists() {
                return Some(p);
            }
        }
        let p = base.join("pixmaps").join(&file);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------------------------

/// `nvidia-smi --query-gpu=name,utilization.gpu,memory.used,memory.total,power.draw
/// --format=csv,noheader,nounits`. Memory is in MiB; a field may be `[N/A]`.
pub fn parse_nvidia_smi(csv: &str) -> Option<GpuMetrics> {
    let f: Vec<&str> = csv.lines().next()?.split(',').map(str::trim).collect();
    let num = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok());
    const MIB: f64 = 1024.0 * 1024.0;
    Some(GpuMetrics {
        available: true,
        name: f.first()?.to_string(),
        utilization_pct: num(1).unwrap_or(0.0),
        vram_used: (num(2).unwrap_or(0.0) * MIB) as u64,
        vram_total: (num(3).unwrap_or(0.0) * MIB) as u64,
        power_w: num(4),
    })
}

fn gpu_vendor(id: &str) -> &'static str {
    match id.trim() {
        "0x1002" => "AMD GPU",
        "0x8086" => "Intel GPU",
        "0x10de" => "NVIDIA GPU",
        "0x106b" => "Apple GPU",
        _ => "GPU",
    }
}

/// GPU metrics from one DRM device's attributes, looked up through `attr`. amdgpu exposes load and
/// VRAM; Intel and Asahi only tell us the GPU is there, which is "no statistics", not 0 % load.
/// Devices without a PCI vendor file (device tree) are named by their kernel driver.
pub fn parse_sysfs_gpu(attr: &dyn Fn(&str) -> Option<String>, driver: Option<&str>) -> GpuMetrics {
    let num = |f: &str| attr(f).and_then(|s| s.trim().parse::<u64>().ok());
    let busy = num("gpu_busy_percent");
    let name = match attr("vendor") {
        Some(v) => gpu_vendor(&v),
        None if driver.is_some_and(|d| d.starts_with("asahi")) => "Apple GPU",
        None => "GPU",
    };
    GpuMetrics {
        available: busy.is_some(),
        name: name.to_string(),
        utilization_pct: busy.unwrap_or(0) as f64,
        vram_used: num("mem_info_vram_used").unwrap_or(0),
        vram_total: num("mem_info_vram_total").unwrap_or(0),
        power_w: None,
    }
}

fn sysfs_gpu() -> Option<GpuMetrics> {
    let mut cards: Vec<PathBuf> = std::fs::read_dir("/sys/class/drm").ok()?.flatten().map(|e| e.path().join("device")).collect();
    cards.retain(|p| p.parent().and_then(|c| c.file_name()).and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("card") && !n.contains('-')));
    cards.sort();
    // With several cards, prefer one that reports its load over whichever enumerated first.
    let dev = cards.iter().find(|d| d.join("gpu_busy_percent").exists()).or(cards.first())?;
    let driver = std::fs::read_link(dev.join("driver")).ok();
    let driver = driver.as_deref().and_then(Path::file_name).and_then(|n| n.to_str());
    Some(parse_sysfs_gpu(&|f| read(dev.join(f)), driver))
}

pub fn read_gpu() -> Option<GpuMetrics> {
    let args = ["--query-gpu=name,utilization.gpu,memory.used,memory.total,power.draw", "--format=csv,noheader,nounits"];
    tool("nvidia-smi", &args, 3).and_then(|o| parse_nvidia_smi(&o)).or_else(sysfs_gpu)
}

// ---------------------------------------------------------------------------------------------
// Energy (RAPL): CPU package power, usually readable only by root
// ---------------------------------------------------------------------------------------------

pub fn rapl_energy_uj() -> Option<u64> {
    read("/sys/class/powercap/intel-rapl:0/energy_uj")?.trim().parse().ok()
}

// ---------------------------------------------------------------------------------------------
// Fans (hwmon)
// ---------------------------------------------------------------------------------------------

pub fn read_fans() -> Vec<Fan> {
    let mut fans = Vec::new();
    let Ok(rd) = std::fs::read_dir("/sys/class/hwmon") else { return fans };
    let mut mons: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    mons.sort();
    for mon in mons {
        for i in 1..=8 {
            let num = |suffix: &str| read(mon.join(format!("fan{i}_{suffix}"))).and_then(|s| s.trim().parse::<f64>().ok());
            if let Some(rpm) = num("input") {
                fans.push(Fan { index: fans.len(), rpm: rpm.max(0.0), min_rpm: num("min").unwrap_or(0.0), max_rpm: num("max").unwrap_or(0.0) });
            }
        }
    }
    fans
}

// ---------------------------------------------------------------------------------------------
// Battery
// ---------------------------------------------------------------------------------------------

/// Battery metrics from one power-supply directory's attributes, looked up through `attr`. Sysfs
/// reports µWh/µAh/µW/µA/µV and tenths of a degree. `power_w` is positive while discharging.
pub fn parse_battery(attr: &dyn Fn(&str) -> Option<String>, on_ac: bool) -> BatteryMetrics {
    let num = |k: &str| attr(k).and_then(|v| v.trim().parse::<f64>().ok());
    let status = attr("status").unwrap_or_default();
    let status = status.trim();
    let charging = status == "Charging";
    let discharging = status == "Discharging";
    // Whichever of energy (µWh) or charge (µAh) this battery reports, ratios work the same.
    let (now, full, design) = match num("energy_now") {
        Some(e) => (Some(e), num("energy_full"), num("energy_full_design")),
        None => (num("charge_now"), num("charge_full"), num("charge_full_design")),
    };
    let draw = num("power_now").or_else(|| Some(num("current_now")?.abs() * num("voltage_now")? / 1e6)).map(|uw| uw.abs() / 1e6);
    // In the charge_* case compare with the current in µA, so the unit cancels too.
    // Some drivers (Asahi's macsmc) report the draw as a negative number while discharging.
    let rate = if num("energy_now").is_some() { num("power_now") } else { num("current_now") }.map(f64::abs).filter(|r| *r > 0.0);
    let percent = num("capacity").or_else(|| Some(now? * 100.0 / full.filter(|f| *f > 0.0)?)).unwrap_or(0.0);
    BatteryMetrics {
        present: attr("present").is_none_or(|p| p.trim() == "1"),
        percent: percent.clamp(0.0, 100.0),
        charging,
        on_ac,
        time_remaining_min: if discharging { rate.and_then(|r| Some((now? / r * 60.0) as i64)).filter(|m| *m > 0) } else { None },
        power_w: draw.filter(|w| *w > 0.0).map(|w| if charging { -w } else { w }),
        cycle_count: num("cycle_count").map(|c| c as i64).filter(|c| *c > 0),
        health_pct: full.zip(design).filter(|(_, d)| *d > 0.0).map(|(f, d)| (f * 100.0 / d).min(100.0)),
        temperature_c: num("temp").map(|t| t / 10.0),
    }
}

pub fn read_battery() -> Option<BatteryMetrics> {
    let mut supplies: Vec<PathBuf> = std::fs::read_dir("/sys/class/power_supply").ok()?.flatten().map(|e| e.path()).collect();
    supplies.sort();
    let attr = |d: &Path, k: &str| read(d.join(k));
    let is = |d: &Path, kind: &str| attr(d, "type").is_some_and(|t| t.trim() == kind);
    let on_ac = supplies.iter().any(|d| (is(d, "Mains") || is(d, "USB")) && attr(d, "online").is_some_and(|o| o.trim() == "1"));
    // scope=Device marks peripherals (a mouse's battery), which aren't the machine's.
    let bat = supplies.iter().find(|d| is(d, "Battery") && attr(d, "scope").is_none_or(|s| s.trim() != "Device"))?;
    Some(parse_battery(&|k| attr(bat, k), on_ac))
}

// ---------------------------------------------------------------------------------------------
// Audio (pactl: PulseAudio or PipeWire's pulse server)
// ---------------------------------------------------------------------------------------------

/// `Default Sink:` / `Default Source:` from `pactl info`.
fn pactl_default(info: &str, key: &str) -> Option<String> {
    info.lines().find_map(|l| Some(l.strip_prefix(key)?.trim().to_string()))
}

/// The first percentage in a `Volume:` line, as 0..1.
fn pactl_volume(line: &str) -> Option<f64> {
    let pct = line.split('/').map(str::trim).find(|p| p.ends_with('%'))?;
    Some(pct.trim_end_matches('%').trim().parse::<f64>().ok()? / 100.0)
}

#[derive(Debug, Default, PartialEq)]
pub struct PactlDevice {
    pub description: Option<String>,
    pub volume: Option<f64>,
    pub muted: bool,
}

/// The device called `name` in `pactl list sinks` / `pactl list sources`.
pub fn parse_pactl_device(list: &str, name: &str) -> Option<PactlDevice> {
    let mut cur: Option<PactlDevice> = None;
    let mut found = false;
    for l in list.lines().map(str::trim) {
        if l.starts_with("Sink #") || l.starts_with("Source #") {
            if found {
                break;
            }
            cur = Some(PactlDevice::default());
        } else if let Some(n) = l.strip_prefix("Name: ") {
            found = n.trim() == name;
        } else if let Some(d) = cur.as_mut() {
            if let Some(v) = l.strip_prefix("Description: ") {
                d.description = Some(v.trim().to_string());
            } else if let Some(v) = l.strip_prefix("Mute: ") {
                d.muted = v.trim() == "yes";
            } else if l.starts_with("Volume: ") && d.volume.is_none() {
                d.volume = pactl_volume(l);
            }
        }
    }
    if found { cur } else { None }
}

/// (pid, active) per stream in `pactl list sink-inputs` / `source-outputs`. `active` means the
/// stream is neither corked nor muted.
pub fn parse_pactl_streams(list: &str) -> Vec<(i32, bool)> {
    let mut out = Vec::new();
    let mut cur: Option<(Option<i32>, bool)> = None;
    let mut flush = |c: Option<(Option<i32>, bool)>| {
        if let Some((Some(pid), active)) = c {
            out.push((pid, active));
        }
    };
    for l in list.lines().map(str::trim) {
        if l.starts_with("Sink Input #") || l.starts_with("Source Output #") {
            flush(cur.take());
            cur = Some((None, true));
        } else if let Some(c) = cur.as_mut() {
            if l == "Corked: yes" || l == "Mute: yes" {
                c.1 = false;
            } else if let Some(v) = l.strip_prefix("application.process.id = ") {
                c.0 = v.trim_matches('"').parse().ok();
            }
        }
    }
    flush(cur);
    out
}

pub fn read_audio() -> AudioMetrics {
    let mut a = AudioMetrics::default();
    let Some(info) = tool("pactl", &["info"], 2) else { return a };
    if let Some(sink) = pactl_default(&info, "Default Sink:")
        && let Some(d) = tool("pactl", &["list", "sinks"], 2).and_then(|l| parse_pactl_device(&l, &sink))
    {
        a.output_device = d.description;
        a.output_volume = d.volume;
        a.output_muted = d.muted;
    }
    if let Some(source) = pactl_default(&info, "Default Source:")
        && let Some(d) = tool("pactl", &["list", "sources"], 2).and_then(|l| parse_pactl_device(&l, &source))
    {
        a.input_volume = d.volume;
    }
    let mut apps: HashMap<i32, AudioApp> = HashMap::new();
    for (pid, active) in tool("pactl", &["list", "sink-inputs"], 2).map(|l| parse_pactl_streams(&l)).unwrap_or_default() {
        let state = match (active, a.output_muted) {
            (false, _) => AudioState::Silent,
            (true, true) => AudioState::Muted,
            (true, false) => AudioState::Playing,
        };
        apps.entry(pid).and_modify(|x| x.state = if state == AudioState::Playing { state } else { x.state }).or_insert(AudioApp {
            pid,
            app_name: String::new(),
            bundle_id: None,
            state,
            using_mic: false,
        });
    }
    for (pid, active) in tool("pactl", &["list", "source-outputs"], 2).map(|l| parse_pactl_streams(&l)).unwrap_or_default() {
        if active {
            apps.entry(pid)
                .or_insert(AudioApp { pid, app_name: String::new(), bundle_id: None, state: AudioState::Silent, using_mic: false })
                .using_mic = true;
        }
    }
    a.apps = apps.into_values().collect();
    a
}

/// Set the default output (or input) volume, 0..1.
pub fn set_volume(input: bool, v: f64) -> bool {
    let (cmd, dev) = if input { ("set-source-volume", "@DEFAULT_SOURCE@") } else { ("set-sink-volume", "@DEFAULT_SINK@") };
    let pct = format!("{:.0}%", (v * 100.0).clamp(0.0, 100.0));
    sys::run_full("pactl", &[cmd, dev, &pct], Duration::from_secs(2)).is_some()
}

pub fn set_muted(muted: bool) -> bool {
    sys::run_full("pactl", &["set-sink-mute", "@DEFAULT_SINK@", if muted { "1" } else { "0" }], Duration::from_secs(2)).is_some()
}

// ---------------------------------------------------------------------------------------------
// Bluetooth (bluetoothctl)
// ---------------------------------------------------------------------------------------------

/// `Device AA:BB:CC:DD:EE:FF Name with spaces` lines of `bluetoothctl devices`.
pub fn parse_bt_devices(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut p = l.trim().strip_prefix("Device ")?.splitn(2, ' ');
            let addr = p.next()?.to_string();
            Some((addr, p.next().unwrap_or("").trim().to_string()))
        })
        .collect()
}

/// A device from `bluetoothctl info <address>`.
pub fn parse_bt_info(address: &str, fallback_name: &str, text: &str) -> BluetoothDevice {
    let get = |k: &str| text.lines().find_map(|l| Some(l.trim().strip_prefix(k)?.trim().to_string()));
    BluetoothDevice {
        name: get("Name:").or_else(|| get("Alias:")).unwrap_or_else(|| fallback_name.to_string()),
        address: address.to_string(),
        kind: get("Icon:").unwrap_or_default(),
        connected: get("Connected:").as_deref() == Some("yes"),
        // `Battery Percentage: 0x55 (85)`
        battery_main: get("Battery Percentage:").and_then(|v| v.split('(').nth(1)?.trim_end_matches(')').parse().ok()),
        battery_left: None,
        battery_right: None,
        battery_case: None,
    }
}

/// Without an adapter there's nothing to ask. `bluetoothctl` also waits forever when bluetoothd
/// isn't running (even with its `--timeout`), which `sys::run` handles: its timeout, or the kernel
/// when this process exits first.
pub fn read_bluetooth() -> Option<BluetoothMetrics> {
    if std::fs::read_dir("/sys/class/bluetooth").map_or(true, |mut d| d.next().is_none()) {
        return Some(BluetoothMetrics::default());
    }
    let ctl = |args: &[&str]| tool("bluetoothctl", args, 5);
    let show = ctl(&["show"])?;
    let powered = show.lines().any(|l| l.trim() == "Powered: yes");
    let devices = ctl(&["devices"]).map(|o| parse_bt_devices(&o)).unwrap_or_default();
    let devices = devices.iter().filter_map(|(addr, name)| Some(parse_bt_info(addr, name, &ctl(&["info", addr])?))).collect();
    Some(BluetoothMetrics { powered, devices })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_ticks_fold_iowait_and_irq() {
        let t = parse_cpu_ticks("cpu  10 2 30 400 5 6 7 8 0 0\ncpu0 1 1 1 1\n").unwrap();
        assert_eq!(t, [10, 30 + 6 + 7 + 8, 400 + 5, 2]);
        assert_eq!(parse_cpu_ticks("intr 1 2 3"), None);
    }

    #[test]
    fn meminfo_categories() {
        let text = "MemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 300 kB\nSReclaimable: 40 kB\nShmem: 20 kB\nAnonPages: 400 kB\nSUnreclaim: 30 kB\nKernelStack: 10 kB\nPageTables: 5 kB\nZswap: 7 kB\n";
        let v = parse_meminfo(text).unwrap();
        assert_eq!(v.app, 420 * 1024);
        assert_eq!(v.wired, 45 * 1024);
        assert_eq!(v.compressed, 7 * 1024);
        // Shared memory counts as app, so it must not count as cache too.
        assert_eq!(v.cached, (50 + 300 + 40 - 20) * 1024);
        assert_eq!(v.free, 100 * 1024);
        assert!(parse_meminfo("Nonsense: 1 kB").is_none());
    }

    #[test]
    fn stat_times_survive_odd_process_names() {
        let stat = "123 (we ird) (name)) S 1 123 123 0 -1 4194304 100 0 0 0 77 33 0 0 20 0 1 0 5 1000 10";
        assert_eq!(parse_stat_times(stat), Some((77, 33)));
    }

    #[test]
    fn proc_io_and_peak_memory() {
        assert_eq!(parse_proc_io("rchar: 9\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 1\n"), (4096, 8192));
        assert_eq!(parse_vm_hwm("Name:\tx\nVmHWM:\t  2048 kB\nVmRSS:\t1 kB\n"), 2048 * 1024);
        let status = "VmRSS:\t 9000 kB\nRssAnon:\t 100 kB\nRssFile:\t 8900 kB\nVmSwap:\t 20 kB\n";
        assert_eq!(parse_footprint(status), 120 * 1024, "shared file pages don't count");
        assert_eq!(parse_footprint("Name:\tkworker\n"), 0);
    }

    #[test]
    fn elf_machines() {
        let mut h = [0u8; 20];
        h[..4].copy_from_slice(b"\x7fELF");
        h[5] = 1;
        h[18] = 62;
        assert_eq!(elf_arch(&h), Some("x86_64"));
        h[18] = 183;
        assert_eq!(elf_arch(&h), Some("arm64"));
        assert_eq!(elf_arch(b"#!/bin/sh\n0123456789"), None);
    }

    #[test]
    fn tcp_table_keeps_my_listeners_only() {
        let v4 = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
           0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 111 1\n\
           1: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 222 1\n\
           2: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 333 1\n\
           3: 0100007F:0BB8 0100007F:D000 01 00000000:00000000 00:00000000 00000000  1000        0 444 1\n";
        let m = parse_net_tcp(v4, 1000);
        assert_eq!(m.len(), 2, "not root's socket, not an established one: {m:?}");
        assert_eq!(m[&111], ("127.0.0.1".to_string(), 3000));
        assert_eq!(m[&222], ("*".to_string(), 8080));
    }

    #[test]
    fn ipv6_addresses_decode() {
        assert_eq!(decode_addr("00000000000000000000000000000000").unwrap(), "*");
        assert_eq!(decode_addr("00000000000000000000000001000000").unwrap(), "[::1]");
        // ::ffff:127.0.0.1
        assert_eq!(decode_addr("0000000000000000FFFF00000100007F").unwrap(), "127.0.0.1");
    }

    #[test]
    fn diskstats_counts_only_whole_disks() {
        let t = "   8       0 sda 1 0 100 0 2 0 200 0 0 0 0\n   8       1 sda1 1 0 100 0 2 0 200 0 0 0 0\n 259 0 nvme0n1 1 0 10 0 2 0 20 0 0 0 0\n";
        let (r, w) = parse_diskstats(t, &|n| n == "sda" || n == "nvme0n1");
        assert_eq!(r, 110 * 512);
        assert_eq!(w, 220 * 512);
    }

    #[test]
    fn ss_totals_per_pid() {
        let t = "0 0 10.0.0.1:1 10.0.0.2:2 users:((\"firefox\",pid=42,fd=7),(\"firefox\",pid=43,fd=7))\n\
                 \t cubic wscale:7,7 bytes_sent:100 bytes_acked:99 bytes_received:200 segs_out:1\n\
                 0 0 10.0.0.1:3 10.0.0.2:4 users:((\"firefox\",pid=42,fd=8))\n\
                 \t cubic bytes_sent:1 bytes_received:2\n\
                 0 0 10.0.0.1:5 10.0.0.2:6\n\
                 \t cubic bytes_sent:1000 bytes_received:1000\n";
        let m = parse_ss(t);
        assert_eq!(m[&42], (202, 101));
        assert_eq!(m.len(), 1, "a socket with no visible owner counts for nobody");
    }

    #[test]
    fn desktop_file_parsing() {
        let t = "[Desktop Entry]\nType=Application\nName=Firefox\nName[es]=Navegador\nExec=env MOZ=1 /usr/lib/firefox/firefox %u\nIcon=firefox\n\n[Desktop Action new]\nName=New Window\nExec=other\n";
        let e = parse_desktop(t).unwrap();
        assert_eq!(e.name, "Firefox");
        assert_eq!(exec_program(&e.exec).unwrap(), "/usr/lib/firefox/firefox");
        assert!(!e.hidden);
        assert!(parse_desktop("[Desktop Entry]\nType=Link\nName=x\n").is_none());
        assert!(parse_desktop("[Desktop Entry]\nType=Application\nName=x\nNoDisplay=true\n").unwrap().hidden);
        assert_eq!(exec_program("\"/opt/My App/app\" --flag").unwrap(), "/opt/My App/app");
    }

    fn entry(exec: &str) -> DesktopEntry {
        DesktopEntry { name: "x".into(), exec: exec.into(), ..Default::default() }
    }

    #[test]
    fn index_matches_program_directory_and_unit() {
        let ix = DesktopIndex::from_entries([
            ("/usr/share/applications/firefox.desktop".to_string(), entry("firefox %u"), None),
            ("/usr/share/applications/chrome.desktop".to_string(), entry("/usr/bin/google-chrome-stable"), Some("/opt/google/chrome/google-chrome".to_string())),
            ("/usr/share/applications/org.gnome.Terminal.desktop".to_string(), entry("gnome-terminal"), None),
            ("/usr/share/applications/script.desktop".to_string(), entry("python3 /opt/thing/main.py"), None),
            ("/usr/share/applications/hidden.desktop".to_string(), DesktopEntry { hidden: true, ..entry("gnome-shell") }, None),
        ]);
        assert_eq!(ix.match_exe("/usr/lib/firefox/firefox"), Some("/usr/share/applications/firefox.desktop"));
        assert_eq!(ix.match_exe("/usr/lib/firefox/firefox (deleted)"), Some("/usr/share/applications/firefox.desktop"));
        assert_eq!(ix.match_exe("/opt/google/chrome/chrome"), Some("/usr/share/applications/chrome.desktop"), "same private directory");
        assert_eq!(ix.match_exe("/usr/bin/python3"), None, "runtimes say nothing about the app");
        assert_eq!(ix.match_exe("/usr/bin/gnome-shell"), None, "NoDisplay entries are not apps");
        assert_eq!(ix.match_exe(""), None);
        assert_eq!(ix.match_unit("dbus-:1.2-org.gnome.Terminal@0.service"), Some("/usr/share/applications/org.gnome.Terminal.desktop"));
        assert_eq!(ix.match_unit("pipewire.service"), None);
    }

    #[test]
    fn appimage_processes_belong_to_their_image() {
        // From a real machine.
        let mountinfo = "73 1 0:30 / /tmp rw shared:5 - tmpfs tmpfs rw\n\
            248 73 0:68 / /tmp/.mount_T3-CodbJGEHN ro,nosuid,nodev,relatime shared:480 - fuse.T3-Code.AppImage T3-Code.AppImage ro,user_id=1000,group_id=1000\n\
            249 73 0:69 / /tmp/.mount_My\\040Apabc ro shared:481 - fuse.My\\040App.AppImage My\\040App.AppImage ro\n\
            250 73 0:70 / /home/me/remote rw shared:482 - fuse.sshfs me@host:/ rw\n";
        assert_eq!(parse_appimage("/tmp/.mount_T3-CodbJGEHN/t3code", mountinfo).as_deref(), Some("T3-Code.AppImage"));
        assert_eq!(parse_appimage("/tmp/.mount_T3-CodbJGEHN/resources/helper", mountinfo).as_deref(), Some("T3-Code.AppImage"));
        assert_eq!(parse_appimage("/tmp/.mount_My Apabc/app", mountinfo).as_deref(), Some("My App.AppImage"));
        assert_eq!(parse_appimage("/tmp/.mount_T3-CodbJGEHNother/t3code", mountinfo), None, "another directory");
        assert_eq!(parse_appimage("/home/me/remote/tool", mountinfo), None, "other FUSE mounts aren't AppImages");
        assert_eq!(parse_appimage("/usr/bin/claude", mountinfo), None);

        let path = "/home/me/.local/share/applications/t3code.desktop";
        let ix = DesktopIndex::from_entries([(path.to_string(), entry("\"/home/me/apps/T3-Code.AppImage\" %U"), None)]);
        assert_eq!(ix.match_hint("appimage:T3-Code.AppImage"), Some(path));
        assert_eq!(ix.match_hint("appimage:Other.AppImage"), None);
        assert_eq!(ix.match_hint("app-t3code@ab12.service"), Some(path), "anything else is a unit");
        assert_eq!(ix.match_exe("/tmp/.mount_T3-CodbJGEHN/t3code"), None, "the mount alone says nothing");
    }

    #[test]
    fn unit_names_to_desktop_ids() {
        let has = |unit: &str, id: &str| unit_candidates(unit).iter().any(|c| c == id);
        assert!(has("app-gnome-org.gnome.Nautilus-1234.scope", "org.gnome.Nautilus"));
        assert!(has("app-gnome-google\\x2dchrome-1234.scope", "google-chrome"));
        assert!(has("app-flatpak-org.mozilla.firefox-99.scope", "org.mozilla.firefox"));
        assert!(has("app-firefox@ab12cd.service", "firefox"));
        assert!(has("app-dbus\\x2d:1.2\\x2dorg.gnome.Terminal@0.service", "org.gnome.Terminal") || has("dbus-:1.2-org.gnome.Terminal@0.service", "org.gnome.Terminal"));
        assert_eq!(unit_candidates("snap.firefox.firefox-ab12.scope"), vec!["firefox_firefox".to_string()]);
        assert_eq!(parse_cgroup_unit("0::/user.slice/user-1000.slice/app.slice/app-gnome-x-1.scope\n").unwrap(), "app-gnome-x-1.scope");
        assert_eq!(parse_cgroup_unit("0::/\n"), None);
    }

    #[test]
    fn nvidia_csv() {
        let g = parse_nvidia_smi("NVIDIA GeForce RTX 4070, 37, 1024, 12288, [N/A]\n").unwrap();
        assert_eq!(g.name, "NVIDIA GeForce RTX 4070");
        assert_eq!(g.utilization_pct, 37.0);
        assert_eq!(g.vram_used, 1024 * 1024 * 1024);
        assert_eq!(g.power_w, None);
        assert!(g.available);
    }

    fn attrs<'a>(kv: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k: &str| kv.iter().find(|(a, _)| *a == k).map(|(_, v)| format!("{v}\n"))
    }

    #[test]
    fn battery_from_energy_attributes() {
        let a = attrs(&[
            ("status", "Discharging"), ("capacity", "75"), ("energy_now", "30000000"), ("energy_full", "40000000"),
            ("energy_full_design", "50000000"), ("power_now", "10000000"), ("cycle_count", "120"), ("present", "1"),
        ]);
        let b = parse_battery(&a, false);
        assert!(b.present && !b.charging && !b.on_ac);
        assert_eq!(b.percent, 75.0);
        assert_eq!(b.power_w, Some(10.0));
        assert_eq!(b.health_pct, Some(80.0));
        assert_eq!(b.cycle_count, Some(120));
        assert_eq!(b.time_remaining_min, Some(180), "30 Wh at 10 W");
    }

    #[test]
    fn battery_with_negative_power_while_discharging() {
        // Asahi's macsmc driver, as read from a real M3 Pro.
        let a = attrs(&[("status", "Discharging"), ("capacity", "100"), ("energy_now", "60000000"), ("power_now", "-10000000")]);
        let b = parse_battery(&a, false);
        assert_eq!(b.power_w, Some(10.0));
        assert_eq!(b.time_remaining_min, Some(360), "60 Wh at 10 W");
    }

    #[test]
    fn sysfs_gpu_without_load_is_not_available() {
        // Asahi: a device-tree device with no statistics.
        let g = parse_sysfs_gpu(&attrs(&[]), Some("asahi-g15"));
        assert!(!g.available, "no reading is not 0 % load");
        assert_eq!(g.name, "Apple GPU");
        // Intel: a PCI vendor, no statistics.
        let g = parse_sysfs_gpu(&attrs(&[("vendor", "0x8086")]), Some("i915"));
        assert!(!g.available);
        assert_eq!(g.name, "Intel GPU");
        // amdgpu idling really is 0 %.
        let g = parse_sysfs_gpu(&attrs(&[("vendor", "0x1002"), ("gpu_busy_percent", "0"), ("mem_info_vram_used", "1024")]), Some("amdgpu"));
        assert!(g.available);
        assert_eq!(g.name, "AMD GPU");
        assert_eq!(g.utilization_pct, 0.0);
        assert_eq!(g.vram_used, 1024);
    }

    #[test]
    fn battery_from_charge_attributes() {
        let a = attrs(&[
            ("status", "Charging"), ("charge_now", "2000000"), ("charge_full", "4000000"), ("charge_full_design", "4000000"),
            ("current_now", "-1000000"), ("voltage_now", "12000000"), ("cycle_count", "0"), ("temp", "305"),
        ]);
        let b = parse_battery(&a, true);
        assert!(b.charging && b.on_ac);
        assert_eq!(b.percent, 50.0, "derived without a capacity attribute");
        assert_eq!(b.power_w, Some(-12.0), "negative while charging, whatever sign the driver uses");
        assert_eq!(b.time_remaining_min, None, "only counts down while discharging");
        assert_eq!(b.cycle_count, None, "drivers report 0 when they don't know");
        assert_eq!(b.temperature_c, Some(30.5));
    }

    #[test]
    fn pactl_default_device() {
        let list = "Sink #1\n\tState: IDLE\n\tName: hdmi\n\tDescription: HDMI\n\tMute: no\n\tVolume: front-left: 1 / 10% / -60 dB\n\nSink #2\n\tName: speakers\n\tDescription: Built-in Audio\n\tMute: yes\n\tVolume: front-left: 26214 /  40% / -23.88 dB,   front-right: 26214 /  40% / -23.88 dB\n\t        balance 0.00\n";
        let d = parse_pactl_device(list, "speakers").unwrap();
        assert_eq!(d.description.as_deref(), Some("Built-in Audio"));
        assert_eq!(d.volume, Some(0.4));
        assert!(d.muted);
        assert_eq!(parse_pactl_device(list, "hdmi").unwrap().volume, Some(0.1));
        assert!(parse_pactl_device(list, "gone").is_none());
    }

    #[test]
    fn pactl_streams() {
        let list = "Sink Input #1\n\tCorked: no\n\tMute: no\n\tProperties:\n\t\tapplication.process.id = \"42\"\nSink Input #2\n\tCorked: yes\n\tMute: no\n\tProperties:\n\t\tapplication.process.id = \"43\"\nSink Input #3\n\tCorked: no\n";
        assert_eq!(parse_pactl_streams(list), vec![(42, true), (43, false)]);
    }

    #[test]
    fn bluetooth_devices() {
        assert_eq!(parse_bt_devices("Device AA:BB Sony WH-1000\nnoise\n"), vec![("AA:BB".to_string(), "Sony WH-1000".to_string())]);
        let d = parse_bt_info("AA:BB", "fallback", "Device AA:BB (public)\n\tName: Sony WH\n\tIcon: audio-headset\n\tConnected: yes\n\tBattery Percentage: 0x55 (85)\n");
        assert_eq!((d.name.as_str(), d.kind.as_str(), d.connected, d.battery_main), ("Sony WH", "audio-headset", true, Some(85.0)));
        let d = parse_bt_info("CC", "fallback", "Connected: no\n");
        assert_eq!((d.name.as_str(), d.connected, d.battery_main), ("fallback", false, None));
    }
}
