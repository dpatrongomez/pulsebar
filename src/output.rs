//! Human-readable tables for the CLI (golden-tested in `tests/cli_golden.rs`).

use crate::inspector;
use crate::limits;
use crate::model::*;
use crate::projects;
use crate::units::{Units, format_duration};

/// Simple column table; numeric-looking columns are right-aligned.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    right: Vec<bool>,
}

impl Table {
    pub fn new(headers: &[&str], right: &[bool]) -> Self {
        Self { headers: headers.iter().map(|s| s.to_string()).collect(), rows: vec![], right: right.to_vec() }
    }
    pub fn row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }
    pub fn render(&self) -> String {
        let n = self.headers.len();
        let mut w: Vec<usize> = self.headers.iter().map(|h| h.chars().count()).collect();
        for r in &self.rows {
            for (i, c) in r.iter().enumerate().take(n) {
                w[i] = w[i].max(c.chars().count());
            }
        }
        let fmt_row = |cells: &[String]| {
            let mut line = String::new();
            for i in 0..n {
                let c = cells.get(i).map(String::as_str).unwrap_or("");
                let pad = w[i].saturating_sub(c.chars().count());
                let last = i + 1 == n;
                if self.right.get(i).copied().unwrap_or(false) {
                    line.push_str(&" ".repeat(pad));
                    line.push_str(c);
                } else {
                    line.push_str(c);
                    if !last {
                        line.push_str(&" ".repeat(pad));
                    }
                }
                if !last {
                    line.push_str("  ");
                }
            }
            line.trim_end().to_string()
        };
        let mut out = fmt_row(&self.headers);
        out.push('\n');
        for r in &self.rows {
            out.push_str(&fmt_row(r));
            out.push('\n');
        }
        out
    }
}

pub fn status(s: &Snapshot, u: &Units) -> String {
    let m = &s.machine;
    let cores = m.cpu.core_count;
    let mut o = format!("PulseBar · {} · {} processes → {} apps\n\n", s.hostname, s.processes.len(), s.apps.len());
    o.push_str(&format!(
        "CPU       {}   user {:.0} % · system {:.0} % · load {:.2} {:.2} {:.2}\n",
        u.cpu(m.cpu.total_pct, cores),
        m.cpu.user_pct,
        m.cpu.system_pct,
        m.cpu.load_avg[0],
        m.cpu.load_avg[1],
        m.cpu.load_avg[2]
    ));
    o.push_str(&format!(
        "Memory    {} of {}   app {} · wired {} · compressed {} · swap {}\n",
        u.bytes(m.memory.used),
        u.bytes(m.memory.total),
        u.bytes(m.memory.app),
        u.bytes(m.memory.wired),
        u.bytes(m.memory.compressed),
        u.bytes(m.memory.swap_used)
    ));
    if let Some(v) = m.disk.boot_free() {
        o.push_str(&format!(
            "Disk      {} free of {}   read {} · write {}\n",
            u.bytes(v.free),
            u.bytes(v.total),
            u.rate(m.disk.read_bps),
            u.rate(m.disk.write_bps)
        ));
    }
    o.push_str(&format!("Network   ↓ {}   ↑ {}\n", u.rate(m.network.rx_bps), u.rate(m.network.tx_bps)));
    if m.gpu.available {
        o.push_str(&format!("GPU       {:.0} %   {}\n", m.gpu.utilization_pct, m.gpu.name));
    }
    if m.neural.available {
        o.push_str(&format!("Neural    {:.2} W\n", m.neural.power_w));
    }
    if m.battery.present {
        let state = if m.battery.charging {
            "charging"
        } else if m.battery.on_ac {
            "on AC"
        } else {
            "on battery"
        };
        let left = m.battery.time_remaining_min.map(|t| format!(" · {} left", format_duration(t * 60))).unwrap_or_default();
        o.push_str(&format!("Battery   {:.0} %   {state}{left}\n", m.battery.percent));
    }
    if let Some(t) = m.sensors.cpu_temp_c {
        o.push_str(&format!("Temp      CPU {}", u.temp(t)));
        if let Some(g) = m.sensors.gpu_temp_c {
            o.push_str(&format!(" · GPU {}", u.temp(g)));
        }
        o.push('\n');
    }
    if let Some(w) = m.power.system_w {
        o.push_str(&format!("Power     {w:.1} W\n"));
    }
    let busiest: Vec<String> =
        s.top_apps(RankMetric::Cpu, 3).iter().map(|a| format!("{} {}", a.app.name, u.cpu(a.cpu_pct, cores))).collect();
    o.push_str(&format!("\nBusiest right now: {}\n", busiest.join(" · ")));
    o
}

