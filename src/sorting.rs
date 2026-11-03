//! Column sorting for the app list and the expanded process list. Pure logic (no GUI types) so
//! it can be unit-tested; the choice is persisted per tab in `Settings::list_sort`.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::model::{AppGroup, Process, RankMetric};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    Name,
    Pid,
    Cpu,
    Memory,
    Energy,
    Disk,
    Network,
}

impl SortKey {
    pub fn from_metric(m: RankMetric) -> Self {
        match m {
            RankMetric::Cpu => SortKey::Cpu,
            RankMetric::Memory => SortKey::Memory,
            RankMetric::Energy => SortKey::Energy,
            RankMetric::Disk => SortKey::Disk,
            RankMetric::Network => SortKey::Network,
        }
    }

    /// Direction a column starts in when first selected: text and PIDs A→Z / low→high,
    /// metrics biggest first.
    pub fn default_desc(self) -> bool {
        !matches!(self, SortKey::Name | SortKey::Pid)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SortSpec {
    pub key: SortKey,
    pub desc: bool,
}

impl SortSpec {
    /// What a tab did before columns were sortable: its own metric, biggest first.
    pub fn for_metric(m: RankMetric) -> Self {
        SortSpec { key: SortKey::from_metric(m), desc: true }
    }

    /// Result of clicking the header of `key`: reverse when it is already the sort column,
    /// otherwise switch to it in its default direction.
    pub fn clicked(self, key: SortKey) -> Self {
        if self.key == key { SortSpec { key, desc: !self.desc } } else { SortSpec { key, desc: key.default_desc() } }
    }

    /// "⏶" / "⏷" when `key` is the active column, else "".
    pub fn arrow(self, key: SortKey) -> &'static str {
        match (self.key == key, self.desc) {
            (false, _) => "",
            (true, true) => "⏷",
            (true, false) => "⏶",
        }
    }
}

/// Persisted sort/column choices of one tab. Every field defaults, so older settings files (and
/// tabs never touched) keep the previous behaviour.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListSort {
    /// App list; `None` = the tab's own metric, descending.
    pub apps: Option<SortSpec>,
    /// Expanded process list; `None` = the tab's own metric, descending.
    pub procs: Option<SortSpec>,
    /// Show CPU and Memory side by side in the app list.
    pub extra_columns: bool,
}

/// Case-insensitive, allocation-free text comparison.
fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

fn app_cmp(a: &AppGroup, b: &AppGroup, key: SortKey) -> Ordering {
    match key {
        SortKey::Name => cmp_ci(&a.app.name, &b.app.name),
        SortKey::Pid => a.root_pid.cmp(&b.root_pid),
        SortKey::Cpu => a.cpu_pct.total_cmp(&b.cpu_pct),
        SortKey::Memory => a.mem_bytes.cmp(&b.mem_bytes),
        SortKey::Energy => a.energy_w.total_cmp(&b.energy_w),
        SortKey::Disk => (a.disk_read_bps + a.disk_write_bps).total_cmp(&(b.disk_read_bps + b.disk_write_bps)),
        SortKey::Network => (a.net_rx_bps + a.net_tx_bps).total_cmp(&(b.net_rx_bps + b.net_tx_bps)),
    }
}

fn proc_cmp(a: &Process, b: &Process, key: SortKey) -> Ordering {
    match key {
        SortKey::Name => cmp_ci(&a.name, &b.name),
        SortKey::Pid => a.pid.cmp(&b.pid),
        SortKey::Cpu => a.cpu_pct.total_cmp(&b.cpu_pct),
        SortKey::Memory => a.mem_bytes.cmp(&b.mem_bytes),
        SortKey::Energy => a.energy_w.total_cmp(&b.energy_w),
        SortKey::Disk => (a.disk_read_bps + a.disk_write_bps).total_cmp(&(b.disk_read_bps + b.disk_write_bps)),
        SortKey::Network => (a.net_rx_bps + a.net_tx_bps).total_cmp(&(b.net_rx_bps + b.net_tx_bps)),
    }
}

fn directed(o: Ordering, desc: bool) -> Ordering {
    if desc { o.reverse() } else { o }
}

/// Sort apps by `spec`; ties fall back to name (A→Z) so the order is deterministic.
pub fn sort_apps(apps: &mut [&AppGroup], spec: SortSpec) {
    apps.sort_by(|a, b| directed(app_cmp(a, b, spec.key), spec.desc).then_with(|| cmp_ci(&a.app.name, &b.app.name)).then_with(|| a.key.cmp(&b.key)));
}

