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
//!
//! On Linux a desktop entry takes the place of a bundle: a process belongs to the entry named by
//! its systemd unit (where the desktop started the app) or matching its executable. There is no
//! responsible process, and the walk up the parent chain stops at session processes
//! (`systemd --user`, `gnome-shell`, `sshd`…) because they started everything below them.

use std::collections::HashMap;

use crate::model::{App, AppGroup, Process};
use crate::sys::{self, BundleCache, BundleInfo};

#[cfg(not(target_os = "linux"))]
pub const SYSTEM_KEY: &str = "system:macos";
#[cfg(not(target_os = "linux"))]
pub const SYSTEM_NAME: &str = "macOS";
#[cfg(target_os = "linux")]
pub const SYSTEM_KEY: &str = "system:linux";
#[cfg(target_os = "linux")]
pub const SYSTEM_NAME: &str = "Linux";

pub trait BundleLookup {
    fn lookup(&mut self, bundle_path: &str) -> BundleInfo;
    /// The bundle (or desktop entry) a process belongs to by itself.
    fn bundle_for(&self, p: &Process) -> Option<String> {
        sys::outermost_bundle(&p.executable_path)
    }
    /// Called once before each grouping pass.
    fn prepare(&mut self) {}
}

impl BundleLookup for BundleCache {
    fn lookup(&mut self, bundle_path: &str) -> BundleInfo {
        self.get(bundle_path).clone()
    }

    fn bundle_for(&self, p: &Process) -> Option<String> {
        let Some((_, ix)) = &self.desktop else { return sys::outermost_bundle(&p.executable_path) };
        p.app_hint.as_deref().and_then(|u| ix.match_unit(u)).or_else(|| ix.match_exe(&p.executable_path)).map(str::to_string)
    }

    /// Scans for newly installed apps every so often (a scan reads a few hundred small files).
    fn prepare(&mut self) {
        const RESCAN: std::time::Duration = std::time::Duration::from_secs(600);
        if cfg!(target_os = "linux") && self.desktop.as_ref().is_none_or(|(t, _)| t.elapsed() > RESCAN) {
            self.desktop = Some((std::time::Instant::now(), crate::linux::DesktopIndex::scan()));
        }
    }
}

/// Processes that start everything else in a Linux session, so being their child says nothing
/// about which app a process belongs to.
fn is_session_process(name: &str) -> bool {
    const NAMES: [&str; 22] = [
        "systemd", "init", "kthreadd", "sshd", "sshd-session", "login", "gnome-shell", "gnome-session-binary", "gnome-session-b", "plasmashell", "kwin_wayland",
        "kwin_x11", "ksmserver", "krunner", "cinnamon", "xfce4-session", "xfwm4", "sway", "Hyprland", "gdm-session-worker", "sddm-helper", "lightdm",
    ];
    NAMES.contains(&name)
}

/// Shells and multiplexers mean a person started what runs below them.
fn is_interactive_process(name: &str) -> bool {
    matches!(name, "bash" | "zsh" | "fish" | "sh" | "dash" | "tmux" | "tmux: server" | "screen" | "SCREEN")
}

fn is_linux_system_executable(p: &Process, my_uid: u32, parent_is_session: bool) -> bool {
    let e = &p.executable_path;
    p.uid != my_uid
        || e.is_empty()
        || ["/usr/lib/", "/usr/lib64/", "/usr/libexec/", "/usr/sbin/", "/sbin/", "/lib/", "/lib64/"].iter().any(|d| e.starts_with(d))
        || (e.starts_with("/usr/bin/") && (p.ppid <= 1 || parent_is_session) && !works_in_a_project(p))
}

/// Daemons run from `/` or the home directory. A runtime from `/usr/bin` working anywhere else is
/// someone's background server (`nohup python3 -m http.server &`), not part of the system.
fn works_in_a_project(p: &Process) -> bool {
    !p.cwd.is_empty() && p.cwd != "/" && std::path::Path::new(&p.cwd) != crate::paths::home()
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
    /// Apply the Linux rules (see the module docs). A field, so tests cover them on any platform.
    pub linux: bool,
}

impl Grouper<BundleCache> {
    pub fn new() -> Self {
        Self { lookup: BundleCache::default(), my_uid: sys::current_uid(), linux: cfg!(target_os = "linux") }
    }
}

impl Default for Grouper<BundleCache> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: BundleLookup> Grouper<L> {
    pub fn with_lookup(lookup: L, my_uid: u32) -> Self {
        Self { lookup, my_uid, linux: false }
    }

