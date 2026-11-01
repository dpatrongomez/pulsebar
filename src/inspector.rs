//! M3 — Process Inspector: a detailed card for one process.

use serde::{Deserialize, Serialize};

use crate::model::{Process, Snapshot};
use crate::sys::{self, CodeSignInfo};
use crate::units::{Units, format_duration};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub pid: i32,
    pub ppid: i32,
    pub parent_name: String,
    pub parent_chain: Vec<String>,
    pub uptime_s: i64,
    pub user: String,
    pub app: String,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Executable {
    pub path: String,
    pub architectures: Vec<String>,
    pub argv: Vec<String>,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resources {
    pub cpu_pct: f64,
    pub mem_bytes: u64,
    pub mem_peak: u64,
    pub cpu_time_user_s: f64,
    pub cpu_time_system_s: f64,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    pub energy_w: f64,
    pub open_files: Option<usize>,
    pub listening_ports: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessDetail {
    pub name: String,
    pub identity: Identity,
    pub security: CodeSignInfo,
    pub executable: Executable,
    pub resources: Resources,
}

fn parent_chain(s: &Snapshot, p: &Process) -> Vec<String> {
    let mut chain = Vec::new();
    let mut cur = p.ppid;
    for _ in 0..32 {
        let Some(pp) = s.process(cur) else { break };
        chain.push(format!("{} ({})", pp.name, pp.pid));
        if pp.ppid == 0 || pp.pid == 1 {
            break;
        }
        cur = pp.ppid;
    }
    chain
}

/// Build the card. `deep` runs codesign / lipo (slow-ish; do it off the UI thread).
pub fn inspect(s: &Snapshot, pid: i32, deep: bool) -> Option<ProcessDetail> {
    let p = s.process(pid)?;
    let group = s.app_for_pid(pid);
    let now = s.timestamp / 1000;
    let exe = p.executable_path.clone();
    let (security, archs) = if deep && !exe.is_empty() {
        let target = sys::outermost_bundle(&exe).unwrap_or_else(|| exe.clone());
        (sys::codesign_info(&target), sys::executable_archs(&exe))
    } else {
        (CodeSignInfo::default(), vec![])
    };
    Some(ProcessDetail {
        name: p.name.clone(),
        identity: Identity {
            pid,
            ppid: p.ppid,
            parent_name: s.process(p.ppid).map(|x| x.name.clone()).unwrap_or_default(),
            parent_chain: parent_chain(s, p),
            uptime_s: (now - p.start_time).max(0),
            user: sys::username(p.uid),
            app: group.map(|g| g.app.name.clone()).unwrap_or_else(|| p.app_name.clone()),
            bundle_id: group.and_then(|g| g.app.bundle_id.clone()),
            version: group.and_then(|g| g.app.version.clone()),
        },
        security,
        executable: Executable { path: exe, architectures: archs, argv: p.argv.clone(), cwd: p.cwd.clone() },
        resources: Resources {
            cpu_pct: p.cpu_pct,
            mem_bytes: p.mem_bytes,
            mem_peak: p.mem_peak,
            cpu_time_user_s: p.cpu_time_user_s,
            cpu_time_system_s: p.cpu_time_system_s,
            disk_read_bps: p.disk_read_bps,
            disk_write_bps: p.disk_write_bps,
            energy_w: p.energy_w,
            open_files: if deep { sys::open_fd_count(pid) } else { None },
            listening_ports: s.ports_of(pid),
        },
    })
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// Plain-text card (Copy button, `pulsebar process <pid>`).
pub fn to_text(d: &ProcessDetail, u: &Units, cores: usize) -> String {
    let i = &d.identity;
    let x = &d.executable;
    let r = &d.resources;
    let s = &d.security;
    let ports = if r.listening_ports.is_empty() {
        "—".into()
    } else {
        r.listening_ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
    };
    let mut out = String::new();
    out.push_str(&format!("{} — pid {}\n\n", d.name, i.pid));
    out.push_str("IDENTITY\n");
    out.push_str(&format!("  PID           {}\n  Parent        {} ({})\n", i.pid, i.parent_name, i.ppid));
    out.push_str(&format!("  Parent chain  {}\n", if i.parent_chain.is_empty() { "—".into() } else { i.parent_chain.join(" ← ") }));
    out.push_str(&format!("  Uptime        {}\n  User          {}\n  App           {}\n", format_duration(i.uptime_s), i.user, i.app));
    out.push_str(&format!("  Bundle ID     {}\n  Version       {}\n\n", i.bundle_id.as_deref().unwrap_or("—"), i.version.as_deref().unwrap_or("—")));
    out.push_str("SECURITY\n");
    out.push_str(&format!("  Signed        {}\n  Identifier    {}\n  Team ID       {}\n", yn(s.signed), s.identifier.as_deref().unwrap_or("—"), s.team_id.as_deref().unwrap_or("—")));
    out.push_str(&format!("  Authority     {}\n  Sandbox       {}\n  Hardened RT   {}\n", s.authority.as_deref().unwrap_or("—"), yn(s.sandboxed), yn(s.hardened_runtime)));
    out.push_str(&format!("  Entitlements  {}\n\n", if s.entitlements.is_empty() { "—".into() } else { s.entitlements.join(", ") }));
    out.push_str("EXECUTABLE\n");
    out.push_str(&format!("  Path          {}\n  Architecture  {}\n", x.path, if x.architectures.is_empty() { "—".into() } else { x.architectures.join(", ") }));
    out.push_str(&format!("  argv          {}\n  cwd           {}\n\n", x.argv.join(" "), if x.cwd.is_empty() { "—" } else { &x.cwd }));
    out.push_str("RESOURCES\n");
    out.push_str(&format!("  CPU           {}\n  Memory        {} (peak {})\n", u.cpu(r.cpu_pct, cores), u.bytes(r.mem_bytes), u.bytes(r.mem_peak)));
    out.push_str(&format!("  CPU time      user {:.1} s · system {:.1} s\n", r.cpu_time_user_s, r.cpu_time_system_s));
    out.push_str(&format!("  Disk          read {} · write {}\n  Energy        {:.2} W\n", u.rate(r.disk_read_bps), u.rate(r.disk_write_bps), r.energy_w));
    out.push_str(&format!("  Open files    {}\n  Listening     {}\n", r.open_files.map(|n| n.to_string()).unwrap_or("—".into()), ports));
    out
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

/// Two-column CSV (field,value) for "Export CSV".
pub fn to_csv(d: &ProcessDetail) -> String {
    let rows: Vec<(&str, String)> = vec![
        ("name", d.name.clone()),
        ("pid", d.identity.pid.to_string()),
        ("ppid", d.identity.ppid.to_string()),
        ("parent", d.identity.parent_name.clone()),
        ("uptime_s", d.identity.uptime_s.to_string()),
        ("user", d.identity.user.clone()),
        ("app", d.identity.app.clone()),
        ("bundle_id", d.identity.bundle_id.clone().unwrap_or_default()),
        ("version", d.identity.version.clone().unwrap_or_default()),
        ("signed", d.security.signed.to_string()),
        ("team_id", d.security.team_id.clone().unwrap_or_default()),
        ("sandboxed", d.security.sandboxed.to_string()),
        ("hardened_runtime", d.security.hardened_runtime.to_string()),
        ("entitlements", d.security.entitlements.join(" ")),
        ("path", d.executable.path.clone()),
        ("architectures", d.executable.architectures.join(" ")),
        ("argv", d.executable.argv.join(" ")),
        ("cwd", d.executable.cwd.clone()),
        ("cpu_pct", format!("{:.2}", d.resources.cpu_pct)),
        ("mem_bytes", d.resources.mem_bytes.to_string()),
        ("mem_peak", d.resources.mem_peak.to_string()),
        ("cpu_time_user_s", format!("{:.2}", d.resources.cpu_time_user_s)),
        ("cpu_time_system_s", format!("{:.2}", d.resources.cpu_time_system_s)),
        ("open_files", d.resources.open_files.map(|n| n.to_string()).unwrap_or_default()),
        ("listening_ports", d.resources.listening_ports.iter().map(u16::to_string).collect::<Vec<_>>().join(" ")),
    ];
    let mut s = String::from("field,value\n");
    for (k, v) in rows {
        s.push_str(&format!("{k},{}\n", csv_escape(&v)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ListeningPort;

    #[test]
    fn node_from_terminal() {
        let s = Snapshot {
            timestamp: 10_000_000,
            processes: vec![
                Process { pid: 100, ppid: 1, name: "Terminal".into(), ..Default::default() },
                Process { pid: 200, ppid: 100, name: "zsh".into(), ..Default::default() },
                Process {
                    pid: 300,
                    ppid: 200,
                    name: "node".into(),
                    argv: vec!["node".into(), "next".into(), "dev".into(), "--port".into(), "3000".into()],
                    cwd: "/Users/me/code/shop".into(),
                    start_time: 9_000,
                    ..Default::default()
                },
            ],
            ports: vec![ListeningPort { pid: 300, port: 3000, address: "*".into(), protocol: "TCP".into(), command: "node".into() }],
            ..Default::default()
        };
        let d = inspect(&s, 300, false).unwrap();
        assert_eq!(d.identity.parent_name, "zsh");
        assert_eq!(d.executable.cwd, "/Users/me/code/shop");
        assert_eq!(d.executable.argv.join(" "), "node next dev --port 3000");
        assert_eq!(d.resources.listening_ports, vec![3000]);
        assert_eq!(d.identity.parent_chain, vec!["zsh (200)", "Terminal (100)"]);
        assert_eq!(d.identity.uptime_s, 1000);
        let csv = to_csv(&d);
        assert!(csv.contains("argv,node next dev --port 3000\n"));
        let txt = to_text(&d, &Units::default(), 8);
        assert!(txt.contains("Listening     3000"));
    }
}