pub fn metric_value(a: &AppGroup, m: RankMetric, u: &Units, cores: usize) -> String {
    match m {
        RankMetric::Cpu => u.cpu(a.cpu_pct, cores),
        RankMetric::Memory => u.bytes(a.mem_bytes),
        RankMetric::Energy => format!("{:.2} W", a.energy_w),
        RankMetric::Disk => u.rate(a.disk_read_bps + a.disk_write_bps),
        RankMetric::Network => u.rate(a.net_rx_bps + a.net_tx_bps),
    }
}

pub fn top(s: &Snapshot, m: RankMetric, n: usize, u: &Units) -> String {
    let mut t = Table::new(&["#", "APP", m.as_str().to_uppercase().as_str(), "PROCS"], &[true, false, true, true]);
    for (i, a) in s.top_apps(m, n).iter().enumerate() {
        t.row(vec![(i + 1).to_string(), a.app.name.clone(), metric_value(a, m, u, s.machine.cpu.core_count), a.process_count.to_string()]);
    }
    t.render()
}

pub fn ports(s: &Snapshot, u: &Units) -> String {
    let mut t = Table::new(&["PORT", "PID", "PROCESS", "APP", "PROJECT", "MEMORY", "IDLE"], &[true, true, false, false, false, true, true]);
    for l in &s.ports {
        let p = s.process(l.pid);
        let project = s.projects.iter().find(|x| x.pids.contains(&l.pid)).map(|x| x.project.name.clone()).unwrap_or("—".into());
        let idle = s
            .projects
            .iter()
            .flat_map(|x| x.servers.iter())
            .find(|x| x.process_pid == l.pid && x.port == l.port)
            .map(|x| format_duration(x.idle_secs))
            .unwrap_or("—".into());
        t.row(vec![
            l.port.to_string(),
            l.pid.to_string(),
            p.map(|p| p.name.clone()).unwrap_or_else(|| l.command.clone()),
            p.map(|p| p.app_name.clone()).unwrap_or_default(),
            project,
            p.map(|p| u.bytes(p.mem_bytes)).unwrap_or_default(),
            idle,
        ]);
    }
    t.render()
}

pub fn projects(s: &Snapshot, u: &Units) -> String {
    let mut t = Table::new(&["PROJECT", "PROCS", "MEMORY", "PORTS", "STATE"], &[false, true, true, false, false]);
    for p in &s.projects {
        let ports = if p.ports.is_empty() { "—".into() } else { p.ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ") };
        t.row(vec![p.project.name.clone(), p.process_count.to_string(), u.bytes(p.mem_bytes), ports, p.state.clone()]);
    }
    let mut o = t.render();
    if let Some(b) = projects::idle_banner(&s.projects) {
        o.push('\n');
        o.push_str(&b);
        o.push('\n');
    }
    o
}

pub fn agents(s: &Snapshot, u: &Units) -> String {
    let now = s.timestamp / 1000;
    let mut t = Table::new(&["AGENT", "PID", "PROJECT", "STATE", "MEMORY", "CPU", "UPTIME", "STARTED"], &[false, true, false, false, true, true, true, false]);
    for a in &s.agents {
        let state = match a.state {
            AgentState::Working => "working",
            AgentState::Waiting => "waiting",
            AgentState::Idle => "idle",
        };
        let started: Vec<String> = a.started.iter().map(|x| x.role.clone()).collect();
        t.row(vec![
            a.agent_kind.label().into(),
            a.pid.to_string(),
            a.project.clone(),
            state.into(),
            u.bytes(a.mem_bytes),
            u.cpu(a.cpu_percent, s.machine.cpu.core_count),
            format_duration(now - a.started_at),
            if started.is_empty() { "—".into() } else { started.join(", ") },
        ]);
    }
    t.render()
}

pub fn limits(s: &Snapshot) -> String {
    let now = s.timestamp / 1000;
    let mut t = Table::new(&["PROVIDER", "PLAN", "WINDOW", "USED", "RESETS IN", "PACE"], &[false, false, false, true, true, false]);
    for p in &s.plans {
        t.row(vec![
            p.provider.clone(),
            p.plan.clone(),
            p.window.clone(),
            format!("{:.0} %", p.used_pct),
            p.resets_at.filter(|r| *r > now).map(|r| format_duration(r - now)).unwrap_or("—".into()),
            p.pace_text.clone(),
        ]);
    }
    let mut o = t.render();
    for p in &s.plans {
        o.push_str(&format!("{}\n", limits::headline(p, now)));
    }
    o
}

pub fn process(s: &Snapshot, pid: i32, u: &Units, deep: bool) -> Option<String> {
    inspector::inspect(s, pid, deep).map(|d| inspector::to_text(&d, u, s.machine.cpu.core_count))
}