/// Sort processes by `spec`; ties fall back to PID (low→high).
pub fn sort_procs(procs: &mut [&Process], spec: SortSpec) {
    procs.sort_by(|a, b| directed(proc_cmp(a, b, spec.key), spec.desc).then_with(|| a.pid.cmp(&b.pid)));
}

/// Reorder `procs` to follow `order` (PIDs captured while the pointer was over the list).
/// Processes missing from `order` (new since) go last, in their current relative order, so a
/// refresh never moves a row that is already on screen.
pub fn follow_frozen(procs: &mut [&Process], order: &[i32]) {
    let pos = |pid: i32| order.iter().position(|x| *x == pid).unwrap_or(usize::MAX);
    procs.sort_by_key(|p| pos(p.pid));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::App;

    const KEYS: [SortKey; 7] = [SortKey::Name, SortKey::Pid, SortKey::Cpu, SortKey::Memory, SortKey::Energy, SortKey::Disk, SortKey::Network];

    fn proc(pid: i32, name: &str, cpu: f64, mem: u64, energy: f64) -> Process {
        Process { pid, name: name.into(), cpu_pct: cpu, mem_bytes: mem, energy_w: energy, disk_read_bps: cpu, net_rx_bps: energy, ..Default::default() }
    }

    fn app(key: &str, name: &str, cpu: f64, mem: u64) -> AppGroup {
        AppGroup { key: key.into(), app: App { name: name.into(), ..Default::default() }, cpu_pct: cpu, mem_bytes: mem, energy_w: cpu, disk_read_bps: cpu, net_rx_bps: cpu, root_pid: mem as i32, ..Default::default() }
    }

    fn pids(v: &[&Process]) -> Vec<i32> {
        v.iter().map(|p| p.pid).collect()
    }

    fn sample() -> Vec<Process> {
        vec![proc(30, "zsh", 5.0, 300, 0.5), proc(10, "Alpha", 50.0, 100, 2.0), proc(20, "beta", 20.0, 900, 0.1), proc(40, "Gamma", 20.0, 500, 9.0)]
    }

    #[test]
    fn every_process_column_sorts_both_ways() {
        let procs = sample();
        let expect: [(SortKey, Vec<i32>); 5] = [
            (SortKey::Name, vec![10, 20, 40, 30]),   // Alpha beta Gamma zsh (case-insensitive)
            (SortKey::Pid, vec![10, 20, 30, 40]),
            (SortKey::Cpu, vec![30, 20, 40, 10]),    // 20.0 tie broken by pid
            (SortKey::Memory, vec![10, 30, 40, 20]),
            (SortKey::Energy, vec![20, 30, 10, 40]),
        ];
        for (key, asc) in expect {
            let mut v: Vec<&Process> = procs.iter().collect();
            sort_procs(&mut v, SortSpec { key, desc: false });
            assert_eq!(pids(&v), asc, "{key:?} ascending");
            let mut v: Vec<&Process> = procs.iter().collect();
            sort_procs(&mut v, SortSpec { key, desc: true });
            let mut rev = asc.clone();
            rev.reverse();
            // Descending is the exact reverse except where values tie (pid stays ascending).
            if key == SortKey::Cpu {
                assert_eq!(pids(&v), vec![10, 20, 40, 30], "{key:?} descending");
            } else {
                assert_eq!(pids(&v), rev, "{key:?} descending");
            }
        }
    }

    #[test]
    fn every_app_column_sorts_both_ways() {
        let apps = [app("a", "Zed", 5.0, 300), app("b", "alpha", 50.0, 100), app("c", "Mail", 20.0, 900)];
        for key in KEYS {
            for desc in [false, true] {
                let mut v: Vec<&AppGroup> = apps.iter().collect();
                sort_apps(&mut v, SortSpec { key, desc });
                let names: Vec<&str> = v.iter().map(|a| a.app.name.as_str()).collect();
                let want: Vec<&str> = match (key, desc) {
                    (SortKey::Name, false) => vec!["alpha", "Mail", "Zed"],
                    (SortKey::Name, true) => vec!["Zed", "Mail", "alpha"],
                    (SortKey::Memory | SortKey::Pid, false) => vec!["alpha", "Zed", "Mail"],
                    (SortKey::Memory | SortKey::Pid, true) => vec!["Mail", "Zed", "alpha"],
                    (_, false) => vec!["Zed", "Mail", "alpha"], // cpu-derived metrics
                    (_, true) => vec!["alpha", "Mail", "Zed"],
                };
                assert_eq!(names, want, "{key:?} desc={desc}");
            }
        }
    }

    #[test]
    fn nan_does_not_panic_and_ties_are_stable() {
        let procs = [proc(2, "a", f64::NAN, 1, 0.0), proc(1, "b", 1.0, 1, 0.0), proc(3, "c", 1.0, 1, 0.0)];
        let mut v: Vec<&Process> = procs.iter().collect();
        sort_procs(&mut v, SortSpec { key: SortKey::Memory, desc: true });
        assert_eq!(pids(&v), vec![1, 2, 3]);
        sort_procs(&mut v, SortSpec { key: SortKey::Cpu, desc: true });
        assert_eq!(v.len(), 3);
    }

    #[test]
    fn clicking_a_header_switches_then_reverses() {
        let s = SortSpec::for_metric(RankMetric::Cpu);
        assert_eq!(s, SortSpec { key: SortKey::Cpu, desc: true });
        let s = s.clicked(SortKey::Cpu);
        assert_eq!(s, SortSpec { key: SortKey::Cpu, desc: false });
        let s = s.clicked(SortKey::Name);
        assert_eq!(s, SortSpec { key: SortKey::Name, desc: false });
        let s = s.clicked(SortKey::Name);
        assert_eq!(s, SortSpec { key: SortKey::Name, desc: true });
        let s = s.clicked(SortKey::Memory);
        assert_eq!(s, SortSpec { key: SortKey::Memory, desc: true });
        assert_eq!(s.arrow(SortKey::Memory), "⏷");
        assert_eq!(s.clicked(SortKey::Memory).arrow(SortKey::Memory), "⏶");
        assert_eq!(s.arrow(SortKey::Cpu), "");
    }

    #[test]
    fn metric_default_matches_previous_behaviour() {
        for m in [RankMetric::Cpu, RankMetric::Memory, RankMetric::Energy, RankMetric::Disk, RankMetric::Network] {
            let procs = sample();
            let mut v: Vec<&Process> = procs.iter().collect();
            sort_procs(&mut v, SortSpec::for_metric(m));
            let first = v[0];
            let max = |f: fn(&Process) -> f64| procs.iter().map(f).fold(f64::MIN, f64::max);
            let top = match m {
                RankMetric::Cpu => max(|p| p.cpu_pct),
                RankMetric::Memory => max(|p| p.mem_bytes as f64),
                RankMetric::Energy => max(|p| p.energy_w),
                RankMetric::Disk => max(|p| p.disk_read_bps + p.disk_write_bps),
                RankMetric::Network => max(|p| p.net_rx_bps + p.net_tx_bps),
            };
            let got = match m {
                RankMetric::Cpu => first.cpu_pct,
                RankMetric::Memory => first.mem_bytes as f64,
                RankMetric::Energy => first.energy_w,
                RankMetric::Disk => first.disk_read_bps + first.disk_write_bps,
                RankMetric::Network => first.net_rx_bps + first.net_tx_bps,
            };
            assert_eq!(got, top, "{m:?}");
        }
    }

    #[test]
    fn frozen_order_survives_refresh_but_not_a_new_sort() {
        let spec = SortSpec::for_metric(RankMetric::Cpu);
        let before = sample();
        let mut v: Vec<&Process> = before.iter().collect();
        sort_procs(&mut v, spec);
        let frozen: Vec<i32> = pids(&v);
        // Refresh: values change so a fresh CPU sort would reorder the rows...
        let mut after = sample();
        after[0].cpu_pct = 99.0; // pid 30 jumps to the top
        after.push(proc(50, "new", 1000.0, 1, 0.0)); // and a brand-new process appears
        let mut fresh: Vec<&Process> = after.iter().collect();
        sort_procs(&mut fresh, spec);
        assert_ne!(pids(&fresh)[..4], frozen[..]);
        // ...but while frozen the rows stay put and the newcomer goes last.
        let mut kept: Vec<&Process> = after.iter().collect();
        follow_frozen(&mut kept, &frozen);
        assert_eq!(pids(&kept), vec![10, 20, 40, 30, 50]);
        // Clicking a header re-sorts immediately (the GUI drops the frozen order).
        let mut resorted: Vec<&Process> = after.iter().collect();
        sort_procs(&mut resorted, spec.clicked(SortKey::Pid));
        assert_eq!(pids(&resorted), vec![10, 20, 30, 40, 50]);
    }
}
