//! Per-app detail panel: identity (icon, bundle id, processes, ports, code signature), CPU /
//! memory / energy / disk / network history for 1 h / 24 h / 7 d, notable events (alerts,
//! restarts, peaks) and the same Quit / Force Quit actions as the app list.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, RichText};

use super::{PulseApp, widgets};
use crate::actions::{Mode, Target};
use crate::history::{self, AppEvent, Peak};
use crate::inspector::{self, ProcessDetail};
use crate::model::*;
use crate::units::Units;
use crate::{grouping, sys};

/// Ranges offered in the panel: (label, seconds).
const RANGES: [(&str, i64); 3] = [("1 h", 3600), ("24 h", 86_400), ("7 d", 7 * 86_400)];
/// Metrics charted, in display order.
const METRICS: [RankMetric; 5] = [RankMetric::Cpu, RankMetric::Memory, RankMetric::Energy, RankMetric::Disk, RankMetric::Network];

type Points = Vec<(i64, f64, f64, f64)>;

/// Most points drawn per chart; longer series are reduced in `load` (min/avg/max kept).
const MAX_POINTS: i64 = 300;

struct Loaded {
    at: Instant,
    range_s: i64,
    /// Time window shown, [from, to] in seconds.
    from: i64,
    to: i64,
    /// Spacing of the points in `charts`; farther apart than 1.5 steps means no data in between.
    step_s: i64,
    charts: Vec<(RankMetric, Points)>,
    peaks: Vec<(RankMetric, Peak)>,
    events: Vec<AppEvent>,
}

pub struct AppDetailState {
    key: String,
    /// Key of the app's rows in the history database.
    id: String,
    name: String,
    range_s: i64,
    loaded: Option<Loaded>,
    sign: Option<ProcessDetail>,
    sign_rx: Option<Receiver<Option<ProcessDetail>>>,
}

/// `app_<metric>` as stored in history.
fn series_name(m: RankMetric) -> String {
    format!("app_{}", m.as_str())
}

/// Reduce `pts` (spaced `res` seconds apart, covering `range_s` seconds from `from`) to at most
/// `MAX_POINTS` by merging the points of each time bin: the bin's min of mins, mean of avgs and
/// max of maxes. Bins without data stay empty so gaps survive. Returns the points and their spacing.
fn downsample(pts: &[(i64, f64, f64, f64)], from: i64, range_s: i64, res: i64) -> (Points, i64) {
    let bin = (range_s + MAX_POINTS - 1) / MAX_POINTS;
    if bin <= res {
        return (pts.to_vec(), res);
    }
    let mut out: Points = Vec::new();
    // Points merged into the last bin of `out`, whose avg is a running sum until the end.
    let (mut n, mut cur) = (0.0, i64::MIN);
    for p in pts {
        let k = (p.0 - from).div_euclid(bin);
        if k != cur {
            if let Some(o) = out.last_mut() {
                o.2 /= n;
            }
            (cur, n) = (k, 0.0);
            out.push((from + k * bin, p.1, 0.0, p.3));
        }
        let o = out.last_mut().unwrap();
        o.1 = o.1.min(p.1);
        o.3 = o.3.max(p.3);
        o.2 += p.2;
        n += 1.0;
    }
    if let Some(o) = out.last_mut() {
        o.2 /= n;
    }
    (out, bin)
}

/// Load the charts, peaks and events of one app from history.
fn load(h: &history::History, id: &str, range_s: i64) -> Loaded {
    let now = sys::now_s();
    let from = now - range_s;
    let mut charts = Vec::new();
    let mut peaks = Vec::new();
    let mut step_s = 0;
    for m in METRICS {
        let name = series_name(m);
        let pts = h.series(&name, Some(id), from, now, now).unwrap_or_default();
        if let Ok(Some(p)) = h.app_peak(&name, id, from, now, now)
            && p.value > 0.0
        {
            peaks.push((m, p));
        }
        let raw: Points = pts.iter().map(|b| (b.time_bucket, b.value_min, b.value_avg, b.value_max)).collect();
        let (pts, step) = downsample(&raw, from, range_s, pts.first().map_or(1, |b| b.resolution_s));
        step_s = step_s.max(step);
        charts.push((m, pts));
    }
    let events = h.events(id, from, now, 50).unwrap_or_default();
    Loaded { at: Instant::now(), range_s, from, to: now, step_s, charts, peaks, events }
}

