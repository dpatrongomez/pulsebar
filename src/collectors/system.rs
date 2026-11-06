//! CPU, memory, disk and network collectors (cheap; run on every tick).

use std::time::Instant;

use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System};

use super::MetricCollector;
use crate::model::{InterfaceMetrics, MachineMetrics, MetricKind, Volume};
use crate::sys;

pub struct CpuCollector {
    sys: System,
    prev_ticks: Option<[u64; 4]>,
}

impl CpuCollector {
    pub fn new() -> Self {
        let sys = System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_cpu_usage()));
        Self { sys, prev_ticks: sys::host_cpu_ticks() }
    }
}

impl Default for CpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// (user%, system%, idle%) from two tick snapshots.
pub fn split_ticks(prev: [u64; 4], cur: [u64; 4]) -> Option<(f64, f64, f64)> {
    let d: Vec<f64> = (0..4).map(|i| cur[i].saturating_sub(prev[i]) as f64).collect();
    let total: f64 = d.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let user = (d[0] + d[3]) * 100.0 / total;
    let system = d[1] * 100.0 / total;
    let idle = d[2] * 100.0 / total;
    Some((user, system, idle))
}

impl MetricCollector for CpuCollector {
    fn kind(&self) -> MetricKind {
        MetricKind::Cpu
    }

    fn collect(&mut self, out: &mut MachineMetrics) {
        self.sys.refresh_cpu_usage();
        let c = &mut out.cpu;
        c.per_core = self.sys.cpus().iter().map(|x| x.cpu_usage() as f64).collect();
        c.core_count = c.per_core.len();
        let la = System::load_average();
        c.load_avg = [la.one, la.five, la.fifteen];
        let cur = sys::host_cpu_ticks();
        match self.prev_ticks.zip(cur).and_then(|(p, c2)| split_ticks(p, c2)) {
            Some((u, s, i)) => {
                c.user_pct = u;
                c.system_pct = s;
                c.idle_pct = i;
                c.total_pct = (u + s).min(100.0);
            }
            None => {
                let total = self.sys.global_cpu_usage() as f64;
                c.total_pct = total;
                c.user_pct = total;
                c.system_pct = 0.0;
                c.idle_pct = 100.0 - total;
            }
        }
        self.prev_ticks = cur;
    }
}

pub struct MemoryCollector {
    sys: System,
}

impl MemoryCollector {
    pub fn new() -> Self {
        Self { sys: System::new_with_specifics(RefreshKind::nothing().with_memory(MemoryRefreshKind::everything())) }
    }
}

impl Default for MemoryCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricCollector for MemoryCollector {
    fn kind(&self) -> MetricKind {
        MetricKind::Memory
    }

    fn collect(&mut self, out: &mut MachineMetrics) {
        self.sys.refresh_memory();
        let m = &mut out.memory;
        m.total = self.sys.total_memory();
        m.swap_total = self.sys.total_swap();
        m.swap_used = self.sys.used_swap();
        if let Some(vm) = sys::vm_breakdown() {
            m.app = vm.app;
            m.wired = vm.wired;
            m.compressed = vm.compressed;
            m.cached = vm.cached;
            m.free = vm.free;
            m.used = (vm.app + vm.wired + vm.compressed).min(m.total);
        } else {
            m.used = self.sys.used_memory();
            m.app = m.used;
            m.free = self.sys.free_memory();
            m.cached = self.sys.available_memory().saturating_sub(m.free);
        }
    }
}

pub struct DiskCollector {
    disks: Disks,
    ticks: u64,
    /// Linux: last system-wide I/O counters, for throughput.
    prev_io: Option<(Instant, (u64, u64))>,
}

impl DiskCollector {
    pub fn new() -> Self {
        Self { disks: Disks::new_with_refreshed_list(), ticks: 0, prev_io: None }
    }
}

impl Default for DiskCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Volumes a person thinks of as theirs: the root disk, external drives, and on Linux a separate
/// `/home` or a mounted data disk. Skips system mounts such as `/boot` and snap loop images.
pub fn is_user_volume(mount: &str) -> bool {
    mount == "/"
        || mount == "/home"
        || (mount.starts_with("/Volumes/") && !mount.starts_with("/Volumes/com.apple"))
        || ["/mnt/", "/media/", "/run/media/"].iter().any(|p| mount.starts_with(p))
}

impl MetricCollector for DiskCollector {
    fn kind(&self) -> MetricKind {
        MetricKind::Disk
    }

