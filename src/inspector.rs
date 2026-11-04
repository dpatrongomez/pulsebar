//! M3 — Process Inspector: a detailed card for one process.

use serde::{Deserialize, Serialize};

use crate::i18n::I18n;
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

/// Plain-text card in English (`pulsebar process <pid>`).
pub fn to_text(d: &ProcessDetail, u: &Units, cores: usize) -> String {
    to_text_in(&I18n::english(), d, u, cores)
}

/// Plain-text card in `i18n`'s language (inspector panel and its Copy button). Labels are padded
/// to the longest label of that language, so the columns line up whatever the wording; with
/// English that is the same 14-column layout the CLI has always printed. Numbers go through
/// `u` (and `i18n.num`) so one card never mixes "3,07 GB" with "1.2 s".
pub fn to_text_in(i18n: &I18n, d: &ProcessDetail, u: &Units, cores: usize) -> String {
    let i = &d.identity;
    let x = &d.executable;
    let r = &d.resources;
    let s = &d.security;
    let dash = || "—".to_string();
    let yn = |b: bool| i18n.t(if b { "insp.yes" } else { "insp.no" });
    let list = |v: Vec<String>, sep: &str| if v.is_empty() { dash() } else { v.join(sep) };
    let opt = |o: &Option<String>| o.clone().unwrap_or_else(dash);
    let sections: Vec<(&str, Vec<(&str, String)>)> = vec![
        (
            "insp.identity",
            vec![
                ("insp.f.pid", i.pid.to_string()),
                ("insp.f.parent", format!("{} ({})", i.parent_name, i.ppid)),
                ("insp.f.parent_chain", list(i.parent_chain.clone(), " ← ")),
                ("insp.f.uptime", format_duration(i.uptime_s)),
                ("insp.f.user", i.user.clone()),
                ("insp.f.app", i.app.clone()),
                ("insp.f.bundle_id", opt(&i.bundle_id)),
                ("insp.f.version", opt(&i.version)),
            ],
        ),
        (
            "insp.security",
            vec![
                ("insp.f.signed", yn(s.signed)),
                ("insp.f.identifier", opt(&s.identifier)),
                ("insp.f.team_id", opt(&s.team_id)),
                ("insp.f.authority", opt(&s.authority)),
                ("insp.f.sandbox", yn(s.sandboxed)),
                ("insp.f.hardened", yn(s.hardened_runtime)),
                ("insp.f.entitlements", list(s.entitlements.clone(), ", ")),
            ],
        ),
        (
            "insp.executable",
            vec![
                ("insp.f.path", x.path.clone()),
                ("insp.f.arch", list(x.architectures.clone(), ", ")),
                ("insp.f.argv", x.argv.join(" ")),
                ("insp.f.cwd", if x.cwd.is_empty() { dash() } else { x.cwd.clone() }),
            ],
        ),
        (
            "insp.resources",
            vec![
                ("insp.f.cpu", u.cpu(r.cpu_pct, cores)),
                ("insp.f.memory", i18n.tf("insp.memory_value", &[("mem", &u.bytes(r.mem_bytes)), ("peak", &u.bytes(r.mem_peak))])),
                (
                    "insp.f.cpu_time",
                    i18n.tf(
                        "insp.cpu_time_value",
                        &[("user", &i18n.num(format!("{:.1}", r.cpu_time_user_s))), ("system", &i18n.num(format!("{:.1}", r.cpu_time_system_s)))],
                    ),
                ),
                ("insp.f.disk", i18n.tf("insp.disk_value", &[("r", &u.rate(r.disk_read_bps)), ("w", &u.rate(r.disk_write_bps))])),
                ("insp.f.energy", u.watts(r.energy_w, 2)),
                ("insp.f.open_files", r.open_files.map(|n| n.to_string()).unwrap_or_else(dash)),
                ("insp.f.listening", list(r.listening_ports.iter().map(u16::to_string).collect(), ", ")),
            ],
        ),
    ];
    let width = sections.iter().flat_map(|(_, rows)| rows.iter()).map(|(k, _)| i18n.t(k).chars().count()).max().unwrap_or(0) + 2;
    let mut out = i18n.tf("insp.heading", &[("name", &d.name), ("pid", &i.pid.to_string())]);
    out.push('\n');
    for (title, rows) in &sections {
        out.push('\n');
        out.push_str(&i18n.t(title));
        out.push('\n');
        for (k, v) in rows {
            let label = i18n.t(k);
            let pad = width - label.chars().count();
            out.push_str(&format!("  {label}{}{v}\n", " ".repeat(pad)));
        }
    }
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

        // The Spanish card is translated, lines up, and uses one number format throughout.
        let es = I18n::new(Some("es"));
        let mut d = d;
        d.resources.mem_bytes = 3_070_000_000;
        d.resources.mem_peak = 3_200_000_000;
        d.resources.cpu_time_user_s = 1.2;
        d.resources.energy_w = 1.234;
        let u = Units { decimal_comma: true, ..Default::default() };
        let t = to_text_in(&es, &d, &u, 8);
        assert!(t.contains("IDENTIDAD\n") && t.contains("RECURSOS\n"), "{t}");
        assert!(t.contains("3,07 GB (pico 3,20 GB)"), "{t}");
        assert!(t.contains("usuario 1,2 s"), "{t}");
        assert!(t.contains("1,23 W"), "{t}");
        assert!(!t.contains("Memory") && !t.contains("peak"), "{t}");
        let col = |label: &str, value: &str| t.lines().find(|l| l.starts_with(label)).and_then(|l| l.find(value));
        assert!(col("  PID", "300").is_some());
        assert_eq!(col("  PID", "300"), col("  Cadena de padres", "zsh"), "{t}");
        assert_eq!(col("  PID", "300"), col("  Escuchando", "3000"), "{t}");
    }
}