fn format_value(u: &Units, m: RankMetric, v: f64, cores: usize) -> String {
    match m {
        RankMetric::Cpu => u.cpu(v, cores),
        RankMetric::Memory => u.bytes(v as u64),
        RankMetric::Energy => super::format_power(v),
        RankMetric::Disk | RankMetric::Network => u.rate(v),
    }
}

fn when(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map(|d| d.with_timezone(&chrono::Local).format("%b %d %H:%M").to_string()).unwrap_or_default()
}

impl PulseApp {
    /// Open the detail panel for the app group `key` (closes the process inspector).
    pub(super) fn open_detail(&mut self, snap: &Arc<Snapshot>, key: &str) {
        let Some(g) = snap.apps.iter().find(|a| a.key == key) else { return };
        let (tx, rx) = channel();
        let s = snap.clone();
        let pid = g.root_pid;
        std::thread::spawn(move || {
            let _ = tx.send(inspector::inspect(&s, pid, true));
        });
        self.inspector = None;
        self.detail = Some(AppDetailState {
            key: key.to_string(),
            id: history::app_id(g),
            name: g.app.name.clone(),
            range_s: self.detail.as_ref().map(|d| d.range_s).unwrap_or(86_400),
            loaded: None,
            sign: inspector::inspect(snap, pid, false),
            sign_rx: Some(rx),
        });
    }