    /// Desktop entries live under /usr/share whatever they describe: only macOS paths tell.
    fn is_system(&self, bundle: &str) -> bool {
        !self.linux && is_system_bundle(bundle)
    }

    fn owner(&self, p: &Process, by_pid: &HashMap<i32, &Process>) -> Owner {
        if let Some(b) = self.lookup.bundle_for(p) {
            return if self.is_system(&b) { Owner::System } else { Owner::Bundle(b) };
        }
        if p.responsible_pid > 1 && p.responsible_pid != p.pid
            && let Some(b) = by_pid.get(&p.responsible_pid).and_then(|r| self.lookup.bundle_for(r))
                && !self.is_system(&b) {
                    return Owner::Bundle(b);
                }
        // Walk the parent chain (bounded, cycle-safe).
        let mut cur = p.ppid;
        let mut root = p;
        let mut under_session = false;
        let mut interactive = is_interactive_process(&p.name);
        for _ in 0..64 {
            if cur <= 1 {
                break;
            }
            let Some(parent) = by_pid.get(&cur) else { break };
            if self.linux && is_session_process(&parent.name) {
                under_session = true;
                break;
            }
            if let Some(b) = self.lookup.bundle_for(parent) {
                return if self.is_system(&b) { Owner::System } else { Owner::Bundle(b) };
            }
            interactive |= is_interactive_process(&parent.name);
            root = parent;
            cur = parent.ppid;
        }
        if self.linux {
            // `/usr/bin/node` in a terminal is the user's, however system-like its path.
            let system = !interactive
                && is_linux_system_executable(root, self.my_uid, under_session)
                && is_linux_system_executable(p, self.my_uid, under_session || p.pid == root.pid);
            return if system || p.uid != self.my_uid { Owner::System } else { Owner::Named(root.name.clone()) };
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
        self.lookup.prepare();
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
                g.root_is_main = true;
            }
        }
        let mut v: Vec<AppGroup> = groups.into_values().collect();
        for g in &mut v {
            g.pids.sort_unstable();
            if self.linux && g.key != SYSTEM_KEY {
                Self::set_linux_root(g, procs);
            }
        }
        v.sort_by(|a, b| b.mem_bytes.cmp(&a.mem_bytes).then(a.app.name.cmp(&b.app.name)));
        v
    }

    /// Linux apps rarely have init as their parent (the session manager starts them), and there is
    /// no `Contents/MacOS` to look for: the main process is the oldest one whose parent is outside
    /// the group.
    fn set_linux_root(g: &mut AppGroup, procs: &[Process]) {
        let roots = procs.iter().filter(|p| g.pids.binary_search(&p.pid).is_ok() && g.pids.binary_search(&p.ppid).is_err());
        if let Some(root) = roots.min_by_key(|p| (p.start_time, p.pid)) {
            g.root_pid = root.pid;
            g.root_is_main = true;
        }
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

    /// Desktop entries by executable name, as `DesktopIndex` would match them.
    struct FakeDesktop(&'static [(&'static str, &'static str)]);
    impl BundleLookup for FakeDesktop {
        fn lookup(&mut self, path: &str) -> BundleInfo {
            let id = path.rsplit('/').next().unwrap().trim_end_matches(".desktop");
            BundleInfo { path: path.into(), bundle_id: Some(id.into()), name: id.into(), version: None }
        }
        fn bundle_for(&self, p: &Process) -> Option<String> {
            let exe = p.executable_path.rsplit('/').next()?;
            self.0.iter().find(|(e, _)| *e == exe).map(|(_, d)| d.to_string())
        }
    }

    fn linux(apps: &'static [(&'static str, &'static str)]) -> Grouper<FakeDesktop> {
        Grouper { lookup: FakeDesktop(apps), my_uid: 501, linux: true }
    }

    fn group_of<'a>(groups: &'a [AppGroup], name: &str) -> &'a AppGroup {
        groups.iter().find(|g| g.app.name == name).unwrap_or_else(|| panic!("no {name} in {:?}", groups.iter().map(|g| &g.app.name).collect::<Vec<_>>()))
    }

    #[test]
    fn linux_desktop_app_and_terminal_children() {
        let mut procs = vec![
            p(1000, 1, "/usr/lib/systemd/systemd", 1),
            p(1100, 1000, "/usr/bin/gnome-shell", 1),
            p(1200, 1100, "/usr/lib/firefox/firefox", 100),
            p(1201, 1200, "/usr/lib/firefox/firefox", 50),
            p(1300, 1000, "/usr/libexec/gnome-terminal-server", 10),
            p(1301, 1300, "/usr/bin/bash", 1),
            p(1302, 1301, "/home/me/.nvm/versions/node/bin/node", 200),
        ];
        procs[0].name = "systemd".into();
        procs[1].name = "gnome-shell".into();
        let mut g = linux(&[("firefox", "/usr/share/applications/firefox.desktop"), ("gnome-terminal-server", "/usr/share/applications/org.gnome.Terminal.desktop")]);
        let groups = g.group(&mut procs);
        assert_eq!(group_of(&groups, "firefox").process_count, 2);
        assert_eq!(group_of(&groups, "firefox").root_pid, 1200);
        assert!(group_of(&groups, "firefox").root_is_main, "its parent is the shell, not init");
        assert_eq!(group_of(&groups, "org.gnome.Terminal").process_count, 3, "the terminal owns what runs in it");
        assert_eq!(group_of(&groups, SYSTEM_NAME).process_count, 2, "the session itself is the system");
    }

    #[test]
    fn linux_session_does_not_swallow_its_children() {
        let mut procs = vec![
            p(1000, 1, "/usr/lib/systemd/systemd", 1),
            p(1100, 1000, "/usr/bin/pipewire", 5),
            p(1200, 1000, "/home/me/bin/syncd", 30),
            p(1201, 1200, "/home/me/bin/syncd", 20),
        ];
        procs[0].name = "systemd".into();
        let mut g = linux(&[]);
        let groups = g.group(&mut procs);
        assert_eq!(group_of(&groups, SYSTEM_NAME).process_count, 2, "distro service under systemd --user");
        assert_eq!(group_of(&groups, "syncd").process_count, 2);
        assert!(group_of(&groups, "syncd").root_is_main);
    }

    #[test]
    fn linux_ssh_and_tmux_sessions_are_the_users() {
        let mut procs = vec![
            p(900, 1, "/usr/sbin/sshd", 1),
            p(901, 900, "/usr/bin/bash", 1),
            p(902, 901, "/usr/bin/node", 300),
            p(950, 1, "/usr/bin/tmux", 1),
            p(951, 950, "/usr/bin/bash", 1),
            p(952, 951, "/usr/bin/python3", 40),
        ];
        procs[0].name = "sshd".into();
        procs[0].uid = 0;
        procs[3].name = "tmux: server".into();
        let mut g = linux(&[]);
        let groups = g.group(&mut procs);
        assert_eq!(group_of(&groups, "bash").process_count, 2, "login shell below sshd");
        assert_eq!(group_of(&groups, "tmux: server").process_count, 3);
        assert_eq!(group_of(&groups, SYSTEM_NAME).process_count, 1, "sshd itself: {groups:?}");
    }

    #[test]
    fn linux_background_server_in_a_project_is_the_users() {
        let mut server = p(40, 1, "/usr/bin/python3", 30);
        server.cwd = "/work/shop".into();
        let mut service = p(41, 1, "/usr/bin/python3", 5);
        service.cwd = "/".into();
        let mut procs = vec![server, service];
        let groups = linux(&[]).group(&mut procs);
        assert_eq!(group_of(&groups, "python3").pids, vec![40]);
        assert_eq!(group_of(&groups, SYSTEM_NAME).pids, vec![41]);
    }

    #[test]
    fn linux_other_users_and_kernel_threads_are_system() {
        let mut procs = vec![p(2, 0, "", 0), p(20, 2, "", 0), p(30, 1, "/opt/postgres/bin/postgres", 50), p(31, 1, "/usr/sbin/cron", 1)];
        procs[2].uid = 999;
        procs[3].uid = 0;
        let mut g = linux(&[]);
        let groups = g.group(&mut procs);
        assert_eq!(groups.len(), 1, "{groups:?}");
        assert_eq!(groups[0].process_count, 4);
    }

    #[test]
    fn root_is_main_only_with_a_real_main_process() {
        let main = "/Applications/Foo.app/Contents/MacOS/Foo";
        let helper = "/Applications/Foo.app/Contents/Library/LoginItems/FooHelper.app/Contents/MacOS/FooHelper";
        let mut g = Grouper::with_lookup(FakeLookup, 501);
        let with_main = g.group(&mut [p(10, 1, main, 1), p(11, 10, helper, 1)]);
        assert!(with_main[0].root_is_main && with_main[0].root_pid == 10);
        let only_helper = g.group(&mut [p(11, 1, helper, 1)]);
        assert!(!only_helper[0].root_is_main);
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
        assert_eq!(by[SYSTEM_NAME].process_count, 2);
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
