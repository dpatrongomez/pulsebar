//! Process table sampler: sysinfo (libproc / KERN_PROCARGS2 under the hood) for the tree, argv,
//! cwd and CPU%, plus `proc_pid_rusage` for physical footprint, peak memory, CPU times, disk I/O
//! and billed energy. Per-process network comes from `nettop` at a slow cadence.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use super::Background;
use crate::model::Process;
use crate::sys;

/// Parse `nettop -P -L 1 -n -x -J bytes_in,bytes_out` (CSV, first column "name.pid").
pub fn parse_nettop(out: &str) -> HashMap<i32, (u64, u64)> {
    let mut m = HashMap::new();
    for line in out.lines().skip(1) {
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 3 {
            continue;
        }
        let Some((_, pid)) = cols[0].rsplit_once('.') else { continue };
        let (Ok(pid), Ok(rx), Ok(tx)) = (pid.parse::<i32>(), cols[1].trim().parse::<u64>(), cols[2].trim().parse::<u64>()) else {
            continue;
        };
        m.insert(pid, (rx, tx));
    }
    m
}

type NetRates = HashMap<i32, (f64, f64)>;

struct NettopState {
    prev: Option<(Instant, HashMap<i32, (u64, u64)>)>,
}

#[derive(Default, Clone, Copy)]
struct Prev {
    energy_nj: u64,
    disk_read: u64,
    disk_written: u64,
}

pub struct ProcessSampler {
    sys: System,
    last: Instant,
    ticks: u64,
    prev: HashMap<i32, Prev>,
    /// pid → (start_time, responsible pid); immutable for a process' lifetime.
    responsible: HashMap<i32, (u64, i32)>,
    net: Option<Background<NetRates>>,
}

impl ProcessSampler {
    /// `with_network`: spawn the nettop worker (the long-running app does; one-shot CLI doesn't).
    pub fn new(with_network: bool) -> Self {
        let net = with_network.then(|| {
            Background::spawn("nettop", Duration::from_secs(15), || NettopState { prev: None }, |st: &mut NettopState| {
                let out = sys::run("/usr/bin/nettop", &["-P", "-L", "1", "-n", "-x", "-J", "bytes_in,bytes_out"], Duration::from_secs(5))?;
                let cur = parse_nettop(&out);
                let now = Instant::now();
                let rates = match st.prev.take() {
                    Some((t0, prev)) => {
                        let dt = now.duration_since(t0).as_secs_f64().max(0.1);
                        cur.iter()
                            .map(|(pid, (rx, tx))| {
                                let (prx, ptx) = prev.get(pid).copied().unwrap_or((*rx, *tx));
                                (*pid, (rx.saturating_sub(prx) as f64 / dt, tx.saturating_sub(ptx) as f64 / dt))
                            })
                            .collect()
                    }
                    None => HashMap::new(),
                };
                st.prev = Some((now, cur));
                Some(rates)
            })
        });
        Self { sys: System::new(), last: Instant::now(), ticks: 0, prev: HashMap::new(), responsible: HashMap::new(), net }
    }

    pub fn sample(&mut self) -> Vec<Process> {
        // argv/exe/user never change; cwd can, so refresh it every 15 ticks.
        let cwd = if self.ticks.is_multiple_of(15) { UpdateKind::Always } else { UpdateKind::OnlyIfNotSet };
        let kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_user(UpdateKind::OnlyIfNotSet)
            .with_cwd(cwd);
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        self.ticks += 1;
        let dt = self.last.elapsed().as_secs_f64().max(0.05);
        self.last = Instant::now();
        let ncpu = self.sys.cpus().len().max(1);
        let ncpu = if ncpu == 1 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) } else { ncpu };
        let net = self.net.as_ref().and_then(|n| n.latest()).unwrap_or_default();

        let mut next_prev = HashMap::with_capacity(self.sys.processes().len());
        let mut next_resp = HashMap::with_capacity(self.sys.processes().len());
        let mut out = Vec::with_capacity(self.sys.processes().len());
        for (pid, p) in self.sys.processes() {
            let pid = pid.as_u32() as i32;
            let ru = sys::rusage(pid);
            let prev = self.prev.get(&pid).copied();
            let resp = match self.responsible.get(&pid) {
                Some((st, r)) if *st == p.start_time() => *r,
                _ => sys::responsible_pid(pid),
            };
            next_resp.insert(pid, (p.start_time(), resp));
            let mut proc_ = Process {
                pid,
                ppid: p.parent().map(|x| x.as_u32() as i32).unwrap_or(0),
                responsible_pid: resp,
                uid: p.user_id().map(|u| **u).unwrap_or(u32::MAX),
                name: p.name().to_string_lossy().into_owned(),
                executable_path: p.exe().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default(),
                argv: p.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect(),
                cwd: p.cwd().map(|c| c.to_string_lossy().into_owned()).unwrap_or_default(),
                start_time: p.start_time() as i64,
                // sysinfo reports per-core % (0..n×100); normalise to % of the Mac.
                cpu_pct: p.cpu_usage() as f64 / ncpu as f64,
                mem_bytes: p.memory(),
                ..Default::default()
            };
            if let Some(ru) = ru {
                proc_.mem_bytes = if ru.phys_footprint > 0 { ru.phys_footprint } else { proc_.mem_bytes };
                proc_.mem_peak = ru.peak_footprint;
                proc_.cpu_time_user_s = ru.user_s;
                proc_.cpu_time_system_s = ru.system_s;
                if let Some(pr) = prev {
                    proc_.energy_w = ru.energy_nj.saturating_sub(pr.energy_nj) as f64 / 1e9 / dt;
                    proc_.disk_read_bps = ru.disk_read.saturating_sub(pr.disk_read) as f64 / dt;
                    proc_.disk_write_bps = ru.disk_written.saturating_sub(pr.disk_written) as f64 / dt;
                }
                next_prev.insert(pid, Prev { energy_nj: ru.energy_nj, disk_read: ru.disk_read, disk_written: ru.disk_written });
            }
            if let Some((rx, tx)) = net.get(&pid) {
                proc_.net_rx_bps = *rx;
                proc_.net_tx_bps = *tx;
            }
            if proc_.name.is_empty() {
                proc_.name = proc_.executable_path.rsplit('/').next().unwrap_or("?").to_string();
            }
            out.push(proc_);
        }
        self.prev = next_prev;
        self.responsible = next_resp;
        out.sort_by_key(|p| p.pid);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nettop_csv() {
        let out = ",bytes_in,bytes_out,\nlaunchd.1,0,0,\nGoogle Chrome H.1234,12345,678,\nbad line\n";
        let m = parse_nettop(out);
        assert_eq!(m.get(&1234), Some(&(12345, 678)));
        assert_eq!(m.len(), 2);
    }
}
