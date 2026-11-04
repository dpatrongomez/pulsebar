//! JSON views shared by the CLI (`--json`) and the MCP tools, so both expose the same schema
//! (see `schemas/`). Only the current user's processes are included.

use serde_json::{Value, json};

use crate::model::*;
use crate::login_item::LoginStatus;
use crate::{docker, limits, projects};

fn mine(s: &Snapshot, uid: u32) -> impl Iterator<Item = &Process> {
    s.processes.iter().filter(move |p| p.uid == uid)
}

pub fn status(s: &Snapshot) -> Value {
    let m = &s.machine;
    let boot = m.disk.boot_free();
    json!({
        "timestamp": s.timestamp,
        "hostname": s.hostname,
        "process_count": s.processes.len(),
        "app_count": s.apps.len(),
        "cpu": {"total_pct": r1(m.cpu.total_pct), "user_pct": r1(m.cpu.user_pct), "system_pct": r1(m.cpu.system_pct),
                 "load_avg": m.cpu.load_avg.iter().map(|x| r2(*x)).collect::<Vec<_>>(), "cores": m.cpu.core_count},
        "memory": {"total_bytes": m.memory.total, "used_bytes": m.memory.used, "app_bytes": m.memory.app, "wired_bytes": m.memory.wired,
                   "compressed_bytes": m.memory.compressed, "cached_bytes": m.memory.cached, "swap_used_bytes": m.memory.swap_used},
        "disk": {"read_bps": r0(m.disk.read_bps), "write_bps": r0(m.disk.write_bps),
                 "boot_free_bytes": boot.map(|v| v.free), "boot_total_bytes": boot.map(|v| v.total)},
        "network": {"rx_bps": r0(m.network.rx_bps), "tx_bps": r0(m.network.tx_bps)},
        "gpu": {"available": m.gpu.available, "name": m.gpu.name, "utilization_pct": r1(m.gpu.utilization_pct), "power_w": m.gpu.power_w.map(r2)},
        "neural": {"available": m.neural.available, "power_w": r2(m.neural.power_w)},
        "battery": {"present": m.battery.present, "percent": r1(m.battery.percent), "charging": m.battery.charging,
                    "on_ac": m.battery.on_ac, "time_remaining_min": m.battery.time_remaining_min, "power_w": m.battery.power_w.map(r2)},
        "temperature": {"cpu_c": m.sensors.cpu_temp_c.map(r1), "gpu_c": m.sensors.gpu_temp_c.map(r1)},
        "power_w": m.power.system_w.map(r2),
        "busiest": s.top_apps(RankMetric::Cpu, 3).iter().map(|a| app_row(a, RankMetric::Cpu)).collect::<Vec<_>>(),
    })
}

fn r0(x: f64) -> f64 {
    x.round()
}
fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}
fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

pub fn app_row(a: &AppGroup, m: RankMetric) -> Value {
    json!({
        "app": a.app.name,
        "bundle_id": a.app.bundle_id,
        "value": r2(a.metric(m)),
        "metric": m.as_str(),
        "cpu_pct": r1(a.cpu_pct),
        "mem_bytes": a.mem_bytes,
        "energy_w": r2(a.energy_w),
        "process_count": a.process_count,
    })
}

pub fn top(s: &Snapshot, m: RankMetric, limit: usize) -> Value {
    json!({"metric": m.as_str(), "apps": s.top_apps(m, limit).iter().map(|a| app_row(a, m)).collect::<Vec<_>>()})
}

fn project_of(s: &Snapshot, pid: i32) -> Option<String> {
    s.projects.iter().find(|p| p.pids.contains(&pid)).map(|p| p.project.name.clone())
}

pub fn port_row(s: &Snapshot, l: &ListeningPort) -> Value {
    let p = s.process(l.pid);
    let srv = s.projects.iter().flat_map(|x| x.servers.iter()).find(|x| x.process_pid == l.pid && x.port == l.port);
    json!({
        "port": l.port,
        "address": l.address,
        "pid": l.pid,
        "process": p.map(|p| p.name.clone()).unwrap_or_else(|| l.command.clone()),
        "command": p.map(|p| p.argv.join(" ")).unwrap_or_default(),
        "app": p.map(|p| p.app_name.clone()),
        "project": project_of(s, l.pid),
        "mem_bytes": p.map(|p| p.mem_bytes),
        "idle_secs": srv.map(|x| x.idle_secs),
    })
}

pub fn ports(s: &Snapshot) -> Value {
    json!({"ports": s.ports.iter().map(|l| port_row(s, l)).collect::<Vec<_>>()})
}

pub fn what_is_on_port(s: &Snapshot, port: u16) -> Value {
    let rows: Vec<Value> = s.ports.iter().filter(|l| l.port == port).map(|l| port_row(s, l)).collect();
    json!({"port": port, "in_use": !rows.is_empty(), "listeners": rows})
}

