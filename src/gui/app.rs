//! M5 — Main window: configurable tabs, resource views with Now/Average today/Peak and Top Apps,
//! developer tabs (Projects, Agents, Containers), History, Settings, the Process Inspector and
//! every confirmation dialog (including quit requests coming from MCP).

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, RichText, Vec2};

use super::icons::Icons;
use super::widgets::{self, Palette};
use super::{Nav, show_app};
use crate::actions::{self, HumanYes, Mode, PendingRequest, Plan, Target};
use crate::daemon::Shared;
use crate::history::RankRow;
use crate::i18n::{I18n, LANGS};
use crate::inspector::{self, ProcessDetail};
use crate::model::*;
use crate::settings::{MenuDisplay, NumberFont, PlanConfig, Retention, Settings, Tab, Theme};
use crate::units::{CpuUnit, DataUnit, TempUnit, format_duration};
use crate::{docker, limits, output, paths, projects, share_card, sys};

struct InspectorState {
    detail: Option<ProcessDetail>,
    rx: Option<Receiver<Option<ProcessDetail>>>,
}

struct HistoryUi {
    metric: String,
    range_s: i64,
    rank_metric: String,
    cache: Option<(String, Instant, Vec<(i64, f64, f64, f64)>, Vec<RankRow>)>,
}

pub struct PulseApp {
    shared: Arc<Shared>,
    settings: Settings,
    i18n: I18n,
    tab: Tab,
    highlight: Option<String>,
    scroll_to_highlight: bool,
    inspector: Option<InspectorState>,
    confirm: Option<Plan>,
    pending: Vec<PendingRequest>,
    last_pending_poll: Instant,
    toasts: Vec<(Instant, String)>,
    history_ui: HistoryUi,
    today: HashMap<String, (Instant, Option<(f64, f64)>)>,
    icons: Icons,
    nav_seen: i64,
    last_nav_poll: Instant,
    expanded: HashMap<String, bool>,
}

fn local_midnight() -> i64 {
    let now = chrono::Local::now();
    now.date_naive().and_hms_opt(0, 0, 0).and_then(|d| d.and_local_timezone(chrono::Local).single()).map(|d| d.timestamp()).unwrap_or(now.timestamp() - 86_400)
}

impl PulseApp {
    pub fn new(cc: &eframe::CreationContext<'_>, shared: Arc<Shared>, tab: Tab, app_key: Option<String>) -> Self {
        let settings = shared.settings.read().unwrap().clone();
        let app = Self {
            i18n: I18n::new(settings.language.as_deref()),
            tab,
            scroll_to_highlight: app_key.is_some(),
            highlight: app_key,
            inspector: None,
            confirm: None,
            pending: vec![],
            last_pending_poll: Instant::now() - Duration::from_secs(10),
            toasts: vec![],
            history_ui: HistoryUi { metric: "cpu".into(), range_s: 86_400, rank_metric: "app_memory".into(), cache: None },
            today: HashMap::new(),
            icons: Icons::new(cc.egui_ctx.clone()),
            nav_seen: std::fs::read(paths::nav_file()).ok().and_then(|b| serde_json::from_slice::<Nav>(&b).ok()).map(|n| n.at_ms).unwrap_or(0),
            last_nav_poll: Instant::now(),
            expanded: HashMap::new(),
            shared,
            settings,
        };
        app.apply_style(&cc.egui_ctx);
        app
    }

    fn apply_style(&self, ctx: &egui::Context) {
        ctx.style_mut(|s| {
            if self.settings.compact {
                s.spacing.item_spacing = Vec2::new(6.0, 3.0);
                s.spacing.button_padding = Vec2::new(4.0, 1.0);
            } else {
                s.spacing.item_spacing = Vec2::new(8.0, 6.0);
                s.spacing.button_padding = Vec2::new(8.0, 3.0);
            }
        });
    }

    fn save_settings(&mut self, ctx: &egui::Context) {
        let _ = self.settings.save();
        *self.shared.settings.write().unwrap() = self.settings.clone();
        self.i18n = I18n::new(self.settings.language.as_deref());
        self.apply_style(ctx);
    }

    fn pal(&self, ctx: &egui::Context) -> Palette {
        widgets::palette(self.settings.theme, ctx.style().visuals.dark_mode)
    }

    fn toast(&mut self, s: impl Into<String>) {
        self.toasts.push((Instant::now(), s.into()));
    }

    fn ask(&mut self, snap: &Snapshot, target: Target, mode: Mode) {
        match actions::plan(snap, target, mode) {
            Ok(p) => self.confirm = Some(p),
            Err(e) => self.toast(e.to_string()),
        }
    }

    fn today_stats(&mut self, metric: &str) -> Option<(f64, f64)> {
        if let Some((t, v)) = self.today.get(metric)
            && t.elapsed() < Duration::from_secs(15) {
                return *v;
            }
        let v = self.shared.history.lock().ok().and_then(|h| h.as_ref().and_then(|h| h.stats_since(metric, local_midnight(), sys::now_s()).ok().flatten()));
        self.today.insert(metric.to_string(), (Instant::now(), v));
        v
    }

    fn open_inspector(&mut self, snap: &Arc<Snapshot>, pid: i32) {
        let (tx, rx) = channel();
        let s = snap.clone();
        std::thread::spawn(move || {
            let _ = tx.send(inspector::inspect(&s, pid, true));
        });
        self.inspector = Some(InspectorState { detail: inspector::inspect(snap, pid, false), rx: Some(rx) });
    }

    /// Navigation requests from the menu bar agent (nav.json).
    fn handle_nav(&mut self) {
        if self.last_nav_poll.elapsed() < Duration::from_millis(300) {
            return;
        }
        self.last_nav_poll = Instant::now();
        let Some(nav) = std::fs::read(paths::nav_file()).ok().and_then(|b| serde_json::from_slice::<Nav>(&b).ok()) else { return };
        if nav.at_ms > self.nav_seen {
            self.nav_seen = nav.at_ms;
            self.tab = nav.tab;
            self.scroll_to_highlight = nav.app.is_some();
            self.highlight = nav.app;
            show_app();
        }
    }

    // ------------------------------------------------------------------------------------------
    // Shared pieces
    // ------------------------------------------------------------------------------------------

