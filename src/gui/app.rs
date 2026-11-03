//! M5 — Main window: configurable tabs, resource views with Now/Average today/Peak and Top Apps,
//! developer tabs (Projects, Agents, Containers), History, Settings, the Process Inspector and
//! every confirmation dialog (including quit requests coming from MCP).

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Rect, RichText, Vec2};

use super::dialog::{self, Dialog, DialogIcon};
use super::icons::Icons;
use super::widgets::{self, ActionIcon, Palette};
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

mod tabs;

struct Stat {
    label: String,
    value: String,
    dot: Option<Color32>,
}

enum Footer {
    Bars(Vec<f64>, Option<f64>),
    /// (label, remaining 0..1, pace) per AI plan.
    Plans(Vec<(String, f64, PaceState)>),
}

/// One Overview metric tile.
struct CardSpec {
    tab: Tab,
    kind: MetricKind,
    title: String,
    caption: String,
    /// "54.59 GB": the last word is drawn smaller as the unit.
    value: String,
    badge: Option<(String, Color32)>,
    stats: Vec<Stat>,
    footer: Footer,
}

struct LegendRow {
    name: String,
    value: String,
    weight: f64,
    color: Color32,
    bundle: Option<String>,
}

/// Donut + legend card.
struct Breakdown3 {
    tab: Tab,
    glyph: &'static str,
    title: String,
    color: Color32,
    big: String,
    small: String,
    rows: Vec<LegendRow>,
}

/// "54.59 GB" → ("54.59", "GB"); "72% left" → ("72%", "left"); "—" → ("—", "").
fn split_unit(v: &str) -> (&str, &str) {
    match v.rsplit_once(' ') {
        Some((n, u)) if n.chars().any(|c| c.is_ascii_digit()) => (n, u),
        _ => (v, ""),
    }
}

fn tab_glyph(t: Tab) -> &'static str {
    match t {
        Tab::Overview => "⊞",
        Tab::Cpu => "▣",
        Tab::Memory => "☰",
        Tab::Disk => "🖴",
        Tab::Network => "🌐",
        Tab::Gpu => "🖥",
        Tab::Neural => "⚛",
        Tab::Battery => "🔋",
        Tab::Sound => "🔊",
        Tab::Bluetooth => "📶",
        Tab::Projects => "🗀",
        Tab::Agents => "👾",
        Tab::Containers => "📦",
        Tab::History => "🕓",
        Tab::Settings => "⚙",
    }
}

/// Watts as "427 mW" below 1 W, else "1.7 W".
fn format_power(w: f64) -> String {
    if w < 1.0 { format!("{:.0} mW", w * 1000.0) } else { format!("{w:.1} W") }
}

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
    frozen: Option<FrozenOrder>,
    started: Option<Instant>,
    screenshot_requested: bool,
    series_cache: HashMap<String, (Instant, Vec<f64>)>,
}