pub fn projects(s: &Snapshot) -> Value {
    json!({
        "idle_banner": projects::idle_banner(&s.projects),
        "projects": s.projects.iter().map(|p| json!({
            "name": p.project.name,
            "folder_path": p.project.folder_path,
            "process_count": p.process_count,
            "mem_bytes": p.mem_bytes,
            "cpu_pct": r1(p.cpu_pct),
            "ports": p.ports,
            "state": p.state,
            "servers": p.servers.iter().map(|d| json!({
                "pid": d.process_pid, "port": d.port, "command": d.command, "mem_bytes": d.mem_bytes,
                "idle_secs": d.idle_secs, "idle": d.idle, "started_at": d.started_at,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()
    })
}

pub fn agents(s: &Snapshot) -> Value {
    json!({"agents": s.agents.iter().map(|a| json!({
        "agent": a.agent_kind.label(),
        "kind": a.agent_kind,
        "pid": a.pid,
        "project": a.project,
        "branch": a.branch_or_task,
        "state": a.state,
        "started_at": a.started_at,
        "mem_bytes": a.mem_bytes,
        "cpu_pct": r1(a.cpu_percent),
        "started": a.started.iter().map(|x| json!({"pid": x.pid, "name": x.name, "role": x.role, "mem_bytes": x.mem_bytes, "ports": x.ports})).collect::<Vec<_>>(),
    })).collect::<Vec<_>>()})
}

pub fn limits(s: &Snapshot) -> Value {
    let now = s.timestamp / 1000;
    json!({"plans": s.plans.iter().map(|p| json!({
        "provider": p.provider, "plan": p.plan, "window": p.window, "limit_kind": p.limit_kind,
        "used_pct": r1(p.used_pct), "resets_at": p.resets_at, "pace": p.pace_state, "pace_text": p.pace_text,
        "headline": limits::headline(p, now), "source": p.source,
    })).collect::<Vec<_>>()})
}

/// `config login --json`: a stable `status` token plus a human `message` that may be reworded.
pub fn login(status: &LoginStatus) -> Value {
    json!({"login": {"enabled": status.is_on(), "status": status.token(), "message": status.describe()}})
}

pub fn containers(s: &Snapshot) -> Value {
    json!({
        "docker_available": s.docker_available,
        "stacks": docker::stacks(&s.containers).iter().map(|(st, cs)| json!({
            "name": st.name, "project_hint": st.project_hint,
            "containers": cs.iter().map(|c| json!({
                "id": c.id, "name": c.name, "image": c.image, "state": c.state, "status": c.status, "up": c.up(),
                "mem_bytes": c.mem_bytes, "mem_limit": c.mem_limit, "mem_pct_of_limit": c.mem_pct_of_limit().map(r1),
                "cpu_pct": r1(c.cpu_pct), "published_ports": c.published_ports,
            })).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
        "near_limit": docker::near_limit(&s.containers).iter().map(|c| docker::card_line(c)).collect::<Vec<_>>(),
    })
}

/// Explains what is slow and why: busiest apps per resource + notable conditions.
pub fn diagnosis(s: &Snapshot, uid: u32) -> Value {
    let mut notes = Vec::new();
    let m = &s.machine;
    if m.cpu.total_pct > 80.0 {
        notes.push(format!("CPU is at {:.0} %", m.cpu.total_pct));
    }
    if m.memory.total > 0 && m.memory.swap_used > 1_000_000_000 {
        notes.push(format!("{:.1} GB of swap in use — memory pressure", m.memory.swap_used as f64 / 1e9));
    }
    if let Some(b) = projects::idle_banner(&s.projects) {
        notes.push(b);
    }
    for c in docker::near_limit(&s.containers) {
        notes.push(format!("container near memory limit: {}", docker::card_line(c)));
    }
    json!({
        "notes": notes,
        "top_cpu": s.top_apps(RankMetric::Cpu, 5).iter().map(|a| app_row(a, RankMetric::Cpu)).collect::<Vec<_>>(),
        "top_memory": s.top_apps(RankMetric::Memory, 5).iter().map(|a| app_row(a, RankMetric::Memory)).collect::<Vec<_>>(),
        "my_process_count": mine(s, uid).count(),
    })
}

/// Restrict a snapshot to the current user's processes (MCP data boundary). App rows are
/// re-aggregated from the remaining processes; machine-wide totals stay as they are.
pub fn scope_to_user(s: &Snapshot, uid: u32) -> Snapshot {
    let mut out = s.clone();
    out.processes.retain(|p| p.uid == uid);
    let by_pid: std::collections::HashMap<i32, &Process> = out.processes.iter().map(|p| (p.pid, p)).collect();
    out.ports.retain(|l| by_pid.contains_key(&l.pid));
    out.apps = s
        .apps
        .iter()
        .filter_map(|a| {
            let procs: Vec<&&Process> = a.pids.iter().filter_map(|pid| by_pid.get(pid)).collect();
            if procs.is_empty() {
                return None;
            }
            let mut g = a.clone();
            g.pids = procs.iter().map(|p| p.pid).collect();
            g.process_count = procs.len();
            g.cpu_pct = procs.iter().map(|p| p.cpu_pct).sum();
            g.mem_bytes = procs.iter().map(|p| p.mem_bytes).sum();
            g.energy_w = procs.iter().map(|p| p.energy_w).sum();
            g.disk_read_bps = procs.iter().map(|p| p.disk_read_bps).sum();
            g.disk_write_bps = procs.iter().map(|p| p.disk_write_bps).sum();
            g.net_rx_bps = procs.iter().map(|p| p.net_rx_bps).sum();
            g.net_tx_bps = procs.iter().map(|p| p.net_tx_bps).sum();
            Some(g)
        })
        .collect();
    out
}
