//! Projects tab: group the current user's processes by project folder (resolved from `cwd`),
//! detect dev servers (processes listening on a port) and highlight idle ones.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::model::{DevServer, ListeningPort, Process, Project, ProjectSummary};
use crate::units::format_bytes;

/// Files/dirs that mark the root of a project.
pub const MARKERS: [&str; 14] = [
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "Gemfile",
    "composer.json",
    "pom.xml",
    "build.gradle",
    "deno.json",
    "mix.exs",
    "Package.swift",
    "requirements.txt",
    "docker-compose.yml",
];

/// Resolve the project root for `cwd`: the *outermost* directory below `$HOME` containing a
/// marker, preferring a `.git` root (so `apps/web` in a monorepo resolves to the monorepo when
/// it is one repository). `has_marker(dir, marker)` is injectable for tests.
pub fn project_root(cwd: &Path, home: &Path, has_marker: &dyn Fn(&Path, &str) -> bool) -> Option<PathBuf> {
    if !cwd.starts_with(home) || cwd == home {
        return None;
    }
    let mut nearest: Option<PathBuf> = None;
    let mut git: Option<PathBuf> = None;
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if d == home {
            break;
        }
        if has_marker(d, ".git") {
            git = Some(d.to_path_buf());
        }
        if nearest.is_none() && MARKERS.iter().any(|m| has_marker(d, m)) {
            nearest = Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    // Ignore dot-dirs in $HOME (~/.cache, ~/.vscode/extensions…).
    let r = git.or(nearest)?;
    let rel = r.strip_prefix(home).ok()?;
    if rel.components().next().map(|c| c.as_os_str().to_string_lossy().starts_with('.')).unwrap_or(true)
        || rel.starts_with("Library")
    {
        return None;
    }
    Some(r)
}

pub fn fs_has_marker(dir: &Path, marker: &str) -> bool {
    dir.join(marker).exists()
}

/// Remembers when each process was last active (CPU above a floor). Lives in the long-running
/// sampler, persisted into `live.json` through the computed `DevServer::idle_secs`.
#[derive(Debug, Default, Clone)]
pub struct ActivityTracker {
    /// pid → (start_time, last_active unix s)
    last_active: HashMap<i32, (i64, i64)>,
    pub cpu_floor_pct: f64,
}

impl ActivityTracker {
    pub fn new() -> Self {
        Self { last_active: HashMap::new(), cpu_floor_pct: 0.15 }
    }

    pub fn update(&mut self, procs: &[Process], now: i64) {
        let mut next = HashMap::with_capacity(procs.len());
        for p in procs {
            let active = p.cpu_pct > self.cpu_floor_pct || p.net_rx_bps + p.net_tx_bps > 2048.0;
            let prev = self.last_active.get(&p.pid).filter(|(st, _)| *st == p.start_time).map(|(_, t)| *t);
            let last = if active { now } else { prev.unwrap_or(now) };
            next.insert(p.pid, (p.start_time, last));
        }
        self.last_active = next;
    }

    pub fn last_active(&self, pid: i32) -> Option<i64> {
        self.last_active.get(&pid).map(|(_, t)| *t)
    }

    /// Seed from a previous snapshot so idle times survive restarts of the sampler.
    pub fn seed(&mut self, servers: &[DevServer]) {
        for s in servers {
            self.last_active.entry(s.process_pid).or_insert((s.started_at, s.last_active_at));
        }
    }
}

pub struct ProjectResolver {
    home: PathBuf,
    cache: HashMap<String, Option<PathBuf>>,
    has_marker: Box<dyn Fn(&Path, &str) -> bool + Send>,
}

impl ProjectResolver {
    pub fn new() -> Self {
        Self::with(crate::paths::home(), Box::new(fs_has_marker))
    }

    pub fn with(home: PathBuf, has_marker: Box<dyn Fn(&Path, &str) -> bool + Send>) -> Self {
        Self { home, cache: HashMap::new(), has_marker }
    }

    pub fn resolve(&mut self, cwd: &str) -> Option<PathBuf> {
        if cwd.is_empty() {
            return None;
        }
        if let Some(r) = self.cache.get(cwd) {
            return r.clone();
        }
        let r = project_root(Path::new(cwd), &self.home, &*self.has_marker);
        if self.cache.len() > 4096 {
            self.cache.clear();
        }
        self.cache.insert(cwd.to_string(), r.clone());
        r
    }
}

impl Default for ProjectResolver {
    fn default() -> Self {
        Self::new()
    }
}

fn short_command(p: &Process) -> String {
    let args: Vec<&str> = p.argv.iter().skip(1).map(String::as_str).take(4).collect();
    let bin = p.argv.first().map(|a| a.rsplit('/').next().unwrap_or(a)).unwrap_or(&p.name);
    if args.is_empty() { bin.to_string() } else { format!("{bin} {}", args.join(" ")) }
}

pub fn summarize(
    procs: &[Process],
    ports: &[ListeningPort],
    resolver: &mut ProjectResolver,
    tracker: &ActivityTracker,
    my_uid: u32,
    now: i64,
    idle_threshold_s: i64,
) -> Vec<ProjectSummary> {
    let mut by_root: BTreeMap<PathBuf, ProjectSummary> = BTreeMap::new();
    for p in procs.iter().filter(|p| p.uid == my_uid) {
        let Some(root) = resolver.resolve(&p.cwd) else { continue };
        let s = by_root.entry(root.clone()).or_insert_with(|| ProjectSummary {
            project: Project {
                name: root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                folder_path: root.to_string_lossy().into_owned(),
            },
            ..Default::default()
        });
        s.process_count += 1;
        s.mem_bytes += p.mem_bytes;
        s.cpu_pct += p.cpu_pct;
        s.pids.push(p.pid);
        let mut my_ports: Vec<u16> = ports.iter().filter(|l| l.pid == p.pid).map(|l| l.port).collect();
        my_ports.sort_unstable();
        my_ports.dedup();
        for port in my_ports {
            let last = tracker.last_active(p.pid).unwrap_or(now);
            let idle_secs = (now - last).max(0);
            s.ports.push(port);
            s.servers.push(DevServer {
                project: s.project.name.clone(),
                process_pid: p.pid,
                command: short_command(p),
                port,
                started_at: p.start_time,
                last_active_at: last,
                mem_bytes: p.mem_bytes,
                idle_secs,
                idle: idle_secs >= idle_threshold_s,
            });
        }
    }
    let mut v: Vec<ProjectSummary> = by_root
        .into_values()
        .map(|mut s| {
            s.ports.sort_unstable();
            s.ports.dedup();
            s.state = if s.servers.is_empty() {
                "running".into()
            } else if s.servers.iter().all(|x| x.idle) {
                "idle".into()
            } else {
                "serving".into()
            };
            s
        })
        .collect();
    v.sort_by(|a, b| b.mem_bytes.cmp(&a.mem_bytes).then(a.project.name.cmp(&b.project.name)));
    v
}

/// Idle dev servers across projects, one entry per process (a process may listen on many ports).
pub fn idle_servers(projects: &[ProjectSummary]) -> Vec<&DevServer> {
    let mut seen = std::collections::HashSet::new();
    projects.iter().flat_map(|p| p.servers.iter()).filter(|s| s.idle && seen.insert(s.process_pid)).collect()
}

/// "4 dev servers are running but idle. Stopping them frees 1.37 GB and ports 3000, 4321, 4322, 8000."
pub fn idle_banner(projects: &[ProjectSummary]) -> Option<String> {
    let idle = idle_servers(projects);
    if idle.is_empty() {
        return None;
    }
    let mem: u64 = idle.iter().map(|s| s.mem_bytes).sum();
    let idle_pids: std::collections::HashSet<i32> = idle.iter().map(|s| s.process_pid).collect();
    let mut ports: Vec<u16> =
        projects.iter().flat_map(|p| p.servers.iter()).filter(|s| idle_pids.contains(&s.process_pid)).map(|s| s.port).collect();
    ports.sort_unstable();
    ports.dedup();
    let n = idle.len();
    let ports_s: Vec<String> = ports.iter().map(u16::to_string).collect();
    Some(format!(
        "{n} dev server{} {} running but idle. Stopping {} frees {} and port{} {}.",
        if n == 1 { "" } else { "s" },
        if n == 1 { "is" } else { "are" },
        if n == 1 { "it" } else { "them" },
        format_bytes(mem as f64),
        if ports.len() == 1 { "" } else { "s" },
        ports_s.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markers(dir: &Path, m: &str) -> bool {
        let d = dir.to_string_lossy();
        (d == "/Users/me/code/shop" && m == ".git") || (d == "/Users/me/code/shop/apps/web" && m == "package.json")
            || (d == "/Users/me/code/api" && m == "Cargo.toml")
    }

    #[test]
    fn roots() {
        let home = Path::new("/Users/me");
        assert_eq!(
            project_root(Path::new("/Users/me/code/shop/apps/web/src"), home, &markers),
            Some(PathBuf::from("/Users/me/code/shop"))
        );
        assert_eq!(project_root(Path::new("/Users/me/code/api"), home, &markers), Some(PathBuf::from("/Users/me/code/api")));
        assert_eq!(project_root(Path::new("/Users/me"), home, &markers), None);
        assert_eq!(project_root(Path::new("/tmp/x"), home, &markers), None);
    }

    #[test]
    fn idle_detection_and_banner() {
        let mk = |pid, cwd: &str, mem, cpu| Process {
            pid,
            uid: 501,
            name: "node".into(),
            argv: vec!["node".into(), "next".into(), "dev".into()],
            cwd: cwd.into(),
            mem_bytes: mem,
            cpu_pct: cpu,
            start_time: 1000,
            ..Default::default()
        };
        let procs = vec![mk(1, "/Users/me/code/shop", 458_000_000, 0.0), mk(2, "/Users/me/code/api", 100_000_000, 0.0)];
        let ports = vec![
            ListeningPort { pid: 1, port: 4322, address: "*".into(), protocol: "TCP".into(), command: "node".into() },
            ListeningPort { pid: 2, port: 8000, address: "*".into(), protocol: "TCP".into(), command: "node".into() },
        ];
        let mut tracker = ActivityTracker::new();
        tracker.update(&procs, 10_000);
        let mut busy = procs.clone();
        busy[1].cpu_pct = 5.0;
        tracker.update(&busy, 14_000);
        let mut r = ProjectResolver::with(PathBuf::from("/Users/me"), Box::new(markers));
        let s = summarize(&procs, &ports, &mut r, &tracker, 501, 14_000, 3600);
        assert_eq!(s.len(), 2);
        let shop = s.iter().find(|p| p.project.name == "shop").unwrap();
        assert!(shop.servers[0].idle, "idle for 4000 s > 3600 s");
        assert_eq!(shop.state, "idle");
        let api = s.iter().find(|p| p.project.name == "api").unwrap();
        assert!(!api.servers[0].idle);
        assert_eq!(
            idle_banner(&s).unwrap(),
            "1 dev server is running but idle. Stopping it frees 458 MB and port 4322."
        );
    }
}