/// Row order of the app list while the pointer is over it. Without this a refresh can re-rank
/// the list between hover and click and slide another app's Quit button under the cursor
/// (egui identifies those buttons by position).
struct FrozenOrder {
    metric: RankMetric,
    apps: Vec<String>,
    procs: HashMap<String, Vec<i32>>,
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
            frozen: None,
            started: None,
            screenshot_requested: false,
            series_cache: HashMap::new(),
            shared,
            settings,
        };
        app.apply_style(&cc.egui_ctx);
        app
    }

    fn apply_style(&self, ctx: &egui::Context) {
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            ctx.style_mut_of(theme, |s| {
                let fill = widgets::window_fill(theme == egui::Theme::Dark);
                s.visuals.panel_fill = fill;
                s.visuals.window_fill = widgets::card_fill(theme == egui::Theme::Dark);
            });
        }
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

    // ------------------------------------------------------------------------------------------
    // Tabs
    // ------------------------------------------------------------------------------------------

    fn overview(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let u = self.settings.units;
        let m = &snap.machine;
        let sparks = self.shared.sparks.read().unwrap().clone();
        let dark = ui.visuals().dark_mode;
        let theme = self.settings.theme;
        let pal = self.pal(ui.ctx());
        let col = |k: MetricKind| widgets::metric_color(theme, dark, k);
        let now = sys::now_s();
        let midnight = local_midnight();
        let i18n = self.i18n.clone();
        let t = |k: &str| i18n.t(k);
        let dash = || "—".to_string();
        let stat = |label: &str, value: String| Stat { label: label.to_string(), value, dot: None };

        let cpu_today = self.today_stats("cpu");
        let gpu_today = self.today_stats("gpu");
        let ane_today = self.today_stats("neural");
        let written_today = self.history_total("disk_write", midnight);
        let net_today = self.history_total("net_down", midnight);
        let net_week = self.history_total("net_down", now - 7 * 86_400);
        let total_or_dash = |v: Option<f64>| v.filter(|v| *v > 0.0).map(|v| u.transferred(v as u64)).unwrap_or_else(dash);

        // Memory pressure in the spirit of Activity Monitor: swapping or a nearly full RAM.
        let mem = &m.memory;
        let used_frac = mem.used as f64 / mem.total.max(1) as f64;
        let (pressure, pressure_color) = if mem.swap_used > mem.total / 8 || used_frac > 0.95 {
            ("High", pal.danger)
        } else if mem.swap_used > 0 && used_frac > 0.85 {
            ("Elevated", pal.warn)
        } else {
            ("Normal", col(MetricKind::Battery))
        };
        let (mem_app_c, mem_wired_c, mem_comp_c) = (col(MetricKind::Cpu), col(MetricKind::Sound), col(MetricKind::Network));

        let boot = m.disk.boot_free();
        let battery = &m.battery;
        let mut cards = vec![
            CardSpec {
                tab: Tab::Cpu,
                kind: MetricKind::Cpu,
                title: t("tab.cpu"),
                caption: t("now"),
                value: u.cpu(m.cpu.total_pct, m.cpu.core_count),
                badge: None,
                stats: vec![
                    stat("User", u.cpu(m.cpu.user_pct, m.cpu.core_count)),
                    stat("System", u.cpu(m.cpu.system_pct, m.cpu.core_count)),
                    stat(&t("avg_today"), cpu_today.map(|s| u.cpu(s.0, m.cpu.core_count)).unwrap_or_else(dash)),
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Cpu), Some(100.0)),
            },
            CardSpec {
                tab: Tab::Memory,
                kind: MetricKind::Memory,
                title: t("tab.memory"),
                caption: format!("In Use of {}", u.bytes(mem.total)),
                value: u.bytes(mem.used),
                badge: Some((pressure.to_string(), pressure_color)),
                stats: vec![
                    Stat { label: "App".into(), value: u.bytes(mem.app), dot: Some(mem_app_c) },
                    Stat { label: "Wired".into(), value: u.bytes(mem.wired), dot: Some(mem_wired_c) },
                    Stat { label: "Compressed".into(), value: u.bytes(mem.compressed), dot: Some(mem_comp_c) },
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Memory), Some(mem.total as f64)),
            },
            CardSpec {
                tab: Tab::Gpu,
                kind: MetricKind::Gpu,
                title: t("tab.gpu"),
                caption: if m.gpu.name.is_empty() { "GPU".into() } else { m.gpu.name.clone() },
                value: format!("{:.0} %", m.gpu.utilization_pct),
                badge: None,
                stats: vec![
                    stat("Memory", if m.gpu.vram_used > 0 { u.bytes(m.gpu.vram_used) } else { dash() }),
                    stat("Average", gpu_today.map(|s| format!("{:.0}%", s.0)).unwrap_or_else(dash)),
                    stat(&t("peak"), gpu_today.map(|s| format!("{:.0}%", s.1)).unwrap_or_else(dash)),
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Gpu), Some(100.0)),
            },
            CardSpec {
                tab: Tab::Disk,
                kind: MetricKind::Disk,
                title: t("tab.disk"),
                caption: boot.map(|v| format!("Free of {}", u.bytes(v.total))).unwrap_or_else(|| "Free".into()),
                value: boot.map(|v| u.bytes(v.free)).unwrap_or_else(dash),
                badge: None,
                stats: vec![
                    stat("Reading", u.rate(m.disk.read_bps)),
                    stat("Writing", u.rate(m.disk.write_bps)),
                    stat("Written Today", total_or_dash(written_today)),
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Disk), None),
            },
            CardSpec {
                tab: Tab::Network,
                kind: MetricKind::Network,
                title: t("tab.network"),
                caption: "Downloading".into(),
                value: u.rate(m.network.rx_bps),
                badge: None,
                stats: vec![
                    stat("Uploading", u.rate(m.network.tx_bps)),
                    stat("Today", total_or_dash(net_today)),
                    stat("Last 7 Days", total_or_dash(net_week)),
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Network), None),
            },
            if battery.present {
                CardSpec {
                    tab: Tab::Battery,
                    kind: MetricKind::Battery,
                    title: t("tab.battery"),
                    caption: if battery.charging { "Charging" } else if battery.on_ac { "On Power Adapter" } else { "On Battery" }.into(),
                    value: format!("{:.0} %", battery.percent),
                    badge: None,
                    stats: vec![
                        stat(if battery.charging { "Until Full" } else { "Remaining" }, battery.time_remaining_min.filter(|m| *m > 0).map(|m| format_duration(m * 60)).unwrap_or_else(dash)),
                        stat("Power Draw", battery.power_w.map(f64::abs).or(m.power.system_w).map(|w| format!("{w:.1} W")).unwrap_or_else(dash)),
                        stat("Health", battery.health_pct.map(|h| format!("{h:.0}%")).unwrap_or_else(dash)),
                    ],
                    footer: Footer::Bars(sparks.get(MetricKind::Battery), Some(100.0)),
                }
            } else {
                CardSpec {
                    tab: Tab::Battery,
                    kind: MetricKind::Power,
                    title: "Power".into(),
                    caption: "System Power Draw".into(),
                    value: m.power.system_w.map(|w| format!("{w:.1} W")).unwrap_or_else(dash),
                    badge: None,
                    stats: vec![
                        stat("CPU", m.power.cpu_w.map(|w| format!("{w:.1} W")).unwrap_or_else(dash)),
                        stat("GPU", m.power.gpu_w.map(|w| format!("{w:.1} W")).unwrap_or_else(dash)),
                        stat("Neural", m.power.ane_w.map(|w| format!("{w:.2} W")).unwrap_or_else(dash)),
                    ],
                    footer: Footer::Bars(sparks.get(MetricKind::Power), None),
                }
            },
            CardSpec {
                tab: Tab::Neural,
                kind: MetricKind::Neural,
                title: "Neural Engine".into(),
                caption: if m.neural.available { "Apple Neural Engine".into() } else { "Not available on this Mac".into() },
                value: if m.neural.available { format!("{:.0} %", m.neural.utilization_pct) } else { dash() },
                badge: None,
                stats: vec![
                    stat("Power", if m.neural.available { format!("{:.2} W", m.neural.power_w) } else { dash() }),
                    stat("Average", ane_today.map(|s| format!("{:.0}%", s.0)).unwrap_or_else(dash)),
                    stat(&t("peak"), ane_today.map(|s| format!("{:.0}%", s.1)).unwrap_or_else(dash)),
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Neural), Some(100.0)),
            },
        ];
        cards.push(self.agents_card(snap, now));

        let gap = 14.0;
        let w4 = ((ui.available_width() - 3.0 * gap) / 4.0).floor().max(180.0);
        let card_h = 214.0;
        let mut go: Option<Tab> = None;
        for row in cards.chunks(4) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for c in row {
                    if self.metric_card(ui, Vec2::new(w4, card_h), c) {
                        go = Some(c.tab);
                    }
                }
            });
            ui.add_space(gap - ui.spacing().item_spacing.y);
        }

        // Breakdown cards: memory by type, memory by app, power by app.
        let w3 = ((ui.available_width() - 2.0 * gap) / 3.0).floor().max(240.0);
        let gray = if dark { Color32::from_gray(110) } else { Color32::from_gray(170) };
        let free_c = if dark { Color32::from_gray(60) } else { Color32::from_gray(215) };
        let by_type = Breakdown3 {
            tab: Tab::Memory,
            glyph: "☰",
            title: "Memory by Type".into(),
            color: col(MetricKind::Cpu),
            big: format!("{:.0}%", used_frac * 100.0),
            small: "in use".into(),
            rows: vec![
                LegendRow { name: "App".into(), value: u.bytes(mem.app), weight: mem.app as f64, color: mem_app_c, bundle: None },
                LegendRow { name: "Wired".into(), value: u.bytes(mem.wired), weight: mem.wired as f64, color: mem_wired_c, bundle: None },
                LegendRow { name: "Compressed".into(), value: u.bytes(mem.compressed), weight: mem.compressed as f64, color: mem_comp_c, bundle: None },
                LegendRow { name: "Cached".into(), value: u.bytes(mem.cached), weight: mem.cached as f64, color: gray, bundle: None },
                LegendRow { name: "Free".into(), value: u.bytes(mem.free), weight: mem.free as f64, color: free_c, bundle: None },
            ],
        };
        let mem_c = col(MetricKind::Memory);
        let by_app = self.app_breakdown(snap, RankMetric::Memory, "Memory by App", "⊞", mem_c, Tab::Memory, &|v| u.bytes(v as u64), gray);
        let pow_c = col(MetricKind::Battery);
        let by_power = self.app_breakdown(snap, RankMetric::Energy, "Power by App", "⚡", pow_c, Tab::Battery, &format_power, gray);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for b in [by_type, by_app, by_power] {
                if self.breakdown_card(ui, Vec2::new(w3, 196.0), &b) {
                    go = Some(b.tab);
                }
            }
        });

        if let Some(b) = projects::idle_banner(&snap.projects) {
            ui.add_space(10.0);
            if ui.add(egui::Button::new(RichText::new(format!("⚠ {b}")).color(pal.warn)).frame(false)).clicked() {
                go = Some(Tab::Projects);
            }
        }
        if let Some(tab) = go {
            self.tab = tab;
        }
    }

    /// The Agents card shows the plan closest to running out; the footer has one meter per plan.
    fn agents_card(&self, snap: &Snapshot, now: i64) -> CardSpec {
        let known: Vec<&AIPlan> = snap.plans.iter().filter(|p| p.pace_state != PaceState::Unknown).collect();
        let tightest = known.iter().copied().min_by(|a, b| (100.0 - a.used_pct).total_cmp(&(100.0 - b.used_pct)));
        let window = |w: &str| match w {
            "week" | "7d" => "This week".to_string(),
            "5h" => "5h window".to_string(),
            other => other.to_string(),
        };
        let sessions = snap.agents.len().to_string();
        let meters = known
            .iter()
            .take(4)
            .map(|p| (format!("{} {}", p.provider, p.window), (100.0 - p.used_pct) / 100.0, p.pace_state))
            .collect();
        match tightest {
            Some(p) => CardSpec {
                tab: Tab::Agents,
                kind: MetricKind::Bluetooth, // placeholder hue, replaced in metric_card
                title: self.i18n.t("tab.agents"),
                caption: [p.provider.as_str(), p.plan.as_str(), &window(&p.window)].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · "),
                value: format!("{:.0}% left", (100.0 - p.used_pct).max(0.0)),
                badge: None,
                stats: vec![
                    Stat { label: "Resets".into(), value: p.resets_at.map(|t| format_duration(t - now)).unwrap_or("—".into()), dot: None },
                    Stat { label: "Sessions".into(), value: sessions, dot: None },
                    Stat { label: "Pace".into(), value: if p.pace_text.is_empty() { "—".into() } else { p.pace_text.clone() }, dot: None },
                ],
                footer: Footer::Plans(meters),
            },
            None => CardSpec {
                tab: Tab::Agents,
                kind: MetricKind::Bluetooth,
                title: self.i18n.t("tab.agents"),
                caption: "No plan usage found".into(),
                value: "—".into(),
                badge: None,
                stats: vec![
                    Stat { label: "Sessions".into(), value: sessions, dot: None },
                    Stat { label: "Working".into(), value: snap.agents.iter().filter(|a| a.state == AgentState::Working).count().to_string(), dot: None },
                    Stat { label: "Memory".into(), value: self.settings.units.bytes(snap.agents.iter().map(|a| a.mem_bytes).sum()), dot: None },
                ],
                footer: Footer::Plans(vec![]),
            },
        }
    }

    /// Top four apps by `m` plus "Other", for a donut card.
    #[allow(clippy::too_many_arguments)]
    fn app_breakdown(&self, snap: &Snapshot, m: RankMetric, title: &str, glyph: &'static str, color: Color32, tab: Tab, fmt: &dyn Fn(f64) -> String, other_c: Color32) -> Breakdown3 {
        let total: f64 = snap.apps.iter().map(|a| a.metric(m)).sum();
        let top = snap.top_apps(m, 4);
        let shades = [1.0, 0.75, 0.55, 0.4];
        let mut rows: Vec<LegendRow> = top
            .iter()
            .enumerate()
            .map(|(i, a)| LegendRow { name: a.app.name.clone(), value: fmt(a.metric(m)), weight: a.metric(m), color: color.gamma_multiply(shades[i]), bundle: a.app.bundle_path.clone() })
            .collect();
        let other = (total - rows.iter().map(|r| r.weight).sum::<f64>()).max(0.0);
        rows.push(LegendRow { name: "Other".into(), value: fmt(other), weight: other, color: other_c, bundle: None });
        Breakdown3 { tab, glyph, title: title.into(), color, big: fmt(total), small: "all apps".into(), rows }
    }

    fn card_frame(ui: &mut egui::Ui, size: Vec2) -> (egui::Response, Rect) {
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let dark = ui.visuals().dark_mode;
        let mut fill = widgets::card_fill(dark);
        if resp.hovered() {
            fill = if dark { Color32::from_rgb(36, 36, 40) } else { Color32::from_rgb(250, 250, 252) };
        }
        ui.painter().rect_filled(rect, 16.0, fill);
        (resp.on_hover_cursor(egui::CursorIcon::PointingHand), rect.shrink(16.0))
    }

    fn card_title(&self, ui: &mut egui::Ui, glyph: &str, title: &str, color: Color32, chevron: bool) {
        ui.horizontal(|ui| {
            widgets::icon_badge(ui, glyph, color, 26.0);
            ui.add_space(4.0);
            ui.label(RichText::new(title).size(15.0).color(color).strong());
            if chevron {
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new("›").size(16.0).weak()));
            }
        });
    }

    /// One Overview tile. Returns true when clicked.
    fn metric_card(&self, ui: &mut egui::Ui, size: Vec2, c: &CardSpec) -> bool {
        let (resp, inner) = Self::card_frame(ui, size);
        let dark = ui.visuals().dark_mode;
        let pal = self.pal(ui.ctx());
        let color = if c.tab == Tab::Agents { widgets::metric_color(self.settings.theme, dark, MetricKind::Memory) } else { widgets::metric_color(self.settings.theme, dark, c.kind) };
        let glyph = if c.kind == MetricKind::Power { "⚡" } else { tab_glyph(c.tab) };
        // A child Ui paints inside the card without allocating space in the row again.
        ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(Align::Min))).scope(|ui| {
            ui.style_mut().interaction.selectable_labels = false;
            ui.spacing_mut().item_spacing.y = 4.0;
            self.card_title(ui, glyph, &c.title, color, true);
            ui.add_space(8.0);
            ui.label(RichText::new(&c.caption).size(12.5).weak());
            let (num, unit) = split_unit(&c.value);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.with_layout(egui::Layout::left_to_right(Align::Max), |ui| {
                    ui.label(widgets::num(&self.settings, num, 32.0).strong().color(ui.visuals().strong_text_color()));
                    if !unit.is_empty() {
                        ui.label(RichText::new(unit).size(17.0).weak());
                    }
                    if let Some((text, col)) = &c.badge {
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| widgets::pill(ui, text, *col));
                    }
                });
            });
            ui.add_space(6.0);
            let col_w = inner.width() / 3.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for s in &c.stats {
                    ui.allocate_ui_with_layout(Vec2::new(col_w, 36.0), egui::Layout::top_down(Align::Min), |ui| {
                        ui.set_width(col_w);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            if let Some(d) = s.dot {
                                widgets::dot(ui, d);
                            }
                            ui.add(egui::Label::new(RichText::new(&s.label).size(12.0).weak()).truncate());
                        });
                        ui.add(egui::Label::new(widgets::num(&self.settings, s.value.clone(), 13.5).strong().color(ui.visuals().strong_text_color())).truncate());
                    });
                }
            });
            ui.add_space(6.0);
            let footer = Vec2::new(inner.width(), (inner.bottom() - ui.cursor().top()).clamp(20.0, 40.0));
            match &c.footer {
                Footer::Bars(v, max) => {
                    widgets::bars(ui, v, footer, color, *max);
                }
                Footer::Plans(plans) if !plans.is_empty() => {
                    let slot = inner.width() / plans.len() as f32;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for (label, frac, pace) in plans {
                            let c = match pace {
                                PaceState::RunsOut => pal.danger,
                                PaceState::Ahead => pal.warn,
                                _ => color,
                            };
                            ui.allocate_ui_with_layout(Vec2::new(slot, footer.y), egui::Layout::top_down(Align::Min), |ui| {
                                ui.spacing_mut().item_spacing.y = 3.0;
                                ui.add(egui::Label::new(RichText::new(label).size(11.0).weak()).truncate());
                                widgets::meter(ui, *frac, slot - 12.0, c);
                            });
                        }
                    });
                }
                Footer::Plans(_) => {}
            }
        });
        resp.clicked()
    }

    fn breakdown_card(&mut self, ui: &mut egui::Ui, size: Vec2, b: &Breakdown3) -> bool {
        let (resp, inner) = Self::card_frame(ui, size);
        // A child Ui paints inside the card without allocating space in the row again.
        ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(Align::Min))).scope(|ui| {
            ui.style_mut().interaction.selectable_labels = false;
            self.card_title(ui, b.glyph, &b.title, b.color, false);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let d = (inner.bottom() - ui.cursor().top()).clamp(90.0, 128.0);
                let parts: Vec<(f64, Color32)> = b.rows.iter().map(|r| (r.weight, r.color)).collect();
                widgets::donut(ui, d, &parts, &b.big, &b.small);
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    for r in &b.rows {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            widgets::dot(ui, r.color);
                            if let Some(t) = r.bundle.as_deref().and_then(|p| self.icons.get(ui.ctx(), Some(p))) {
                                ui.image((t, Vec2::splat(16.0)));
                            }
                            // Leave room for the value so long names truncate instead of colliding.
                            let value_w = ui.painter().layout_no_wrap(r.value.clone(), egui::FontId::proportional(13.5), Color32::WHITE).size().x;
                            let name_w = (ui.available_width() - value_w - 10.0).max(40.0);
                            ui.allocate_ui_with_layout(Vec2::new(name_w, 18.0), egui::Layout::left_to_right(Align::Center), |ui| {
                                ui.set_width(name_w);
                                ui.add(egui::Label::new(RichText::new(&r.name).size(13.5)).truncate());
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                ui.label(widgets::num(&self.settings, r.value.clone(), 13.5).weak());
                            });
                        });
                    }
                });
            });
        });
        resp.clicked()
    }

    fn history_total(&mut self, metric: &str, since: i64) -> Option<f64> {
        let key = format!("total:{metric}:{}", since / 3600);
        if let Some((t, v)) = self.today.get(&key)
            && t.elapsed() < Duration::from_secs(30) {
                return v.map(|x| x.0);
            }
        let v = self.shared.history.lock().ok().and_then(|h| h.as_ref().and_then(|h| h.total_since(metric, since, sys::now_s()).ok()));
        self.today.insert(key, (Instant::now(), v.map(|x| (x, x))));
        v
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

    /// Fixed-width, vertically centred cell so the columns of a settings row line up.
    fn settings_cell<R>(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        let size = Vec2::new(width, 28.0);
        ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_size(size);
            add(ui)
        })
        .inner
    }

    /// Hairline between list rows.
    fn settings_divider(ui: &mut egui::Ui) {
        let r = ui.available_rect_before_wrap();
        let c = ui.visuals().widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6);
        ui.painter().hline(r.x_range(), ui.cursor().top() + 1.0, egui::Stroke::new(1.0, c));
        ui.add_space(3.0);
    }

    /// Up / down chevrons pinned to the right edge of a row; returns the requested move.
    fn move_buttons(ui: &mut egui::Ui, i: usize, n: usize, what: &str) -> Option<(usize, usize)> {
        let mut mv = None;
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            let down_tip = format!("Move {what} down");
            let up_tip = format!("Move {what} up");
            ui.add_enabled_ui(i + 1 < n, |ui| {
                if widgets::icon_button(ui, widgets::ActionIcon::MoveDown, ui.visuals().strong_text_color(), &down_tip).clicked() {
                    mv = Some((i, i + 1));
                }
            });
            ui.add_enabled_ui(i > 0, |ui| {
                if widgets::icon_button(ui, widgets::ActionIcon::MoveUp, ui.visuals().strong_text_color(), &up_tip).clicked() {
                    mv = Some((i, i - 1));
                }
            });
        });
        mv
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut changed = false;
        let s = &mut self.settings;
        Self::section(ui, "Units", "", |ui| {
            ui.horizontal_wrapped(|ui| {
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
        Self::section(ui, "Appearance", "", |ui| {
            ui.horizontal_wrapped(|ui| {
                changed |= ui.radio_value(&mut s.theme, Theme::Monochrome, "Monochrome").changed();
                changed |= ui.radio_value(&mut s.theme, Theme::Brand, "Brand colour").changed();
                changed |= ui.radio_value(&mut s.theme, Theme::ColorBlind, "Colour-blind safe").changed();
            });
            ui.horizontal_wrapped(|ui| {
                changed |= ui.radio_value(&mut s.number_font, NumberFont::Proportional, "Proportional numbers").changed();
                changed |= ui.radio_value(&mut s.number_font, NumberFont::Monospace, "Monospaced numbers").changed();
                changed |= ui.checkbox(&mut s.compact, "Compact density").changed();
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Language").weak());
                let cur = s.language.clone().unwrap_or_else(|| "System".into());
                egui::ComboBox::from_id_salt("lang").selected_text(cur).show_ui(ui, |ui| {
                    changed |= ui.selectable_value(&mut s.language, None, "System").changed();
                    for l in LANGS {
                        changed |= ui.selectable_value(&mut s.language, Some(l.to_string()), l).changed();
                    }
                });
            });
        });
        Self::section(ui, "Menu bar", "", |ui| {
            let mut mv: Option<(usize, usize)> = None;
            let n = s.menubar.len();
            for (i, it) in s.menubar.iter_mut().enumerate() {
                if i > 0 {
                    Self::settings_divider(ui);
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    Self::settings_cell(ui, 140.0, |ui| changed |= ui.checkbox(&mut it.visible, it.metric.label()).changed());
                    Self::settings_cell(ui, 104.0, |ui| {
                        egui::ComboBox::from_id_salt(format!("disp{i}"))
                            .width(92.0)
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
                    });
                    Self::settings_cell(ui, 84.0, |ui| changed |= ui.checkbox(&mut it.framed, "Framed").changed());
                    Self::settings_cell(ui, 112.0, |ui| {
                        let mut has = it.threshold.is_some();
                        if ui.checkbox(&mut has, "Red above").changed() {
                            it.threshold = if has { Some(it.metric.default_threshold().unwrap_or(80.0)) } else { None };
                            changed = true;
                        }
                    });
                    Self::settings_cell(ui, 64.0, |ui| {
                        if let Some(t) = it.threshold.as_mut() {
                            changed |= ui.add(egui::DragValue::new(t).speed(1.0)).changed();
                        }
                    });
                    if let Some(m) = Self::move_buttons(ui, i, n, it.metric.label()) {
                        mv = Some(m);
                    }
                });
            }
            if let Some((a, b)) = mv {
                s.move_menubar_item(a, b);
                changed = true;
            }
        });
        Self::section(ui, "Tabs", "", |ui| {
            let mut mv: Option<(usize, usize)> = None;
            let n = s.tabs.len();
            let i18n = I18n::new(s.language.as_deref());
            for (i, t) in s.tabs.iter_mut().enumerate() {
                if i > 0 {
                    Self::settings_divider(ui);
                }
                ui.horizontal(|ui| {
                    let enabled = t.tab != Tab::Settings;
                    let label = i18n.t(t.tab.key());
                    Self::settings_cell(ui, 200.0, |ui| {
                        changed |= ui.add_enabled(enabled, egui::Checkbox::new(&mut t.visible, label.clone())).changed();
                    });
                    if let Some(m) = Self::move_buttons(ui, i, n, &label) {
                        mv = Some(m);
                    }
                });
            }
            if let Some((a, b)) = mv {
                s.move_tab(a, b);
                changed = true;
            }
            ui.add_space(10.0);
            let reset = egui::Button::new(i18n.t("reset_layout")).corner_radius(8.0).min_size(Vec2::new(0.0, 28.0));
            if ui.add(reset).clicked() {
                s.reset_layout();
                changed = true;
            }
        });
        Self::section(ui, "History & alerts", "", |ui| {
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
        Self::section(ui, "AI plans (local only)", "", |ui| {
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
        Self::section(ui, "MCP server", "", |ui| {
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

    /// The app icon for a plan that targets one app (or one of its processes).
    fn plan_icon(&mut self, ctx: &egui::Context, snap: &Snapshot, plan: &Plan, warn: Color32) -> DialogIcon {
        let single_app = matches!(plan.target, Target::App(_) | Target::Pid(_));
        let bundle = plan.pids.first().filter(|_| single_app).and_then(|pid| {
            snap.apps.iter().find(|a| a.key != crate::grouping::SYSTEM_KEY && a.pids.contains(pid)).and_then(|a| a.app.bundle_path.clone())
        });
        match self.icons.get(ctx, bundle.as_deref()) {
            Some(tex) => DialogIcon::App { tex, badge: (plan.mode == Mode::ForceQuit).then_some(warn) },
            None => DialogIcon::Warning(warn),
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let pal = self.pal(ctx);
        if let Some(plan) = self.confirm.clone() {
            let force = plan.mode == Mode::ForceQuit;
            let tone = if force { pal.danger } else { pal.warn };
            let d = Dialog {
                id: "confirm",
                icon: self.plan_icon(ctx, snap, &plan, tone),
                title: plan.headline(),
                subject: None,
                lines: if force { vec!["The process is stopped immediately; unsaved work is lost.".into()] } else { vec![] },
                chips: plan.details(),
                cancel: self.i18n.t("cancel"),
                confirm: if force { self.i18n.t("force_quit") } else { self.i18n.t("quit") },
                confirm_color: if force { pal.danger } else { pal.accent },
                enter_confirms: true,
            };
            match dialog::show(ctx, &d) {
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
            let force = req.plan.mode == Mode::ForceQuit;
            let tone = if force { pal.danger } else { pal.warn };
            let d = Dialog {
                id: "pending",
                icon: DialogIcon::Warning(tone),
                title: "An assistant asks to stop something".into(),
                subject: Some(req.plan.headline()),
                lines: vec![format!("Request from {} (MCP)", req.source)],
                chips: req.plan.details(),
                cancel: "Deny".into(),
                confirm: "Allow".into(),
                confirm_color: if force { pal.danger } else { pal.accent },
                // Unlike the quit confirmation, this one appears unprompted: a stray Return
                // must never approve it.
                enter_confirms: false,
            };
            if let Some(yes) = dialog::show(ctx, &d) {
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

impl PulseApp {
    /// `PULSEBAR_SCREENSHOT=/path.png`: capture the window after a few seconds, save, quit.
    /// Reads our own framebuffer, so it needs no screen-recording permission.
    fn debug_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = std::env::var_os("PULSEBAR_SCREENSHOT") else { return };
        let started = *self.started.get_or_insert_with(Instant::now);
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                let _ = buf.save(&path);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if started.elapsed() > Duration::from_secs(8) && !self.screenshot_requested {
            self.screenshot_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        ctx.request_repaint_after(Duration::from_millis(200));
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

        self.debug_screenshot(ctx);
        let snap = self.shared.snapshot.read().unwrap().clone();
        let Some(snap) = snap else {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.centered_and_justified(|ui| ui.label("Sampling…"));
            });
            return;
        };

        egui::TopBottomPanel::top("tabs").show_separator_line(false).show(ctx, |ui| {
            ui.add_space(8.0);
            let dark = ui.visuals().dark_mode;
            let strip = if dark { Color32::from_rgb(28, 28, 31) } else { Color32::from_rgb(230, 230, 235) };
            let accent = widgets::metric_color(self.settings.theme, dark, MetricKind::Cpu);
            egui::Frame::new().fill(strip).corner_radius(12.0).inner_margin(4.0).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let tabs = self.settings.tabs.clone();
                    for t in tabs.iter().filter(|t| t.visible) {
                        let selected = self.tab == t.tab;
                        let text = RichText::new(format!("{}  {}", tab_glyph(t.tab), self.i18n.t(t.tab.key()))).size(13.0);
                        let text = if selected { text.color(accent) } else { text };
                        let fill = if selected { accent.gamma_multiply(0.18) } else { Color32::TRANSPARENT };
                        let b = egui::Button::new(text).fill(fill).stroke(egui::Stroke::NONE).corner_radius(9.0).min_size(Vec2::new(0.0, 30.0));
                        if ui.add(b).clicked() {
                            self.tab = t.tab;
                            self.highlight = None;
                        }
                    }
                });
            });
            ui.add_space(6.0);
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
        self.dialogs(ctx, &snap);
    }
}
