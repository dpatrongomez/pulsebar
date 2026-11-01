//! M2 — "Apps, not processes": attribute every process to the application responsible for it
//! and aggregate metrics per app.
//!
//! Resolution order for a process:
//! 1. its own executable lives inside an `.app` bundle (outermost bundle wins, so Chrome's
//!    nested helper apps resolve to "Google Chrome");
//! 2. the macOS *responsible* process (Activity Monitor's attribution: XPC services, Terminal
//!    children…) has a bundle;
//! 3. walk up the parent chain to the first ancestor with a bundle;
//! 4. otherwise: system daemons collapse into one "macOS" row, user-land CLI daemons group by
//!    executable name (e.g. "postgres").

use std::collections::HashMap;

use crate::model::{App, AppGroup, Process};
use crate::sys::{self, BundleCache, BundleInfo};

pub const SYSTEM_KEY: &str = "system:macos";
pub const SYSTEM_NAME: &str = "macOS";

pub trait BundleLookup {
    fn lookup(&mut self, bundle_path: &str) -> BundleInfo;
}

impl BundleLookup for BundleCache {
    fn lookup(&mut self, bundle_path: &str) -> BundleInfo {
        self.get(bundle_path).clone()
    }
}

/// Bundles under /System/Library are OS components (Dock, ControlCenter, …) except Finder.
fn is_system_bundle(path: &str) -> bool {
    (path.starts_with("/System/Library/") || path.starts_with("/usr/") || path.starts_with("/Library/Apple/"))
        && !path.ends_with("/Finder.app")
}

fn is_system_executable(p: &Process, my_uid: u32) -> bool {
    let e = &p.executable_path;
    p.uid != my_uid
        || e.is_empty()
        || e.starts_with("/System/")
        || e.starts_with("/usr/libexec/")
        || e.starts_with("/usr/sbin/")
        || e.starts_with("/sbin/")
        || e.starts_with("/Library/Apple/")
        || (e.starts_with("/usr/bin/") && p.ppid <= 1)
}

#[derive(Debug, Clone, PartialEq)]
enum Owner {
    Bundle(String),
    System,
    Named(String),
}

pub struct Grouper<L: BundleLookup> {
    pub lookup: L,
    pub my_uid: u32,
}

impl Grouper<BundleCache> {
    pub fn new() -> Self {
        Self { lookup: BundleCache::default(), my_uid: sys::current_uid() }
    }
}

impl Default for Grouper<BundleCache> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: BundleLookup> Grouper<L> {
    pub fn with_lookup(lookup: L, my_uid: u32) -> Self {
        Self { lookup, my_uid }
    }

    fn own_bundle(p: &Process) -> Option<String> {
        sys::outermost_bundle(&p.executable_path)
    }

    fn owner(&self, p: &Process, by_pid: &HashMap<i32, &Process>) -> Owner {
        if let Some(b) = Self::own_bundle(p) {
            return if is_system_bundle(&b) { Owner::System } else { Owner::Bundle(b) };
        }
        if p.responsible_pid > 1 && p.responsible_pid != p.pid
            && let Some(b) = by_pid.get(&p.responsible_pid).and_then(|r| Self::own_bundle(r))
                && !is_system_bundle(&b) {
                    return Owner::Bundle(b);
                }
        // Walk the parent chain (bounded, cycle-safe).
        let mut cur = p.ppid;
        let mut root = p;
        for _ in 0..64 {
            if cur <= 1 {
                break;
            }
            let Some(parent) = by_pid.get(&cur) else { break };
            if let Some(b) = Self::own_bundle(parent) {
                return if is_system_bundle(&b) { Owner::System } else { Owner::Bundle(b) };
            }
            root = parent;
            cur = parent.ppid;
        }
        if is_system_executable(root, self.my_uid) && is_system_executable(p, self.my_uid) {
            Owner::System
        } else {
            Owner::Named(root.name.clone())
        }
    }

