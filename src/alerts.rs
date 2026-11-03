//! M7 — Local alert rules (no network).
//!
//! * CPU average > 70 % for ≥ 10 min (window must be fully covered).
//! * One app's memory grows > 1 GB/h *sustained*: the window is split into segments and every
//!   segment must grow, so a single step (a one-minute batch job) never fires.
//! * Disk writes / network sustained above a threshold for ≥ 5 min (≥ 80 % of samples above).

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::model::{AlertKind, AlertRule, Snapshot};
use crate::units::{format_bytes, format_duration};

pub fn default_rules(disk_bps: f64, net_bps: f64) -> Vec<AlertRule> {
    vec![
        AlertRule { kind: AlertKind::CpuSustained, threshold: 70.0, duration_s: 600, enabled: true },
        AlertRule { kind: AlertKind::AppMemoryGrowth, threshold: 1e9, duration_s: 1800, enabled: true },
        AlertRule { kind: AlertKind::DiskWriteSustained, threshold: disk_bps, duration_s: 300, enabled: true },
        AlertRule { kind: AlertKind::NetworkSustained, threshold: net_bps, duration_s: 300, enabled: true },
    ]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub kind: AlertKind,
    /// App group key ("bundle:com.x") or "" for machine-wide alerts.
    pub app_key: String,
    pub app_name: String,
    pub message: String,
    pub at: i64,
}

#[derive(Default)]
struct Series {
    pts: VecDeque<(i64, f64)>,
}

impl Series {
    fn push(&mut self, t: i64, v: f64, keep: i64) {
        self.pts.push_back((t, v));
        while self.pts.front().is_some_and(|(t0, _)| *t0 < t - keep) {
            self.pts.pop_front();
        }
    }
    fn window(&self, now: i64, dur: i64) -> Vec<(i64, f64)> {
        self.pts.iter().filter(|(t, _)| *t >= now - dur).copied().collect()
    }
    /// True when samples exist from (almost) the start of the window.
    fn covers(&self, now: i64, dur: i64) -> bool {
        self.pts.front().is_some_and(|(t0, _)| *t0 <= now - dur + dur / 20)
    }
}

fn slope_per_s(pts: &[(i64, f64)]) -> f64 {
    let n = pts.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let mt = pts.iter().map(|p| p.0 as f64).sum::<f64>() / n;
    let mv = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let num: f64 = pts.iter().map(|p| (p.0 as f64 - mt) * (p.1 - mv)).sum();
    let den: f64 = pts.iter().map(|p| (p.0 as f64 - mt).powi(2)).sum();
    if den == 0.0 { 0.0 } else { num / den }
}

/// Sustained growth: overall slope above `per_s` and every one of `segments` slices grows.
pub fn sustained_growth(pts: &[(i64, f64)], per_s: f64, segments: usize) -> bool {
    if pts.len() < segments * 2 || slope_per_s(pts) < per_s {
        return false;
    }
    let t0 = pts[0].0;
    let t1 = pts[pts.len() - 1].0;
    let span = (t1 - t0).max(1) as f64;
    let mut prev_end: Option<f64> = None;
    for i in 0..segments {
        let a = t0 as f64 + span * i as f64 / segments as f64;
        let b = t0 as f64 + span * (i + 1) as f64 / segments as f64;
        let seg: Vec<(i64, f64)> = pts.iter().filter(|p| (p.0 as f64) >= a && (p.0 as f64) <= b).copied().collect();
        if seg.len() < 2 {
            return false;
        }
        let growth = seg[seg.len() - 1].1 - seg[0].1;
        // Each slice must contribute at least a quarter of its fair share of the growth.
        if growth < per_s * span / segments as f64 * 0.25 {
            return false;
        }
        if let Some(pe) = prev_end
            && seg[0].1 < pe - per_s * span {
                return false;
            }
        prev_end = Some(seg[seg.len() - 1].1);
    }
    true
}

pub struct AlertEngine {
    pub rules: Vec<AlertRule>,
    cpu: Series,
    disk: Series,
    net: Series,
    app_mem: HashMap<String, (String, Series)>,
    last_app_sample: i64,
    fired: HashMap<(AlertKind, String), i64>,
    snoozed: HashMap<(AlertKind, String), i64>,
    pub cooldown_s: i64,
}