    /// Volumes. On macOS throughput (`read_bps`/`write_bps`) is filled by the sampler from the
    /// per-process I/O counters, which also gives per-app attribution. Linux hides other users'
    /// counters, so there the total comes from the kernel's per-disk statistics.
    fn collect(&mut self, out: &mut MachineMetrics) {
        if cfg!(target_os = "linux")
            && let Some(cur) = crate::linux::disk_counters()
        {
            let now = Instant::now();
            if let Some((t0, (r0, w0))) = self.prev_io {
                let dt = now.duration_since(t0).as_secs_f64().max(0.05);
                out.disk.read_bps = cur.0.saturating_sub(r0) as f64 / dt;
                out.disk.write_bps = cur.1.saturating_sub(w0) as f64 / dt;
            }
            self.prev_io = Some((now, cur));
        }
        if self.ticks.is_multiple_of(30) {
            self.disks.refresh(true);
        }
        self.ticks += 1;
        out.disk.volumes = self
            .disks
            .list()
            .iter()
            .filter(|d| is_user_volume(&d.mount_point().to_string_lossy()))
            .map(|d| Volume {
                name: d.name().to_string_lossy().into_owned(),
                mount_point: d.mount_point().to_string_lossy().into_owned(),
                total: d.total_space(),
                free: d.available_space(),
                removable: d.is_removable(),
            })
            .collect();
    }
}

pub struct NetworkCollector {
    nets: Networks,
    last: Instant,
}

impl NetworkCollector {
    pub fn new() -> Self {
        Self { nets: Networks::new_with_refreshed_list(), last: Instant::now() }
    }
}

impl Default for NetworkCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Interfaces whose traffic counts towards the machine's total: skip loopback, tunnels and AWDL,
/// plus the Linux virtual interfaces that would count container and VPN traffic twice.
pub fn counts_towards_total(name: &str) -> bool {
    const SKIP: [&str; 17] = ["lo", "utun", "awdl", "llw", "gif", "stf", "anpi", "bridge", "ap", "veth", "docker", "br-", "virbr", "vnet", "tun", "tap", "wg"];
    !SKIP.iter().any(|p| name.starts_with(p))
}

impl MetricCollector for NetworkCollector {
    fn kind(&self) -> MetricKind {
        MetricKind::Network
    }

    fn collect(&mut self, out: &mut MachineMetrics) {
        self.nets.refresh(true);
        let dt = self.last.elapsed().as_secs_f64().max(0.05);
        self.last = Instant::now();
        let mut ifs: Vec<InterfaceMetrics> = self
            .nets
            .list()
            .iter()
            .map(|(name, d)| InterfaceMetrics {
                name: name.clone(),
                rx_bps: d.received() as f64 / dt,
                tx_bps: d.transmitted() as f64 / dt,
                total_rx: d.total_received(),
                total_tx: d.total_transmitted(),
            })
            .collect();
        ifs.sort_by(|a, b| a.name.cmp(&b.name));
        out.network.rx_bps = ifs.iter().filter(|i| counts_towards_total(&i.name)).map(|i| i.rx_bps).sum();
        out.network.tx_bps = ifs.iter().filter(|i| counts_towards_total(&i.name)).map(|i| i.tx_bps).sum();
        out.network.interfaces = ifs.into_iter().filter(|i| i.total_rx + i.total_tx > 0).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_split_sums_to_100() {
        let (u, s, i) = split_ticks([100, 50, 1000, 0], [160, 80, 1210, 10]).unwrap();
        assert!((u + s + i - 100.0).abs() < 1e-9);
        assert!(u > s);
        assert!(split_ticks([1, 1, 1, 1], [1, 1, 1, 1]).is_none());
    }

    #[test]
    fn interface_filter() {
        assert!(counts_towards_total("en0"));
        assert!(!counts_towards_total("lo0"));
        assert!(!counts_towards_total("utun3"));
        assert!(is_user_volume("/"));
        assert!(!is_user_volume("/System/Volumes/Data"));
        assert!(counts_towards_total("eth0") && counts_towards_total("wlp2s0") && counts_towards_total("enp3s0"));
        assert!(!counts_towards_total("veth12ab") && !counts_towards_total("docker0") && !counts_towards_total("br-1a2b") && !counts_towards_total("wg0"));
        assert!(is_user_volume("/home") && is_user_volume("/media/me/USB") && is_user_volume("/run/media/me/USB") && is_user_volume("/mnt/data"));
        assert!(!is_user_volume("/boot/efi") && !is_user_volume("/snap/core22/1234") && !is_user_volume("/var/lib/docker"));
    }
}