    pub(super) fn detail_panel(&mut self, ctx: &egui::Context, snap: &Arc<Snapshot>) {
        let Some(mut st) = self.detail.take() else { return };
        if let Some(rx) = &st.sign_rx
            && let Ok(d) = rx.try_recv()
        {
            if d.is_some() {
                st.sign = d;
            }
            st.sign_rx = None;
        }
        // (Re)load when first shown, when the range changed, and every 30 s.
        if st.loaded.as_ref().is_none_or(|l| l.range_s != st.range_s || l.at.elapsed() > Duration::from_secs(30)) {
            let l = self.shared.history.lock().ok().and_then(|h| h.as_ref().map(|h| load(h, &st.id, st.range_s)));
            if l.is_some() || st.loaded.is_none() {
                st.loaded = l.or(Some(Loaded { at: Instant::now(), range_s: st.range_s, from: 0, to: 1, step_s: 0, charts: vec![], peaks: vec![], events: vec![] }));
            }
        }

        let u = self.settings.units;
        let cores = snap.machine.cpu.core_count;
        let pal = self.pal(ctx);
        let group = snap.apps.iter().find(|a| a.key == st.key).cloned();
        let mut close = false;
        let mut action: Option<Mode> = None;
        egui::SidePanel::right("app_detail").default_width(460.0).show(ctx, |ui| {
            ui.add_space(6.0);
            // Header: icon, name, bundle id / version, close.
            ui.horizontal(|ui| {
                let bundle_path = group.as_ref().and_then(|g| g.app.bundle_path.clone());
                self.app_icon(ui, bundle_path.as_deref(), &st.name, 44.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add(egui::Label::new(RichText::new(&st.name).size(18.0).strong()).truncate());
                    let bundle = group.as_ref().and_then(|g| g.app.bundle_id.clone()).unwrap_or_else(|| st.id.clone());
                    let version = group.as_ref().and_then(|g| g.app.version.clone());
                    let sub = match version {
                        Some(v) => format!("{bundle} · {} {v}", self.i18n.t("version")),
                        None => bundle,
                    };
                    ui.add(egui::Label::new(RichText::new(sub).size(12.5).weak()).truncate());
                });
                ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| {
                    if widgets::icon_button(ui, widgets::ActionIcon::Quit, ui.visuals().strong_text_color(), &self.i18n.t("close")).clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(8.0);
            match &group {
                Some(g) => {
                    let can_quit = g.key != grouping::SYSTEM_KEY;
                    let ports = {
                        let mut v: Vec<u16> = g.pids.iter().flat_map(|p| snap.ports_of(*p)).collect();
                        v.sort_unstable();
                        v.dedup();
                        v
                    };
                    ui.horizontal_wrapped(|ui| {
                        let procs = if g.process_count == 1 { "1 process".to_string() } else { format!("{} processes", g.process_count) };
                        widgets::chip(ui, &procs, pal.accent);
                        if !ports.is_empty() {
                            let list = ports.iter().take(8).map(u16::to_string).collect::<Vec<_>>().join(", ");
                            widgets::chip(ui, &format!("{} {list}{}", self.i18n.t("ports"), if ports.len() > 8 { "…" } else { "" }), pal.accent);
                        }
                        match st.sign.as_ref().map(|d| &d.security) {
                            Some(s) if s.signed => {
                                let team = s.team_id.as_deref().map(|t| format!(" · {t}")).unwrap_or_default();
                                let auth = s.authority.as_deref().unwrap_or(s.identifier.as_deref().unwrap_or(""));
                                widgets::chip(ui, &format!("{}: {auth}{team}", self.i18n.t("signature")), pal.accent);
                                if s.sandboxed {
                                    widgets::chip(ui, "Sandbox", pal.accent);
                                }
                                if s.hardened_runtime {
                                    widgets::chip(ui, "Hardened runtime", pal.accent);
                                }
                            }
                            Some(_) if st.sign_rx.is_none() => widgets::chip(ui, &format!("{}: {}", self.i18n.t("signature"), self.i18n.t("unsigned")), pal.warn),
                            _ => {
                                ui.label(RichText::new(self.i18n.t("checking_sig")).size(12.5).weak());
                            }
                        }
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let now_line = METRICS.iter().map(|m| format!("{} {}", m_label(&self.i18n, *m), format_value(&u, *m, g.metric(*m), cores))).collect::<Vec<_>>().join("  ·  ");
                        ui.add(egui::Label::new(RichText::new(now_line).size(12.0).weak()).wrap());
                    });
                    if can_quit {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button(self.i18n.t("quit") + "…").clicked() {
                                action = Some(Mode::Quit);
                            }
                            if ui.add(egui::Button::new(RichText::new(self.i18n.t("force_quit") + "…").color(pal.danger))).clicked() {
                                action = Some(Mode::ForceQuit);
                            }
                        });
                    }
                }
                None => {
                    ui.label(RichText::new(self.i18n.t("not_running")).weak());
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (label, s) in RANGES {
                    ui.selectable_value(&mut st.range_s, s, label);
                }
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let Some(l) = &st.loaded else { return };
                let any = l.charts.iter().any(|(_, p)| p.len() >= 2);
                if !any {
                    ui.label(RichText::new(self.i18n.t("no_app_history")).weak());
                    ui.add_space(6.0);
                }
                for (m, pts) in &l.charts {
                    let color = self.color(
                        ui,
                        match m {
                            RankMetric::Cpu => MetricKind::Cpu,
                            RankMetric::Memory => MetricKind::Memory,
                            RankMetric::Disk => MetricKind::Disk,
                            RankMetric::Network => MetricKind::Network,
                            RankMetric::Energy => MetricKind::Battery,
                        },
                    );
                    let m = *m;
                    widgets::card(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(m_label(&self.i18n, m)).size(13.0).weak());
                            if let Some(last) = pts.last() {
                                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(RichText::new(format_value(&u, m, last.2, cores)).size(13.0).strong());
                                });
                            }
                        });
                        widgets::history_chart_range(ui, pts, (l.from, l.to), l.step_s, 90.0, color, &move |v| format_value(&u, m, v, cores));
                    });
                    ui.add_space(6.0);
                }
                // Notable events: alerts and restarts, then peaks.
                widgets::card(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(self.i18n.t("recent_events")).size(13.0).weak());
                    if l.events.is_empty() {
                        ui.label(RichText::new(self.i18n.t("no_events")).size(12.5).weak());
                    }
                    for e in &l.events {
                        let (key, color) = if e.kind == "restart" { ("event_restart", pal.accent) } else { ("event_alert", pal.warn) };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(when(e.ts)).size(12.0).weak());
                            widgets::chip(ui, &self.i18n.t(key), color);
                            let text = match (e.kind.as_str(), e.old_pid, e.new_pid) {
                                ("restart", Some(old), Some(new)) => self.i18n.t("restart_msg").replace("{old}", &old.to_string()).replace("{new}", &new.to_string()),
                                _ => e.message.clone(),
                            };
                            ui.label(RichText::new(text).size(12.5));
                        });
                    }
                    if !l.peaks.is_empty() {
                        ui.add_space(6.0);
                        ui.label(RichText::new(self.i18n.t("peaks")).size(13.0).weak());
                        for (m, p) in &l.peaks {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(when(p.ts)).size(12.0).weak());
                                ui.label(RichText::new(format!("{} {}", m_label(&self.i18n, *m), format_value(&u, *m, p.value, cores))).size(12.5));
                            });
                        }
                    }
                });
            });
        });
        if let (Some(mode), Some(g)) = (action, &group) {
            self.ask(snap, Target::App(g.key.clone()), mode);
        }
        if !close {
            self.detail = Some(st);
        }
    }
}