impl AlertEngine {
    pub fn new(rules: Vec<AlertRule>) -> Self {
        Self {
            rules,
            cpu: Series::default(),
            disk: Series::default(),
            net: Series::default(),
            app_mem: HashMap::new(),
            last_app_sample: 0,
            fired: HashMap::new(),
            snoozed: HashMap::new(),
            cooldown_s: 3600,
        }
    }

    fn rule(&self, k: AlertKind) -> Option<&AlertRule> {
        self.rules.iter().find(|r| r.kind == k && r.enabled)
    }

    fn max_keep(&self) -> i64 {
        self.rules.iter().map(|r| r.duration_s).max().unwrap_or(600) + 60
    }

    /// "Silence for 24 h".
    pub fn snooze(&mut self, kind: AlertKind, app_key: &str, until: i64) {
        self.snoozed.insert((kind, app_key.to_string()), until);
    }

    /// Feed raw values (separated from `Snapshot` for synthetic tests).
    pub fn feed(&mut self, now: i64, cpu: f64, disk_write: f64, net: f64, apps: &[(String, String, f64)]) -> Vec<Alert> {
        let keep = self.max_keep();
        self.cpu.push(now, cpu, keep);
        self.disk.push(now, disk_write, keep);
        self.net.push(now, net, keep);
        // App memory every 30 s is plenty for a GB/h trend.
        if now - self.last_app_sample >= 30 {
            self.last_app_sample = now;
            for (key, name, mem) in apps {
                let e = self.app_mem.entry(key.clone()).or_insert_with(|| (name.clone(), Series::default()));
                e.1.push(now, *mem, keep);
            }
            let alive: std::collections::HashSet<&String> = apps.iter().map(|a| &a.0).collect();
            self.app_mem.retain(|k, _| alive.contains(k));
        }
        self.evaluate(now)
    }

    pub fn feed_snapshot(&mut self, s: &Snapshot) -> Vec<Alert> {
        let apps: Vec<(String, String, f64)> =
            s.apps.iter().filter(|a| a.key != crate::grouping::SYSTEM_KEY).map(|a| (a.key.clone(), a.app.name.clone(), a.mem_bytes as f64)).collect();
        self.feed(
            s.timestamp / 1000,
            s.machine.cpu.total_pct,
            s.machine.disk.write_bps,
            s.machine.network.rx_bps + s.machine.network.tx_bps,
            &apps,
        )
    }

    fn evaluate(&mut self, now: i64) -> Vec<Alert> {
        let mut candidates: Vec<Alert> = Vec::new();
        let sustained = |s: &Series, r: &AlertRule| -> Option<f64> {
            if !s.covers(now, r.duration_s) {
                return None;
            }
            let w = s.window(now, r.duration_s);
            let avg = w.iter().map(|p| p.1).sum::<f64>() / w.len().max(1) as f64;
            let above = w.iter().filter(|p| p.1 > r.threshold).count() as f64 / w.len().max(1) as f64;
            (avg > r.threshold && above >= 0.8).then_some(avg)
        };
        if let Some(r) = self.rule(AlertKind::CpuSustained)
            && self.cpu.covers(now, r.duration_s) {
                let w = self.cpu.window(now, r.duration_s);
                let avg = w.iter().map(|p| p.1).sum::<f64>() / w.len().max(1) as f64;
                if avg > r.threshold {
                    candidates.push(Alert {
                        kind: r.kind,
                        app_key: String::new(),
                        app_name: String::new(),
                        message: format!("CPU averaged {avg:.0} % for the last {}", format_duration(r.duration_s)),
                        at: now,
                    });
                }
            }
        if let Some(r) = self.rule(AlertKind::DiskWriteSustained)
            && let Some(avg) = sustained(&self.disk, r) {
                candidates.push(Alert {
                    kind: r.kind,
                    app_key: String::new(),
                    app_name: String::new(),
                    message: format!("Disk writes sustained at {}/s for {}", format_bytes(avg), format_duration(r.duration_s)),
                    at: now,
                });
            }
        if let Some(r) = self.rule(AlertKind::NetworkSustained)
            && let Some(avg) = sustained(&self.net, r) {
                candidates.push(Alert {
                    kind: r.kind,
                    app_key: String::new(),
                    app_name: String::new(),
                    message: format!("Network traffic sustained at {}/s for {}", format_bytes(avg), format_duration(r.duration_s)),
                    at: now,
                });
            }
        if let Some(r) = self.rule(AlertKind::AppMemoryGrowth).cloned() {
            for (key, (name, s)) in &self.app_mem {
                if !s.covers(now, r.duration_s) {
                    continue;
                }
                let w = s.window(now, r.duration_s);
                if sustained_growth(&w, r.threshold / 3600.0, 3) {
                    let per_h = slope_per_s(&w) * 3600.0;
                    candidates.push(Alert {
                        kind: r.kind,
                        app_key: key.clone(),
                        app_name: name.clone(),
                        message: format!("{name} memory is growing {}/h (now {})", format_bytes(per_h), format_bytes(w.last().map(|p| p.1).unwrap_or(0.0))),
                        at: now,
                    });
                }
            }
        }
        candidates
            .into_iter()
            .filter(|a| {
                let k = (a.kind, a.app_key.clone());
                if self.snoozed.get(&k).is_some_and(|u| *u > now) {
                    return false;
                }
                if self.fired.get(&k).is_some_and(|t| now - *t < self.cooldown_s) {
                    return false;
                }
                self.fired.insert(k, now);
                true
            })
            .collect()
    }
}