    fn header(&mut self, ui: &mut egui::Ui, title: &str, now: String, metric: &str, fmt: &dyn Fn(f64) -> String, breakdown: &[(String, String)]) {
        let stats = self.today_stats(metric);
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(format!("{title} · {}", self.i18n.t("now"))).weak());
                    ui.label(widgets::num(&self.settings, now, 40.0).strong());
                });
                ui.add_space(30.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(self.i18n.t("avg_today")).weak());
                    ui.label(widgets::num(&self.settings, stats.map(|s| fmt(s.0)).unwrap_or("—".into()), 22.0));
                });
                ui.add_space(20.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(self.i18n.t("peak")).weak());
                    ui.label(widgets::num(&self.settings, stats.map(|s| fmt(s.1)).unwrap_or("—".into()), 22.0));
                });
            });
            if !breakdown.is_empty() {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for (k, v) in breakdown {
                        ui.label(RichText::new(k).weak());
                        ui.label(widgets::num(&self.settings, v.clone(), 14.0));
                        ui.add_space(10.0);
                    }
                });
            }
        });
    }

    fn export_buttons(&mut self, ui: &mut egui::Ui, snap: &Snapshot, m: RankMetric) {
        ui.horizontal(|ui| {
            for dark in [false, true] {
                if ui.button(if dark { "Export card (dark)" } else { "Export card (light)" }).clicked() {
                    let card = crate::cli::card_for(snap, m, &self.settings, dark);
                    match share_card::export(&card, &paths::exports_dir(), &format!("pulsebar-{}", m.as_str())) {
                        Ok(p) => self.toast(format!("Saved {}", p.display())),
                        Err(e) => self.toast(format!("Export failed: {e}")),
                    }
                }
            }
        });
    }

    fn top_apps(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>, m: RankMetric, n: usize) {
        let u = self.settings.units;
        let cores = snap.machine.cpu.core_count;
        let pal = self.pal(ui.ctx());
        let mut action: Option<(Target, Mode)> = None;
        let mut inspect: Option<i32> = None;
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(self.i18n.t("top_apps"));
                ui.label(RichText::new(format!("{} {} → {} apps", snap.processes.len(), self.i18n.t("processes"), snap.apps.len())).weak());
            });
            let top = snap.top_apps(m, n);
            let max = top.first().map(|a| a.metric(m)).unwrap_or(1.0).max(1e-9);
            for a in top {
                let highlighted = self.highlight.as_deref() == Some(a.key.as_str());
                let open = *self.expanded.get(&a.key).unwrap_or(&false);
                let row = ui.horizontal(|ui| {
                    if let Some(t) = self.icons.get(ui.ctx(), a.app.bundle_path.as_deref()) {
                        ui.image((t, Vec2::splat(18.0)));
                    } else {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 4.0, pal.accent.gamma_multiply(0.4));
                        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, a.app.name.chars().next().unwrap_or('?'), egui::FontId::proportional(11.0), Color32::WHITE);
                    }
                    let name = RichText::new(&a.app.name);
                    let name = if highlighted { name.strong().color(pal.accent) } else { name };
                    if ui.add(egui::Button::new(name).frame(false)).on_hover_text("Show processes").clicked() {
                        self.expanded.insert(a.key.clone(), !open);
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if a.key != crate::grouping::SYSTEM_KEY {
                            if ui.small_button(self.i18n.t("force_quit") + "…").clicked() {
                                action = Some((Target::App(a.key.clone()), Mode::ForceQuit));
                            }
                            if ui.small_button(self.i18n.t("quit") + "…").clicked() {
                                action = Some((Target::App(a.key.clone()), Mode::Quit));
                            }
                        }
                        ui.label(RichText::new(format!("{} proc.", a.process_count)).weak());
                        ui.label(widgets::num(&self.settings, output::metric_value(a, m, &u, cores), 14.0).strong());
                        widgets::meter(ui, a.metric(m) / max, 90.0, pal.accent);
                    });
                });
                if highlighted && self.scroll_to_highlight {
                    row.response.scroll_to_me(Some(Align::Center));
                    self.scroll_to_highlight = false;
                    self.expanded.insert(a.key.clone(), true);
                }
                if open || (highlighted && self.expanded.get(&a.key).copied().unwrap_or(false)) {
                    let mut procs: Vec<&Process> = a.pids.iter().filter_map(|p| snap.process(*p)).collect();
                    procs.sort_by(|x, y| match m {
                        RankMetric::Cpu => y.cpu_pct.total_cmp(&x.cpu_pct),
                        RankMetric::Energy => y.energy_w.total_cmp(&x.energy_w),
                        _ => y.mem_bytes.cmp(&x.mem_bytes),
                    });
                    ui.indent(a.key.clone(), |ui| {
                        egui::Grid::new(format!("helpers-{}", a.key)).striped(true).num_columns(6).show(ui, |ui| {
                            for p in procs.iter().take(200) {
                                ui.label(&p.name);
                                ui.label(RichText::new(p.pid.to_string()).weak());
                                ui.label(u.cpu(p.cpu_pct, cores));
                                ui.label(u.bytes(p.mem_bytes));
                                if ui.small_button("Inspect").clicked() {
                                    inspect = Some(p.pid);
                                }
                                if ui.small_button(self.i18n.t("quit") + "…").clicked() {
                                    action = Some((Target::Pid(p.pid), Mode::Quit));
                                }
                                ui.end_row();
                            }
                        });
                    });
                }
            }
        });
        if let Some((t, md)) = action {
            self.ask(snap, t, md);
        }
        if let Some(pid) = inspect {
            self.open_inspector(snap, pid);
        }
    }

    // ------------------------------------------------------------------------------------------
    // Tabs
    // ------------------------------------------------------------------------------------------

    fn overview(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let u = self.settings.units;
        let m = &snap.machine;
        let sparks = self.shared.sparks.read().unwrap().clone();
        let pal = self.pal(ui.ctx());
        let limit = snap.plans.iter().filter(|p| p.pace_state != PaceState::Unknown).map(|p| 100.0 - p.used_pct).fold(f64::NAN, f64::min);
        let tiles: Vec<(Tab, &str, String, String, MetricKind)> = vec![
            (Tab::Cpu, "CPU", u.cpu(m.cpu.total_pct, m.cpu.core_count), format!("User {:.0} / System {:.0} · Load {:.2}", m.cpu.user_pct, m.cpu.system_pct, m.cpu.load_avg[0]), MetricKind::Cpu),
            (Tab::Memory, "Memory", u.bytes(m.memory.used), format!("of {} · swap {}", u.bytes(m.memory.total), u.bytes(m.memory.swap_used)), MetricKind::Memory),
            (Tab::Disk, "Disk free", m.disk.boot_free().map(|v| u.bytes(v.free)).unwrap_or("—".into()), format!("R {} · W {}", u.rate(m.disk.read_bps), u.rate(m.disk.write_bps)), MetricKind::Disk),
            (Tab::Network, "Network", u.rate(m.network.rx_bps + m.network.tx_bps), format!("↓ {} ↑ {}", u.rate(m.network.rx_bps), u.rate(m.network.tx_bps)), MetricKind::Network),
            (Tab::Gpu, "GPU", format!("{:.0} %", m.gpu.utilization_pct), m.gpu.name.clone(), MetricKind::Gpu),
            (Tab::Neural, "Neural Engine", format!("{:.2} W", m.neural.power_w), if m.neural.available { format!("~{:.0} % load", m.neural.utilization_pct) } else { "not available".into() }, MetricKind::Neural),
            (Tab::Battery, "Battery", if m.battery.present { format!("{:.0} %", m.battery.percent) } else { "AC".into() }, m.power.system_w.map(|w| format!("{w:.1} W system")).unwrap_or_default(), MetricKind::Battery),
            (Tab::Agents, "Agents limit", if limit.is_nan() { "—".into() } else { format!("{limit:.0} % left") }, format!("{} sessions", snap.agents.len()), MetricKind::Cpu),
            (Tab::Sound, "Sound", m.audio.output_volume.map(|v| format!("{:.0} %", v * 100.0)).unwrap_or("—".into()), m.audio.output_device.clone().unwrap_or_default(), MetricKind::Sound),
        ];
        let w = ((ui.available_width() - 24.0) / 3.0).max(200.0);
        egui::Grid::new("tiles").num_columns(3).spacing([12.0, 12.0]).show(ui, |ui| {
            for (i, (tab, label, value, sub, kind)) in tiles.into_iter().enumerate() {
                let resp = ui
                    .allocate_ui(Vec2::new(w, 110.0), |ui| {
                        widgets::card(ui, |ui| {
                            ui.set_width(w - 26.0);
                            ui.label(RichText::new(label).weak());
                            ui.label(widgets::num(&self.settings, value, 26.0).strong());
                            ui.label(RichText::new(sub).small().weak());
                            let fixed = matches!(kind, MetricKind::Cpu | MetricKind::Gpu).then_some(100.0);
                            widgets::sparkline(ui, &sparks.get(kind), Vec2::new(w - 30.0, 22.0), pal.accent, fixed);
                        })
                    })
                    .response
                    .interact(egui::Sense::click());
                if resp.clicked() {
                    self.tab = tab;
                }
                if i % 3 == 2 {
                    ui.end_row();
                }
            }
        });
        ui.add_space(8.0);
        widgets::card(ui, |ui| {
            ui.heading(self.i18n.t("busiest"));
            ui.horizontal(|ui| {
                for a in snap.top_apps(RankMetric::Cpu, 3) {
                    ui.label(RichText::new(&a.app.name).strong());
                    ui.label(widgets::num(&self.settings, u.cpu(a.cpu_pct, m.cpu.core_count), 14.0));
                    ui.label(RichText::new(format!("· {}", u.bytes(a.mem_bytes))).weak());
                    ui.add_space(16.0);
                }
            });
        });
        if let Some(b) = projects::idle_banner(&snap.projects) {
            ui.add_space(6.0);
            if ui.add(egui::Button::new(RichText::new(format!("⚠ {b}")).color(pal.warn)).frame(false)).clicked() {
                self.tab = Tab::Projects;
            }
        }
    }

    fn cpu(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let c = snap.machine.cpu.clone();
        let u = self.settings.units;
        let cores = c.core_count;
        let fmt = move |v: f64| u.cpu(v, cores);
        self.header(ui, "CPU", u.cpu(c.total_pct, cores), "cpu", &fmt, &[
            ("User".into(), format!("{:.0} %", c.user_pct)),
            ("System".into(), format!("{:.0} %", c.system_pct)),
            ("Idle".into(), format!("{:.0} %", c.idle_pct)),
            ("Load".into(), format!("{:.2} {:.2} {:.2}", c.load_avg[0], c.load_avg[1], c.load_avg[2])),
        ]);
        let pal = self.pal(ui.ctx());
        widgets::card(ui, |ui| {
            ui.label(RichText::new("Per core").weak());
            ui.horizontal_wrapped(|ui| {
                for (i, v) in c.per_core.iter().enumerate() {
                    ui.vertical(|ui| {
                        widgets::meter(ui, v / 100.0, 60.0, if *v > 90.0 { pal.danger } else { pal.accent });
                        ui.label(RichText::new(format!("{} · {:.0}%", i + 1, v)).small().weak());
                    });
                }
            });
            let t = &snap.machine.sensors;
            if t.cpu_temp_c.is_some() || !t.fans.is_empty() {
                ui.horizontal(|ui| {
                    if let Some(x) = t.cpu_temp_c {
                        ui.label(format!("CPU {}", u.temp(x)));
                    }
                    if let Some(x) = t.gpu_temp_c {
                        ui.label(format!("GPU {}", u.temp(x)));
                    }
                    for f in &t.fans {
                        ui.label(format!("Fan {} {:.0} rpm", f.index + 1, f.rpm));
                    }
                });
            }
        });
        self.export_buttons(ui, snap, RankMetric::Cpu);
        self.top_apps(ui, snap, RankMetric::Cpu, 25);
    }

    fn memory(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let m = snap.machine.memory.clone();
        let u = self.settings.units;
        let fmt = move |v: f64| u.bytes(v as u64);
        self.header(ui, "Memory", u.bytes(m.used), "memory", &fmt, &[
            ("App".into(), u.bytes(m.app)),
            ("Wired".into(), u.bytes(m.wired)),
            ("Compressed".into(), u.bytes(m.compressed)),
            ("Cached".into(), u.bytes(m.cached)),
            ("Free".into(), u.bytes(m.free)),
            ("Swap".into(), u.bytes(m.swap_used)),
        ]);
        let p = self.pal(ui.ctx());
        widgets::stacked_bar(ui, &[("App".into(), m.app as f64, p.series[0]), ("Wired".into(), m.wired as f64, p.series[1]), ("Compressed".into(), m.compressed as f64, p.series[2]), ("Cached".into(), m.cached as f64, p.series[3])], m.total as f64);
        self.export_buttons(ui, snap, RankMetric::Memory);
        self.top_apps(ui, snap, RankMetric::Memory, 60);
    }

    fn disk(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let d = snap.machine.disk.clone();
        let u = self.settings.units;
        let fmt = move |v: f64| u.rate(v);
        self.header(ui, "Disk", u.rate(d.read_bps + d.write_bps), "disk", &fmt, &[("Read".into(), u.rate(d.read_bps)), ("Write".into(), u.rate(d.write_bps))]);
        let pal = self.pal(ui.ctx());
        widgets::card(ui, |ui| {
            for v in &d.volumes {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&v.name).strong());
                    ui.label(RichText::new(&v.mount_point).weak());
                    widgets::meter(ui, 1.0 - v.free as f64 / v.total.max(1) as f64, 200.0, pal.accent);
                    ui.label(format!("{} free of {}", u.bytes(v.free), u.bytes(v.total)));
                });
            }
        });
        self.export_buttons(ui, snap, RankMetric::Disk);
        self.top_apps(ui, snap, RankMetric::Disk, 25);
    }

    fn network(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let n = snap.machine.network.clone();
        let u = self.settings.units;
        let fmt = move |v: f64| u.rate(v);
        self.header(ui, "Network", u.rate(n.rx_bps + n.tx_bps), "network", &fmt, &[("Down".into(), u.rate(n.rx_bps)), ("Up".into(), u.rate(n.tx_bps))]);
        widgets::card(ui, |ui| {
            egui::Grid::new("ifs").striped(true).num_columns(5).show(ui, |ui| {
                for h in ["Interface", "Down", "Up", "Total down", "Total up"] {
                    ui.label(RichText::new(h).weak());
                }
                ui.end_row();
                for i in &n.interfaces {
                    ui.label(&i.name);
                    ui.label(u.rate(i.rx_bps));
                    ui.label(u.rate(i.tx_bps));
                    ui.label(u.transferred(i.total_rx));
                    ui.label(u.transferred(i.total_tx));
                    ui.end_row();
                }
            });
            ui.label(RichText::new("Per-app network is sampled every 15 s.").small().weak());
        });
        self.export_buttons(ui, snap, RankMetric::Network);
        self.top_apps(ui, snap, RankMetric::Network, 25);
    }

    fn gpu(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let g = snap.machine.gpu.clone();
        let u = self.settings.units;
        let fmt = |v: f64| format!("{v:.0} %");
        self.header(ui, &format!("GPU · {}", g.name), format!("{:.0} %", g.utilization_pct), "gpu", &fmt, &[
            ("Memory in use".into(), u.bytes(g.vram_used)),
            ("Of".into(), u.bytes(g.vram_total)),
            ("Power".into(), g.power_w.map(|w| format!("{w:.1} W")).unwrap_or("—".into())),
        ]);
        if !g.available {
            ui.label("No GPU statistics available.");
        }
        ui.label(RichText::new("macOS does not expose GPU time per app; apps are ranked by energy.").weak());
        self.top_apps(ui, snap, RankMetric::Energy, 15);
    }

    fn neural(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let n = snap.machine.neural.clone();
        let fmt = |v: f64| format!("{v:.0} %");
        self.header(ui, "Neural Engine", format!("{:.2} W", n.power_w), "neural", &fmt, &[("Estimated load".into(), format!("{:.0} %", n.utilization_pct))]);
        ui.label(RichText::new(if n.available {
            "ANE power comes from IOReport's Energy Model; load is estimated against an 8 W reference."
        } else {
            "The Neural Engine is not reported on this Mac (Intel, or IOReport unavailable)."
        }).weak());
        let p = &snap.machine.power;
        widgets::card(ui, |ui| {
            ui.label(format!(
                "CPU {} · GPU {} · ANE {}",
                p.cpu_w.map(|w| format!("{w:.1} W")).unwrap_or("—".into()),
                p.gpu_w.map(|w| format!("{w:.1} W")).unwrap_or("—".into()),
                p.ane_w.map(|w| format!("{w:.2} W")).unwrap_or("—".into())
            ));
        });
    }

    fn battery(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let b = snap.machine.battery.clone();
        let u = self.settings.units;
        let fmt = |v: f64| format!("{v:.0} %");
        self.header(ui, "Battery", if b.present { format!("{:.0} %", b.percent) } else { "No battery".into() }, "battery", &fmt, &[
            ("State".into(), if b.charging { "charging".into() } else if b.on_ac { "on AC".into() } else { "on battery".into() }),
            ("Remaining".into(), b.time_remaining_min.map(|m| format_duration(m * 60)).unwrap_or("—".into())),
            ("Battery power".into(), b.power_w.map(|w| format!("{w:.1} W")).unwrap_or("—".into())),
            ("System power".into(), snap.machine.power.system_w.map(|w| format!("{w:.1} W")).unwrap_or("—".into())),
            ("Health".into(), b.health_pct.map(|h| format!("{h:.0} %")).unwrap_or("—".into())),
            ("Cycles".into(), b.cycle_count.map(|c| c.to_string()).unwrap_or("—".into())),
            ("Temperature".into(), b.temperature_c.map(|t| u.temp(t)).unwrap_or("—".into())),
        ]);
        self.export_buttons(ui, snap, RankMetric::Energy);
        self.top_apps(ui, snap, RankMetric::Energy, 25);
    }

    fn sound(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let a = snap.machine.audio.clone();
        widgets::card(ui, |ui| {
            ui.heading(a.output_device.clone().unwrap_or("No output device".into()));
            if let Some(mut v) = a.output_volume {
                ui.horizontal(|ui| {
                    ui.label("Output");
                    if ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false)).changed() {
                        crate::collectors::devices::set_volume(false, v);
                    }
                    let mut muted = a.output_muted;
                    if ui.checkbox(&mut muted, "Muted").changed() {
                        crate::collectors::devices::set_muted(muted);
                    }
                });
            }
            if let Some(mut v) = a.input_volume {
                ui.horizontal(|ui| {
                    ui.label("Microphone");
                    if ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false)).changed() {
                        crate::collectors::devices::set_volume(true, v);
                    }
                });
            }
        });
        widgets::card(ui, |ui| {
            ui.heading("Apps using audio");
            if a.apps.is_empty() {
                ui.label(RichText::new("No app is using audio right now.").weak());
            }
            for app in &a.apps {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&app.app_name).strong());
                    let s = match app.state {
                        AudioState::Playing => "Playing",
                        AudioState::Silent => "Silent",
                        AudioState::Muted => "Muted",
                    };
                    ui.label(s);
                    if app.using_mic {
                        ui.label("🎙 mic");
                    }
                });
            }
            ui.label(RichText::new("Per-app volume needs a CoreAudio process tap (macOS 14.4+); this build controls device volume.").small().weak());
        });
    }

    fn bluetooth(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let b = snap.machine.bluetooth.clone();
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(if b.powered { "Bluetooth on" } else { "Bluetooth off" });
                if ui.button("Bluetooth Settings…").clicked() {
                    let _ = std::process::Command::new("/usr/bin/open").arg("x-apple.systempreferences:com.apple.BluetoothSettings").spawn();
                }
            });
            let pct = |v: Option<f64>| v.map(|x| format!("{x:.0} %")).unwrap_or("—".into());
            for d in &b.devices {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&d.name).strong());
                    ui.label(RichText::new(if d.connected { "connected" } else { "paired but away" }).weak());
                    if d.battery_left.is_some() || d.battery_right.is_some() {
                        ui.label(format!("L {} · R {} · Case {}", pct(d.battery_left), pct(d.battery_right), pct(d.battery_case)));
                    } else if d.battery_main.is_some() {
                        ui.label(pct(d.battery_main));
                    }
                });
            }
            if b.devices.is_empty() {
                ui.label(RichText::new("No devices yet (refreshed every 60 s).").weak());
            }
        });
    }

    fn projects_tab(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let u = self.settings.units;
        let pal = self.pal(ui.ctx());
        let mut ask: Vec<Target> = vec![];
        if let Some(b) = projects::idle_banner(&snap.projects) {
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&b).color(pal.warn));
                    if ui.button(self.i18n.t("stop_all")).clicked() {
                        ask = projects::idle_servers(&snap.projects).iter().map(|s| Target::DevServer(s.process_pid)).collect();
                    }
                });
            });
        }
        if snap.projects.is_empty() {
            ui.label(RichText::new("No processes are running inside a project folder under your home directory.").weak());
        }
        for p in &snap.projects {
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(&p.project.name);
                    ui.label(RichText::new(&p.project.folder_path).weak());
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(&p.state);
                        ui.label(u.bytes(p.mem_bytes));
                        ui.label(format!("{} proc.", p.process_count));
                    });
                });
                for s in &p.servers {
                    ui.horizontal(|ui| {
                        let txt = RichText::new(format!(":{}  {}", s.port, s.command));
                        ui.label(if s.idle { txt.color(pal.warn) } else { txt });
                        ui.label(RichText::new(format!("{} · idle {}", u.bytes(s.mem_bytes), format_duration(s.idle_secs))).weak());
                        if ui.small_button(self.i18n.t("stop")).clicked() {
                            ask.push(Target::DevServer(s.process_pid));
                        }
                    });
                }
            });
        }
        // Several targets → combine into one confirmation.
        if ask.len() == 1 {
            self.ask(snap, ask.remove(0), Mode::Quit);
        } else if ask.len() > 1 {
            let plans: Vec<Plan> = ask.into_iter().filter_map(|t| actions::plan(snap, t, Mode::Quit).ok()).collect();
            let mut pids: Vec<i32> = plans.iter().flat_map(|p| p.pids.clone()).collect();
            pids.sort_unstable();
            pids.dedup();
            let mut ports: Vec<u16> = plans.iter().flat_map(|p| p.ports.clone()).collect();
            ports.sort_unstable();
            ports.dedup();
            let n = plans.len();
            if let Some(first) = plans.into_iter().next() {
                let mem = pids.iter().filter_map(|p| snap.process(*p)).map(|p| p.mem_bytes).sum();
                self.confirm = Some(Plan { title: format!("{n} idle dev servers"), pids, ports, mem_bytes: mem, ..first });
            }
        }
    }

    fn agents_tab(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let u = self.settings.units;
        let now = snap.timestamp / 1000;
        let pal = self.pal(ui.ctx());
        widgets::card(ui, |ui| {
            ui.heading("AI plans");
            if snap.plans.is_empty() {
                ui.label(RichText::new("Configure plans in Settings.").weak());
            }
            for p in &snap.plans {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(limits::headline(p, now)).strong());
                    let c = match p.pace_state {
                        PaceState::RunsOut => pal.danger,
                        PaceState::OnPace | PaceState::Ahead => pal.ok,
                        PaceState::Unknown => ui.visuals().weak_text_color(),
                    };
                    ui.label(RichText::new(&p.pace_text).color(c));
                });
                widgets::meter(ui, p.used_pct / 100.0, 300.0, pal.accent);
            }
        });
        let mut quit: Option<i32> = None;
        if snap.agents.is_empty() {
            ui.label(RichText::new("No AI agent sessions running.").weak());
        }
        for a in &snap.agents {
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(a.agent_kind.label());
                    ui.label(RichText::new(format!("{}{}", a.project, a.branch_or_task.as_ref().map(|b| format!(" · {b}")).unwrap_or_default())).weak());
                    let st = match a.state {
                        AgentState::Working => ("working", pal.ok),
                        AgentState::Waiting => ("waiting", pal.warn),
                        AgentState::Idle => ("idle", ui.visuals().weak_text_color()),
                    };
                    ui.label(RichText::new(st.0).color(st.1));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Quit Session…").clicked() {
                            quit = Some(a.pid);
                        }
                        ui.label(u.cpu(a.cpu_percent, snap.machine.cpu.core_count));
                        ui.label(u.bytes(a.mem_bytes));
                        ui.label(RichText::new(format_duration(now - a.started_at)).weak());
                    });
                });
                if !a.started.is_empty() {
                    ui.label(RichText::new("Started").weak());
                    for s in &a.started {
                        ui.label(format!("  {} — {} (pid {}) · {}", s.role, s.name, s.pid, u.bytes(s.mem_bytes)));
                    }
                }
            });
        }
        if let Some(pid) = quit {
            self.ask(snap, Target::AgentSession(pid), Mode::Quit);
        }
    }

    fn containers_tab(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let u = self.settings.units;
        let pal = self.pal(ui.ctx());
        if !snap.docker_available {
            ui.label(RichText::new("Docker is not running (no local socket found).").weak());
            return;
        }
        for c in docker::near_limit(&snap.containers) {
            ui.label(RichText::new(format!("⚠ {}", docker::card_line(c))).color(pal.danger));
        }
        let mut stop: Option<String> = None;
        for (stack, cs) in docker::stacks(&snap.containers) {
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(&stack.name);
                    if let Some(h) = &stack.project_hint {
                        ui.label(RichText::new(h).weak());
                    }
                });
                egui::Grid::new(format!("stack-{}", stack.name)).striped(true).num_columns(8).show(ui, |ui| {
                    for h in ["", "Container", "Image", "Memory", "% of limit", "CPU", "Ports", ""] {
                        ui.label(RichText::new(h).weak());
                    }
                    ui.end_row();
                    for c in cs {
                        ui.label(if c.up() { RichText::new("●").color(pal.ok) } else { RichText::new("○").weak() });
                        ui.label(&c.name).on_hover_text(&c.status);
                        ui.label(RichText::new(&c.image).weak());
                        ui.label(u.bytes(c.mem_bytes));
                        ui.label(c.mem_pct_of_limit().map(|p| format!("{p:.0} % of {}", u.bytes(c.mem_limit))).unwrap_or("no limit".into()));
                        ui.label(format!("{:.1} %", c.cpu_pct));
                        ui.label(c.published_ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", "));
                        if c.up() && ui.small_button(self.i18n.t("stop")).clicked() {
                            stop = Some(c.id.clone());
                        }
                        ui.end_row();
                    }
                });
            });
        }
        if let Some(id) = stop {
            self.ask(snap, Target::Container(id), Mode::Quit);
        }
    }

    fn history_tab(&mut self, ui: &mut egui::Ui) {
        let u = self.settings.units;
        let pal = self.pal(ui.ctx());
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("hmetric").selected_text(&self.history_ui.metric).show_ui(ui, |ui| {
                for k in MetricKind::ALL {
                    ui.selectable_value(&mut self.history_ui.metric, k.as_str().to_string(), k.as_str());
                }
            });
            for (label, s) in [("12 h", 12 * 3600), ("24 h", 86_400), ("7 d", 7 * 86_400), ("30 d", 30 * 86_400), ("12 m", 365 * 86_400)] {
                ui.selectable_value(&mut self.history_ui.range_s, s, label);
            }
        });
        let key = format!("{}:{}:{}", self.history_ui.metric, self.history_ui.range_s, self.history_ui.rank_metric);
        let stale = self.history_ui.cache.as_ref().is_none_or(|(k, t, _, _)| *k != key || t.elapsed() > Duration::from_secs(30));
        if stale {
            let now = sys::now_s();
            let from = now - self.history_ui.range_s;
            let (series, rank) = self
                .shared
                .history
                .lock()
                .ok()
                .and_then(|h| {
                    h.as_ref().map(|h| {
                        (h.series(&self.history_ui.metric, None, from, now, now).unwrap_or_default(), h.ranking(&self.history_ui.rank_metric, from, now, now, 10).unwrap_or_default())
                    })
                })
                .unwrap_or_default();
            let pts = series.iter().map(|b| (b.time_bucket, b.value_min, b.value_avg, b.value_max)).collect();
            self.history_ui.cache = Some((key, Instant::now(), pts, rank));
        }
        let metric = self.history_ui.metric.clone();
        let fmt = move |v: f64| match metric.as_str() {
            "memory" => u.bytes(v as u64),
            "disk" | "network" => u.rate(v),
            "temperature" => u.temp(v),
            "power" | "neural" => format!("{v:.1} W"),
            _ => format!("{v:.0}"),
        };
        if let Some((_, _, pts, rank)) = &self.history_ui.cache {
            widgets::card(ui, |ui| widgets::history_chart(ui, pts, 220.0, pal.accent, &fmt));
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Ranking for the period");
                    ui.selectable_value(&mut self.history_ui.rank_metric, "app_memory".to_string(), "Memory");
                    ui.selectable_value(&mut self.history_ui.rank_metric, "app_energy".to_string(), "Energy");
                    ui.selectable_value(&mut self.history_ui.rank_metric, "app_cpu".to_string(), "CPU");
                });
                let rm = self.history_ui.rank_metric.clone();
                for (i, r) in rank.iter().enumerate() {
                    let v = match rm.as_str() {
                        "app_memory" => format!("avg {} · peak {}", u.bytes(r.avg as u64), u.bytes(r.peak as u64)),
                        "app_energy" => format!("{:.1} Wh · avg {:.2} W", r.integral / 3600.0, r.avg),
                        _ => format!("avg {:.1} % · peak {:.0} %", r.avg, r.peak),
                    };
                    ui.label(format!("{}. {}  —  {v}", i + 1, r.app));
                }
                if rank.is_empty() {
                    ui.label(RichText::new("No data for this period yet.").weak());
                }
            });
        }
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut changed = false;
        let s = &mut self.settings;
        widgets::card(ui, |ui| {
            ui.heading("Units");
            ui.horizontal(|ui| {
                changed |= ui.radio_value(&mut s.units.temperature, TempUnit::Celsius, "°C").changed();
                changed |= ui.radio_value(&mut s.units.temperature, TempUnit::Fahrenheit, "°F").changed();
                ui.separator();
                changed |= ui.radio_value(&mut s.units.data, DataUnit::Bytes, "Bytes").changed();
                changed |= ui.radio_value(&mut s.units.data, DataUnit::Bits, "Bits").changed();
                ui.separator();
                changed |= ui.radio_value(&mut s.units.cpu, CpuUnit::PercentOfMac, "% of Mac").changed();
                changed |= ui.radio_value(&mut s.units.cpu, CpuUnit::PerCore, "Per core").changed();
            });
        });
        widgets::card(ui, |ui| {
            ui.heading("Appearance");
            ui.horizontal(|ui| {
                changed |= ui.radio_value(&mut s.theme, Theme::Monochrome, "Monochrome").changed();
                changed |= ui.radio_value(&mut s.theme, Theme::Brand, "Brand colour").changed();
                changed |= ui.radio_value(&mut s.theme, Theme::ColorBlind, "Colour-blind safe").changed();
            });
            ui.horizontal(|ui| {
                changed |= ui.radio_value(&mut s.number_font, NumberFont::Proportional, "Proportional numbers").changed();
                changed |= ui.radio_value(&mut s.number_font, NumberFont::Monospace, "Monospaced numbers").changed();
                changed |= ui.checkbox(&mut s.compact, "Compact density").changed();
            });
            ui.horizontal(|ui| {
                ui.label("Language");
                let cur = s.language.clone().unwrap_or_else(|| "System".into());
                egui::ComboBox::from_id_salt("lang").selected_text(cur).show_ui(ui, |ui| {
                    changed |= ui.selectable_value(&mut s.language, None, "System").changed();
                    for l in LANGS {
                        changed |= ui.selectable_value(&mut s.language, Some(l.to_string()), l).changed();
                    }
                });
            });
        });
        widgets::card(ui, |ui| {
            ui.heading("Menu bar");
            let mut mv: Option<(usize, usize)> = None;
            let n = s.menubar.len();
            for (i, it) in s.menubar.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut it.visible, it.metric.label()).changed();
                    egui::ComboBox::from_id_salt(format!("disp{i}"))
                        .selected_text(match it.display {
                            MenuDisplay::Number => "Number",
                            MenuDisplay::Sparkline => "Graph",
                            MenuDisplay::Both => "Both",
                        })
                        .show_ui(ui, |ui| {
                            changed |= ui.selectable_value(&mut it.display, MenuDisplay::Number, "Number").changed();
                            changed |= ui.selectable_value(&mut it.display, MenuDisplay::Sparkline, "Graph").changed();
                            changed |= ui.selectable_value(&mut it.display, MenuDisplay::Both, "Both").changed();
                        });
                    changed |= ui.checkbox(&mut it.framed, "Framed").changed();
                    let mut has = it.threshold.is_some();
                    if ui.checkbox(&mut has, "Red above").changed() {
                        it.threshold = if has { Some(it.metric.default_threshold().unwrap_or(80.0)) } else { None };
                        changed = true;
                    }
                    if let Some(t) = it.threshold.as_mut() {
                        changed |= ui.add(egui::DragValue::new(t).speed(1.0)).changed();
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("↑")).clicked() {
                        mv = Some((i, i - 1));
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new("↓")).clicked() {
                        mv = Some((i, i + 1));
                    }
                });
            }
            if let Some((a, b)) = mv {
                s.move_menubar_item(a, b);
                changed = true;
            }
        });
        widgets::card(ui, |ui| {
            ui.heading("Tabs");
            let mut mv: Option<(usize, usize)> = None;
            let n = s.tabs.len();
            let i18n = I18n::new(s.language.as_deref());
            for (i, t) in s.tabs.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let enabled = t.tab != Tab::Settings;
                    changed |= ui.add_enabled(enabled, egui::Checkbox::new(&mut t.visible, i18n.t(t.tab.key()))).changed();
                    if ui.add_enabled(i > 0, egui::Button::new("↑")).clicked() {
                        mv = Some((i, i - 1));
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new("↓")).clicked() {
                        mv = Some((i, i + 1));
                    }
                });
            }
            if let Some((a, b)) = mv {
                s.move_tab(a, b);
                changed = true;
            }
            if ui.button(i18n.t("reset_layout")).clicked() {
                s.reset_layout();
                changed = true;
            }
        });
        widgets::card(ui, |ui| {
            ui.heading("History & alerts");
            ui.horizontal(|ui| {
                ui.label("Keep history for");
                for (r, l) in [(Retention::Days30, "30 days"), (Retention::Days90, "90 days"), (Retention::Months6, "6 months"), (Retention::Year1, "1 year")] {
                    changed |= ui.radio_value(&mut s.retention, r, l).changed();
                }
            });
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut s.notifications, "Notifications").changed();
                ui.label("Idle dev server after (min)");
                changed |= ui.add(egui::DragValue::new(&mut s.idle_threshold_min).range(5..=1440)).changed();
                ui.label("Disk write alert (MB/s)");
                let mut mb = s.alert_disk_write_bps / 1e6;
                if ui.add(egui::DragValue::new(&mut mb).range(1.0..=10_000.0)).changed() {
                    s.alert_disk_write_bps = mb * 1e6;
                    changed = true;
                }
                ui.label("Network alert (MB/s)");
                let mut nb = s.alert_network_bps / 1e6;
                if ui.add(egui::DragValue::new(&mut nb).range(1.0..=10_000.0)).changed() {
                    s.alert_network_bps = nb * 1e6;
                    changed = true;
                }
            });
        });
        widgets::card(ui, |ui| {
            ui.heading("AI plans (local only)");
            let mut remove = None;
            for (i, p) in s.plans.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(format!("prov{i}")).selected_text(&p.provider).show_ui(ui, |ui| {
                        for pr in ["claude", "codex", "copilot", "cursor"] {
                            changed |= ui.selectable_value(&mut p.provider, pr.to_string(), pr).changed();
                        }
                    });
                    changed |= ui.add(egui::TextEdit::singleline(&mut p.plan).desired_width(90.0)).changed();
                    ui.label("window h");
                    changed |= ui.add(egui::DragValue::new(&mut p.window_hours).range(1.0..=744.0)).changed();
                    if p.provider == "claude" {
                        ui.label("token budget");
                        let mut b = p.budget.unwrap_or(0.0);
                        if ui.add(egui::DragValue::new(&mut b).speed(100_000.0)).changed() {
                            p.budget = (b > 0.0).then_some(b);
                            changed = true;
                        }
                    }
                    if p.provider != "codex" {
                        ui.label("manual used %");
                        let mut m = p.manual_used_pct.unwrap_or(-1.0);
                        if ui.add(egui::DragValue::new(&mut m).range(-1.0..=100.0)).changed() {
                            p.manual_used_pct = (m >= 0.0).then_some(m);
                            changed = true;
                        }
                    }
                    if ui.small_button("✕").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                s.plans.remove(i);
                changed = true;
            }
            if ui.button("Add plan").clicked() {
                s.plans.push(PlanConfig { provider: "copilot".into(), plan: "Pro".into(), window_hours: 720.0, budget: None, manual_used_pct: None, manual_resets_at: None });
                changed = true;
            }
            ui.label(RichText::new("Codex usage is read from ~/.codex/sessions; Claude from ~/.claude/projects. No provider is contacted.").small().weak());
        });
        let mut copy = None;
        widgets::card(ui, |ui| {
            ui.heading("MCP server");
            changed |= ui.checkbox(&mut s.mcp_enabled, "Enable MCP server (stdio) — off by default").changed();
            let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or("pulsebar".into());
            let cmd = format!("claude mcp add pulsebar -- \"{exe}\" mcp");
            ui.horizontal(|ui| {
                ui.code(&cmd);
                if ui.small_button("Copy").clicked() {
                    copy = Some(cmd.clone());
                }
            });
            ui.label(RichText::new("Tools are read-only except pulsebar_request_quit, which always waits for your approval here.").small().weak());
        });
        if let Some(c) = copy {
            ctx.copy_text(c);
        }
        if changed {
            self.save_settings(ctx);
        }
    }

    // ------------------------------------------------------------------------------------------
    // Overlays
    // ------------------------------------------------------------------------------------------

    fn inspector_panel(&mut self, ctx: &egui::Context, snap: &Arc<Snapshot>) {
        let Some(st) = self.inspector.as_mut() else { return };
        if let Some(rx) = &st.rx
            && let Ok(d) = rx.try_recv() {
                if d.is_some() {
                    st.detail = d;
                }
                st.rx = None;
            }
        let u = self.settings.units;
        let cores = snap.machine.cpu.core_count;
        let mut close = false;
        let mut quit = None;
        let mut toast = None;
        egui::SidePanel::right("inspector").default_width(420.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Process Inspector");
                if ui.button("✕").clicked() {
                    close = true;
                }
            });
            let Some(d) = st.detail.clone() else {
                ui.label("Process has exited.");
                return;
            };
            ui.horizontal(|ui| {
                if ui.button("Copy").clicked() {
                    ctx.copy_text(inspector::to_text(&d, &u, cores));
                    toast = Some("Copied".to_string());
                }
                if ui.button("Export CSV").clicked() {
                    let p = paths::exports_dir().join(format!("pulsebar-process-{}.csv", d.identity.pid));
                    toast = Some(match std::fs::write(&p, inspector::to_csv(&d)) {
                        Ok(()) => format!("Saved {}", p.display()),
                        Err(e) => e.to_string(),
                    });
                }
                if ui.button("Open in Terminal").clicked() {
                    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or("pulsebar".into());
                    let script = format!("tell application \"Terminal\" to do script \"'{exe}' process {}\"", d.identity.pid);
                    let _ = std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).spawn();
                }
                if ui.button(self.i18n.t("quit") + "…").clicked() {
                    quit = Some(d.identity.pid);
                }
            });
            if st.rx.is_some() {
                ui.label(RichText::new("Checking code signature…").weak());
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.monospace(inspector::to_text(&d, &u, cores));
            });
        });
        if close {
            self.inspector = None;
        }
        if let Some(t) = toast {
            self.toast(t);
        }
        if let Some(pid) = quit {
            self.ask(snap, Target::Pid(pid), Mode::Quit);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(plan) = self.confirm.clone() {
            let mut decided: Option<bool> = None;
            egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.heading(if plan.mode == Mode::ForceQuit { "Force quit?" } else { "Confirm" });
                ui.label(plan.summary());
                ui.horizontal(|ui| {
                    if ui.button(self.i18n.t("cancel")).clicked() {
                        decided = Some(false);
                    }
                    let label = if plan.mode == Mode::ForceQuit { self.i18n.t("force_quit") } else { self.i18n.t("quit") };
                    if ui.button(RichText::new(label).strong()).clicked() {
                        decided = Some(true);
                    }
                });
            });
            match decided {
                Some(true) => {
                    let o = actions::execute(actions::confirm(plan, HumanYes::from_ui_click()));
                    self.toast(o.message);
                    self.confirm = None;
                }
                Some(false) => self.confirm = None,
                None => {}
            }
            return;
        }
        if let Some(req) = self.pending.first().cloned() {
            let mut decided: Option<bool> = None;
            egui::Modal::new(egui::Id::new("pending")).show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.heading("An assistant asks to stop something");
                ui.label(RichText::new(format!("Request from {} (MCP)", req.source)).weak());
                ui.label(&req.summary);
                ui.horizontal(|ui| {
                    if ui.button("Deny").clicked() {
                        decided = Some(false);
                    }
                    if ui.button(RichText::new("Allow").strong()).clicked() {
                        decided = Some(true);
                    }
                });
            });
            if let Some(yes) = decided {
                let r = actions::resolve_pending(req, yes.then(HumanYes::from_ui_click));
                if let Ok(r) = r {
                    self.toast(r.outcome.map(|o| o.message).unwrap_or_else(|| "Request denied.".into()));
                }
                self.pending.remove(0);
            }
        }
    }

    fn alerts_bar(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let alerts = self.shared.alerts.lock().unwrap().clone();
        if alerts.is_empty() {
            return;
        }
        let pal = self.pal(ui.ctx());
        let mut dismiss = None;
        let mut quit = None;
        for (i, a) in alerts.iter().enumerate().rev().take(3) {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("⚠ {}", a.message)).color(pal.danger));
                if !a.app_key.is_empty() && ui.small_button("Quit now…").clicked() {
                    quit = Some(a.app_key.clone());
                    dismiss = Some(i);
                }
                if ui.small_button("Silence 24 h").clicked() {
                    crate::daemon::snooze_alert(a, sys::now_s() + 86_400);
                    dismiss = Some(i);
                }
                if ui.small_button("Dismiss").clicked() {
                    dismiss = Some(i);
                }
            });
        }
        if let Some(i) = dismiss {
            crate::daemon::dismiss_alert(&alerts[i]);
            self.shared.alerts.lock().unwrap().remove(i);
        }
        if let Some(k) = quit {
            self.ask(snap, Target::App(k), Mode::Quit);
        }
        ui.separator();
    }
}