fn m_label(i18n: &crate::i18n::I18n, m: RankMetric) -> String {
    match m {
        RankMetric::Cpu => i18n.t("tab.cpu"),
        RankMetric::Memory => i18n.t("tab.memory"),
        RankMetric::Energy => i18n.t("energy"),
        RankMetric::Disk => i18n.t("tab.disk"),
        RankMetric::Network => i18n.t("tab.network"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{History, Sample};

    /// The panel's loader returns 24 h of CPU and memory plus peaks and events for one app.
    #[test]
    fn load_returns_series_peaks_and_events() {
        let mut h = History::open_in_memory().unwrap();
        let now = sys::now_s();
        for i in 0..(25 * 60) {
            let ts = now - 25 * 3600 + i * 60;
            h.record(ts, &[
                Sample { app: Some("com.x"), metric: "app_cpu", value: 5.0 + (i % 7) as f64 },
                Sample { app: Some("com.x"), metric: "app_memory", value: 1e9 },
            ]);
        }
        h.flush().unwrap();
        h.record_event(&AppEvent { ts: now - 600, app: "com.x".into(), kind: "restart".into(), old_pid: Some(1), new_pid: Some(2), ..Default::default() }).unwrap();
        let l = load(&h, "com.x", 86_400);
        let (_, cpu) = &l.charts[0];
        let (_, mem) = &l.charts[1];
        // Reduced from ≥ 1439 one-minute buckets to ~300 points that still span the 24 h.
        assert!(cpu.len() >= 290 && mem.len() >= 290, "{} {}", cpu.len(), mem.len());
        assert!(cpu.last().unwrap().0 - cpu[0].0 >= 86_400 - 2 * l.step_s);
        // No disk/network data was recorded for this app: empty charts, no peaks for them.
        assert!(l.charts[3].1.is_empty() && l.charts[4].1.is_empty());
        assert!(l.peaks.iter().any(|(m, p)| *m == RankMetric::Cpu && p.value == 11.0));
        assert_eq!(l.events.len(), 1);
        assert_eq!((l.events[0].old_pid, l.events[0].new_pid), (Some(1), Some(2)));
        // 24 h at 1-minute buckets is reduced to at most MAX_POINTS, keeping the extremes.
        assert!(cpu.len() as i64 <= MAX_POINTS + 1 && mem.len() as i64 <= MAX_POINTS + 1 && l.step_s > 60, "{} {}", cpu.len(), l.step_s);
        assert_eq!(cpu.iter().map(|p| p.3).fold(0.0, f64::max), 11.0);
        assert_eq!(cpu.iter().map(|p| p.1).fold(f64::MAX, f64::min), 5.0);
        assert_eq!((l.from, l.to - l.from), (l.to - 86_400, 86_400));
    }

    #[test]
    fn downsample_keeps_extremes_and_gaps() {
        let from = 1_000_000;
        let range = 7 * 86_400;
        // 5-minute buckets over 7 d, with a day without data in the middle.
        let pts: Points = (0..2016)
            .map(|i| (from + i * 300, 1.0 + (i % 10) as f64, 2.0 + (i % 10) as f64, 3.0 + (i % 10) as f64))
            .filter(|p| p.0 < from + 3 * 86_400 || p.0 >= from + 4 * 86_400)
            .collect();
        let (out, step) = downsample(&pts, from, range, 300);
        assert!(out.len() as i64 <= MAX_POINTS, "{}", out.len());
        assert!(step >= range / MAX_POINTS);
        assert_eq!(out.iter().map(|p| p.3).fold(0.0, f64::max), 12.0);
        assert_eq!(out.iter().map(|p| p.1).fold(f64::MAX, f64::min), 1.0);
        assert!(out.iter().all(|p| p.1 <= p.2 && p.2 <= p.3));
        // The empty day shows up as a jump larger than 1.5 steps between two consecutive points.
        assert!(out.windows(2).any(|w| (w[1].0 - w[0].0) as f64 > step as f64 * 1.5));
        // Short ranges are left alone.
        let (same, s) = downsample(&pts[..30], from, 3600, 60);
        assert_eq!((same.len(), s), (30, 60));
    }
}