/// Notify about a fired alert: a native notification with actions (see `gui::notify`) when the
/// app is bundled and the user allows it, otherwise the osascript fallback.
pub fn notify_alert(a: &Alert) {
    #[cfg(all(feature = "gui", target_os = "macos"))]
    if crate::gui::notify::deliver(a) {
        return;
    }
    notify(if a.app_name.is_empty() { "Alert" } else { &a.app_name }, &a.message);
}

/// Fallback notification (osascript; no network), attributed to Script Editor. Actions are only
/// offered in the app's alert banner.
pub fn notify(title: &str, body: &str) {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("display notification \"{}\" with title \"PulseBar\" subtitle \"{}\"", esc(body), esc(title));
    let _ = std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> AlertEngine {
        AlertEngine::new(default_rules(100e6, 50e6))
    }

    #[test]
    fn cpu_sustained_fires_once() {
        let mut e = engine();
        let mut fired = vec![];
        for t in 0..900 {
            fired.extend(e.feed(t, 85.0, 0.0, 0.0, &[]));
        }
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].kind, AlertKind::CpuSustained);
        assert!(fired[0].at >= 570, "needs full 10 min window, fired at {}", fired[0].at);
    }

    #[test]
    fn one_minute_batch_is_not_an_alert() {
        let mut e = engine();
        let mut fired = vec![];
        let mut mem = 2e9;
        for t in 0..3600 {
            let batch = (1200..1260).contains(&t);
            if batch {
                mem += 1.2e9 / 60.0; // +1.2 GB in one minute, then flat
            }
            let apps = vec![("bundle:com.batch".to_string(), "Batch".to_string(), mem)];
            fired.extend(e.feed(t, if batch { 100.0 } else { 20.0 }, if batch { 500e6 } else { 1e6 }, 1e3, &apps));
        }
        assert!(fired.is_empty(), "false positives: {fired:?}");
    }

    #[test]
    fn leak_fires() {
        let mut e = engine();
        let mut fired = vec![];
        for t in 0..3600 {
            let mem = 1e9 + t as f64 * (1.5e9 / 3600.0); // 1.5 GB/h steady
            let apps = vec![("bundle:com.leaky".to_string(), "Leaky".to_string(), mem)];
            fired.extend(e.feed(t, 10.0, 0.0, 0.0, &apps));
        }
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].kind, AlertKind::AppMemoryGrowth);
        assert_eq!(fired[0].app_name, "Leaky");
    }

    #[test]
    fn disk_sustained_and_snooze() {
        let mut e = engine();
        e.snooze(AlertKind::NetworkSustained, "", 10_000);
        let mut fired = vec![];
        for t in 0..400 {
            fired.extend(e.feed(t, 5.0, 150e6, 80e6, &[]));
        }
        let kinds: Vec<_> = fired.iter().map(|a| a.kind).collect();
        assert_eq!(kinds, vec![AlertKind::DiskWriteSustained]);
    }
}