impl eframe::App for PulseApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Closing the window ends this process; the menu bar agent keeps running.
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        self.shared.foreground.store(focused, Ordering::Relaxed);
        self.handle_nav();
        if self.last_pending_poll.elapsed() > Duration::from_secs(1) {
            let fresh = actions::list_pending();
            if !fresh.is_empty() && self.pending.is_empty() {
                show_app();
            }
            self.pending = fresh;
            self.last_pending_poll = Instant::now();
        }
        self.toasts.retain(|(t, _)| t.elapsed() < Duration::from_secs(6));
        ctx.request_repaint_after(Duration::from_millis(1000));

        let snap = self.shared.snapshot.read().unwrap().clone();
        let Some(snap) = snap else {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.centered_and_justified(|ui| ui.label("Sampling…"));
            });
            return;
        };

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let tabs = self.settings.tabs.clone();
                for t in tabs.iter().filter(|t| t.visible) {
                    if ui.selectable_label(self.tab == t.tab, self.i18n.t(t.tab.key())).clicked() {
                        self.tab = t.tab;
                        self.highlight = None;
                    }
                }
            });
            ui.add_space(2.0);
        });
        if !self.toasts.is_empty() {
            egui::TopBottomPanel::bottom("toasts").show(ctx, |ui| {
                for (_, t) in &self.toasts {
                    ui.label(RichText::new(t).strong());
                }
            });
        }
        self.inspector_panel(ctx, &snap);
        egui::CentralPanel::default().show(ctx, |ui| {
            self.alerts_bar(ui, &snap);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                Tab::Overview => self.overview(ui, &snap),
                Tab::Cpu => self.cpu(ui, &snap),
                Tab::Memory => self.memory(ui, &snap),
                Tab::Disk => self.disk(ui, &snap),
                Tab::Network => self.network(ui, &snap),
                Tab::Gpu => self.gpu(ui, &snap),
                Tab::Neural => self.neural(ui, &snap),
                Tab::Battery => self.battery(ui, &snap),
                Tab::Sound => self.sound(ui, &snap),
                Tab::Bluetooth => self.bluetooth(ui, &snap),
                Tab::Projects => self.projects_tab(ui, &snap),
                Tab::Agents => self.agents_tab(ui, &snap),
                Tab::Containers => self.containers_tab(ui, &snap),
                Tab::History => self.history_tab(ui),
                Tab::Settings => self.settings_tab(ui, ctx),
            });
        });
        self.dialogs(ctx);
    }
}