    /// Assign `app_name`/`app_bundle_id` on every process and return one row per app,
    /// sorted by memory (desc).
    pub fn group(&mut self, procs: &mut [Process]) -> Vec<AppGroup> {
        let owners: Vec<Owner> = {
            let by_pid: HashMap<i32, &Process> = procs.iter().map(|p| (p.pid, p)).collect();
            procs.iter().map(|p| self.owner(p, &by_pid)).collect()
        };
        let mut groups: HashMap<String, AppGroup> = HashMap::new();
        for (p, owner) in procs.iter_mut().zip(owners) {
            let (key, app) = match owner {
                Owner::Bundle(path) => {
                    let info = self.lookup.lookup(&path);
                    let key = info.bundle_id.clone().map(|b| format!("bundle:{b}")).unwrap_or_else(|| format!("path:{path}"));
                    (
                        key,
                        App {
                            bundle_id: info.bundle_id,
                            name: info.name,
                            bundle_path: Some(path),
                            version: info.version,
                            signature_team_id: None,
                        },
                    )
                }
                Owner::System => (SYSTEM_KEY.to_string(), App { name: SYSTEM_NAME.into(), ..Default::default() }),
                Owner::Named(n) => (format!("name:{n}"), App { name: n, ..Default::default() }),
            };
            p.app_name = app.name.clone();
            p.app_bundle_id = app.bundle_id.clone();
            let g = groups.entry(key.clone()).or_insert_with(|| AppGroup { key, app, root_pid: p.pid, ..Default::default() });
            g.cpu_pct += p.cpu_pct;
            g.mem_bytes += p.mem_bytes;
            g.energy_w += p.energy_w;
            g.disk_read_bps += p.disk_read_bps;
            g.disk_write_bps += p.disk_write_bps;
            g.net_rx_bps += p.net_rx_bps;
            g.net_tx_bps += p.net_tx_bps;
            g.process_count += 1;
            g.pids.push(p.pid);
            // The root is the earliest-started process of the group (the main app process).
            if p.ppid <= 1 && Self::is_main_candidate(p, &g.app) {
                g.root_pid = p.pid;
            }
        }
        let mut v: Vec<AppGroup> = groups.into_values().collect();
        for g in &mut v {
            g.pids.sort_unstable();
        }
        v.sort_by(|a, b| b.mem_bytes.cmp(&a.mem_bytes).then(a.app.name.cmp(&b.app.name)));
        v
    }

    fn is_main_candidate(p: &Process, app: &App) -> bool {
        match &app.bundle_path {
            Some(b) => p.executable_path.starts_with(&format!("{b}/Contents/MacOS/")),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeLookup;
    impl BundleLookup for FakeLookup {
        fn lookup(&mut self, path: &str) -> BundleInfo {
            let name = path.rsplit('/').next().unwrap().trim_end_matches(".app").to_string();
            BundleInfo { path: path.into(), bundle_id: Some(format!("com.test.{}", name.replace(' ', ""))), name, version: None }
        }
    }

    fn p(pid: i32, ppid: i32, exe: &str, mem: u64) -> Process {
        Process {
            pid,
            ppid,
            uid: 501,
            name: exe.rsplit('/').next().unwrap().to_string(),
            executable_path: exe.into(),
            mem_bytes: mem,
            cpu_pct: 1.0,
            ..Default::default()
        }
    }

    #[test]
    fn chrome_helpers_collapse_into_one_row() {
        let chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        let helper = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/130/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)";
        let mut procs = vec![p(100, 1, chrome, 500)];
        for i in 0..95 {
            procs.push(p(200 + i, 100, helper, 100));
        }
        let mut g = Grouper::with_lookup(FakeLookup, 501);
        let groups = g.group(&mut procs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].app.name, "Google Chrome");
        assert_eq!(groups[0].process_count, 96);
        assert_eq!(groups[0].mem_bytes, 500 + 95 * 100);
        assert_eq!(groups[0].root_pid, 100);
    }

    #[test]
    fn terminal_children_and_daemons() {
        let mut procs = vec![
            p(10, 1, "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal", 100),
            p(11, 10, "/usr/bin/login", 1),
            p(12, 11, "/bin/zsh", 5),
            p(13, 12, "/opt/homebrew/bin/node", 300),
            p(20, 1, "/usr/libexec/syspolicyd", 10),
            p(21, 1, "/opt/homebrew/opt/postgresql/bin/postgres", 50),
            p(22, 21, "/opt/homebrew/opt/postgresql/bin/postgres", 20),
            p(30, 1, "/System/Library/CoreServices/Dock.app/Contents/MacOS/Dock", 40),
        ];
        let mut g = Grouper::with_lookup(FakeLookup, 501);
        let groups = g.group(&mut procs);
        let by: HashMap<_, _> = groups.iter().map(|g| (g.app.name.as_str(), g)).collect();
        assert_eq!(by["Terminal"].process_count, 4);
        assert_eq!(by["Terminal"].mem_bytes, 406);
        assert_eq!(by["macOS"].process_count, 2);
        assert_eq!(by["postgres"].process_count, 2);
        assert_eq!(procs[3].app_name, "Terminal");
    }

    #[test]
    fn responsible_pid_wins_over_launchd_parent() {
        let mut xpc = p(40, 1, "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent", 200);
        xpc.responsible_pid = 41;
        let mut procs = vec![xpc, p(41, 1, "/Applications/Safari.app/Contents/MacOS/Safari", 100)];
        let mut g = Grouper::with_lookup(FakeLookup, 501);
        let groups = g.group(&mut procs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].app.name, "Safari");
        assert_eq!(groups[0].mem_bytes, 300);
    }
}
