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
use super::shortcuts::{self, ListEffect, Row, Shortcut};
use super::{loc, nothing};
use super::widgets::{self, ActionIcon, Palette};
use super::{Nav, show_app};
use crate::actions::{self, HumanYes, Mode, PendingRequest, Plan, Target};
use crate::daemon::Shared;
use crate::history::RankRow;
use crate::i18n::{COMPLETE, I18n, PARTIAL};
use crate::inspector::{self, ProcessDetail};
use crate::model::*;
use crate::settings::{MenuDisplay, NumberFont, PlanConfig, Retention, Settings, Tab, Theme};
use crate::sorting::{self, FrozenOrder, SortKey, SortSpec};
use crate::units::{CpuUnit, DataUnit, TempUnit, Units, format_duration};
use crate::{docker, output, paths, projects, share_card, sys};

mod detail;
mod login;
mod sidebar;
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

/// Watts as "427 mW" below 1 W, else "1.7 W" ("1,7 W" in Spanish).
fn format_power(i18n: &I18n, w: f64) -> String {
    if w < 1.0 { format!("{:.0} mW", w * 1000.0) } else { i18n.num(format!("{w:.1} W")) }
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
    /// Selected process row inside the highlighted app (keyboard / click selection).
    sel_proc: Option<i32>,
    scroll_to_highlight: bool,
    scroll_to_sel: bool,
    /// Rows of the app list as drawn last frame, for ↑ ↓ → ←.
    list_rows: Vec<Row>,
    /// A text field had focus at the end of the last frame: shortcuts stay out of its way.
    text_focus: bool,
    inspector: Option<InspectorState>,
    detail: Option<detail::AppDetailState>,
    confirm: Option<Plan>,
    /// App whose quit confirmation was requested (notification action); opened once sampled.
    quit_request: Option<String>,
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
    /// Last ↑ ↓ → ← use: the list order stays frozen for a few seconds after it, even with the
    /// pointer elsewhere.
    last_key_nav: Option<Instant>,
    started: Option<Instant>,
    screenshot_requested: bool,
    /// Which font set is installed (see `nothing::fonts`); `None` until the first `apply_style`.
    nothing_fonts: Option<bool>,
    series_cache: HashMap<String, (Instant, Vec<f64>)>,
    login: login::LoginUi,
    /// Search query per tab (kept while the window is open).
    search: HashMap<Tab, String>,
    /// Set by ⌘F; the visible app list's search field takes focus on the next paint.
    focus_search: bool,
}

/// How long the app list keeps its order after the last keyboard navigation.
const KEY_NAV_FREEZE: Duration = Duration::from_secs(5);

fn local_midnight() -> i64 {
    let now = chrono::Local::now();
    now.date_naive().and_hms_opt(0, 0, 0).and_then(|d| d.and_local_timezone(chrono::Local).single()).map(|d| d.timestamp()).unwrap_or(now.timestamp() - 86_400)
}

impl PulseApp {
    pub fn new(cc: &eframe::CreationContext<'_>, shared: Arc<Shared>, tab: Tab, app_key: Option<String>, quit: bool) -> Self {
        let settings = shared.settings.read().unwrap().clone();
        let mut app = Self {
            i18n: I18n::new(settings.language.as_deref()),
            tab,
            scroll_to_highlight: app_key.is_some(),
            quit_request: app_key.clone().filter(|_| quit),
            highlight: app_key,
            sel_proc: None,
            scroll_to_sel: false,
            list_rows: vec![],
            text_focus: false,
            inspector: None,
            detail: None,
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
            last_key_nav: None,
            started: None,
            screenshot_requested: false,
            series_cache: HashMap::new(),
            login: login::LoginUi::new(),
            search: HashMap::new(),
            focus_search: false,
            shared,
            settings,
            nothing_fonts: None,
        };
        app.apply_style(&cc.egui_ctx);
        app
    }

    fn apply_style(&mut self, ctx: &egui::Context) {
        let nothing_on = self.settings.theme == Theme::Nothing;
        nothing::set_active(ctx, nothing_on);
        // Only on a real switch: comparing or rebuilding font atlases is not free, and settings
        // are saved often.
        if self.nothing_fonts != Some(nothing_on) {
            self.nothing_fonts = Some(nothing_on);
            ctx.set_fonts(nothing::fonts(nothing_on));
        }
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let dark = theme == egui::Theme::Dark;
            ctx.style_mut_of(theme, |s| {
                // From the defaults each time, so leaving the Nothing theme leaves nothing behind.
                s.visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
                s.text_styles = egui::Style::default().text_styles;
                s.visuals.panel_fill = widgets::window_fill(dark);
                s.visuals.window_fill = widgets::base_card_fill(dark);
                if nothing_on {
                    nothing::apply(s, dark);
                }
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
        // Everything the window shows is rebuilt from `self.i18n` every frame, so a language
        // change is live as soon as the settings are saved; the menu bar agent reloads
        // settings.json on its own.
        ctx.request_repaint();
        self.apply_style(ctx);
    }

    /// The units setting plus the language's decimal sign. Use this instead of `settings.units`
    /// for anything that is displayed.
    fn units(&self) -> Units {
        Units { decimal_comma: self.i18n.decimal_comma(), ..self.settings.units }
    }

    fn t(&self, key: &str) -> String {
        self.i18n.t(key)
    }

    fn tf(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.i18n.tf(key, args)
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
            Err(e) => {
                let m = loc::error_text(&self.i18n, &e);
                self.toast(m)
            }
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
        // Both are right-hand panels: only one at a time.
        self.detail = None;
        let (tx, rx) = channel();
        let s = snap.clone();
        std::thread::spawn(move || {
            let _ = tx.send(inspector::inspect(&s, pid, true));
        });
        self.inspector = Some(InspectorState { detail: inspector::inspect(snap, pid, false), rx: Some(rx) });
    }

    /// A confirmation or MCP approval dialog is showing.
    fn dialog_open(&self) -> bool {
        self.confirm.is_some() || !self.pending.is_empty()
    }

    /// Navigation requests from the menu bar agent (nav.json).
    fn handle_nav(&mut self, ctx: &egui::Context) {
        if self.last_nav_poll.elapsed() < Duration::from_millis(300) {
            return;
        }
        self.last_nav_poll = Instant::now();
        let Some(nav) = std::fs::read(paths::nav_file()).ok().and_then(|b| serde_json::from_slice::<Nav>(&b).ok()) else { return };
        if nav.at_ms > self.nav_seen {
            self.nav_seen = nav.at_ms;
            self.tab = nav.tab;
            self.scroll_to_highlight = nav.app.is_some();
            self.quit_request = nav.app.clone().filter(|_| nav.quit);
            // A search on the target tab could hide the app being opened.
            if nav.app.is_some() {
                self.search.remove(&nav.tab);
            }
            self.highlight = nav.app;
            self.sel_proc = None;
            show_app();
            // AppKit raises the window above; everywhere else it takes a viewport command.
            if cfg!(not(target_os = "macos")) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
    }


    /// Selects a row of the app list and scrolls it into view.
    fn select_row(&mut self, row: &Row) {
        match row {
            Row::App(k) => {
                self.highlight = Some(k.clone());
                self.sel_proc = None;
            }
            Row::Proc { app, pid } => {
                self.highlight = Some(app.clone());
                self.sel_proc = Some(*pid);
            }
        }
        self.scroll_to_sel = true;
        self.last_key_nav = Some(Instant::now());
    }

    /// Keyboard navigation happened recently enough to keep the rows where they are.
    fn key_nav_active(&self) -> bool {
        self.last_key_nav.is_some_and(|t| t.elapsed() < KEY_NAV_FREEZE)
    }

    fn selected_row(&self) -> Option<Row> {
        let app = self.highlight.clone()?;
        Some(match self.sel_proc {
            Some(pid) => Row::Proc { app, pid },
            None => Row::App(app),
        })
    }

    fn go_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.highlight = None;
        self.sel_proc = None;
    }

    /// Window-wide keyboard shortcuts. They stay silent while a text field has focus, and under
    /// a modal dialog only ⌘W works.
    fn handle_shortcuts(&mut self, ctx: &egui::Context, snap: &Arc<Snapshot>) {
        if self.text_focus {
            return;
        }
        let modal = self.confirm.is_some() || !self.pending.is_empty();
        let widget_focused = ctx.wants_keyboard_input();
        let events = ctx.input(|i| i.events.clone());
        for e in events {
            let egui::Event::Key { key, pressed: true, repeat, modifiers, .. } = e else { continue };
            let Some(sc) = shortcuts::map(key, modifiers) else { continue };
            if !shortcuts::allowed(sc, repeat, modal, widget_focused) {
                continue;
            }
            let visible: Vec<Tab> = self.settings.tabs.iter().filter(|t| t.visible).map(|t| t.tab).collect();
            match sc {
                Shortcut::TabNumber(n) => {
                    let Some(t) = shortcuts::tab_by_number(&visible, n) else { continue };
                    self.go_tab(t);
                }
                Shortcut::PrevTab | Shortcut::NextTab => {
                    let Some(t) = shortcuts::cycle_tab(&visible, self.tab, if sc == Shortcut::NextTab { 1 } else { -1 }) else { continue };
                    self.go_tab(t);
                }
                Shortcut::OpenSettings => self.go_tab(Tab::Settings),
                Shortcut::CloseWindow => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Shortcut::Up | Shortcut::Down | Shortcut::Expand | Shortcut::Collapse => {
                    let expanded = &self.expanded;
                    let effect = shortcuts::list_effect(sc, &self.list_rows, self.selected_row().as_ref(), &|k| expanded.get(k).copied().unwrap_or(false));
                    match effect {
                        ListEffect::Select(r) => self.select_row(&r),
                        ListEffect::SetExpanded(k, open) => {
                            self.expanded.insert(k, open);
                            self.last_key_nav = Some(Instant::now());
                        }
                        ListEffect::Nothing => continue,
                    }
                }
                Shortcut::Inspect | Shortcut::Quit | Shortcut::ForceQuit => {
                    let Some(row) = self.selected_row().filter(|r| self.list_rows.contains(r)) else { continue };
                    let mode = if sc == Shortcut::ForceQuit { Mode::ForceQuit } else { Mode::Quit };
                    match (sc, row) {
                        // Force Quit is for apps only: the mouse offers no Force Quit on a process
                        // row, and the System group would otherwise be reachable with SIGKILL.
                        (Shortcut::ForceQuit, Row::Proc { .. }) => continue,
                        (Shortcut::Inspect, Row::Proc { pid, .. }) => self.open_inspector(snap, pid),
                        (Shortcut::Inspect, Row::App(k)) => {
                            // An app has no single process: inspect its biggest one.
                            let pid = snap.apps.iter().find(|a| a.key == k).and_then(|a| a.pids.iter().filter_map(|p| snap.process(*p)).max_by_key(|p| p.mem_bytes).map(|p| p.pid).or(Some(a.root_pid)));
                            if let Some(pid) = pid {
                                self.open_inspector(snap, pid);
                            }
                        }
                        (_, Row::Proc { pid, .. }) => self.ask(snap, Target::Pid(pid), mode),
                        (_, Row::App(k)) => self.ask(snap, Target::App(k), mode),
                    }
                }
            }
            // Handled: keep egui's own focus navigation and text handling from seeing the key.
            ctx.input_mut(|i| i.consume_key(modifiers, key));
        }
    }

    // ------------------------------------------------------------------------------------------
    // Shared pieces
    // ------------------------------------------------------------------------------------------

    // ------------------------------------------------------------------------------------------
    // Tabs
    // ------------------------------------------------------------------------------------------

    /// How many cards per row fit in `avail` (at least `min_w` wide, at most `max_cols`) and their width.
    fn grid(avail: f32, gap: f32, min_w: f32, max_cols: usize) -> (usize, f32) {
        let cols = (((avail + gap) / (min_w + gap)) as usize).clamp(1, max_cols);
        (cols, ((avail - gap * (cols - 1) as f32) / cols as f32).floor().max(1.0))
    }

    fn overview(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let u = self.units();
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
        // `key` is an i18n key.
        let stat = |key: &str, value: String| Stat { label: i18n.t(key), value, dot: None };
        let pw = |w: f64| i18n.num(format!("{w:.1} W"));

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
            (t("pressure.high"), pal.danger)
        } else if mem.swap_used > 0 && used_frac > 0.85 {
            (t("pressure.elevated"), pal.warn)
        } else {
            (t("pressure.normal"), col(MetricKind::Battery))
        };
        let [mem_app_c, mem_wired_c, mem_comp_c] = widgets::memory_colors(theme, dark);

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
                    stat("st.user", u.cpu(m.cpu.user_pct, m.cpu.core_count)),
                    stat("st.system", u.cpu(m.cpu.system_pct, m.cpu.core_count)),
                    stat("avg_today", cpu_today.map(|s| u.cpu(s.0, m.cpu.core_count)).unwrap_or_else(dash)),
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Cpu), Some(100.0)),
            },
            CardSpec {
                tab: Tab::Memory,
                kind: MetricKind::Memory,
                title: t("tab.memory"),
                caption: self.tf("in_use_of", &[("total", &u.bytes(mem.total))]),
                value: u.bytes(mem.used),
                badge: Some((pressure, pressure_color)),
                stats: vec![
                    Stat { label: t("mem.app"), value: u.bytes(mem.app), dot: Some(mem_app_c) },
                    Stat { label: t("mem.wired"), value: u.bytes(mem.wired), dot: Some(mem_wired_c) },
                    Stat { label: t("mem.compressed"), value: u.bytes(mem.compressed), dot: Some(mem_comp_c) },
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Memory), Some(mem.total as f64)),
            },
            CardSpec {
                tab: Tab::Gpu,
                kind: MetricKind::Gpu,
                title: t("tab.gpu"),
                caption: if m.gpu.name.is_empty() { "GPU".into() } else { m.gpu.name.clone() },
                value: if m.gpu.available { format!("{:.0} %", m.gpu.utilization_pct) } else { dash() },
                badge: None,
                stats: vec![
                    stat("tab.memory", if m.gpu.vram_used > 0 { u.bytes(m.gpu.vram_used) } else { dash() }),
                    stat("st.average", gpu_today.map(|s| format!("{:.0}%", s.0)).unwrap_or_else(dash)),
                    stat("peak", gpu_today.map(|s| format!("{:.0}%", s.1)).unwrap_or_else(dash)),
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Gpu), Some(100.0)),
            },
            CardSpec {
                tab: Tab::Disk,
                kind: MetricKind::Disk,
                title: t("tab.disk"),
                caption: boot.map(|v| self.tf("free_of", &[("total", &u.bytes(v.total))])).unwrap_or_else(|| t("free")),
                value: boot.map(|v| u.bytes(v.free)).unwrap_or_else(dash),
                badge: None,
                stats: vec![
                    stat("st.reading", u.rate(m.disk.read_bps)),
                    stat("st.writing", u.rate(m.disk.write_bps)),
                    stat("st.written_today", total_or_dash(written_today)),
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Disk), None),
            },
            CardSpec {
                tab: Tab::Network,
                kind: MetricKind::Network,
                title: t("tab.network"),
                caption: t("st.downloading"),
                value: u.rate(m.network.rx_bps),
                badge: None,
                stats: vec![
                    stat("st.uploading", u.rate(m.network.tx_bps)),
                    stat("st.today", total_or_dash(net_today)),
                    stat("st.last_7d", total_or_dash(net_week)),
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Network), None),
            },
            if battery.present {
                CardSpec {
                    tab: Tab::Battery,
                    kind: MetricKind::Battery,
                    title: t("tab.battery"),
                    caption: t(if battery.charging { "battery.charging" } else if battery.on_ac { "battery.on_adapter" } else { "battery.on_battery" }),
                    value: format!("{:.0} %", battery.percent),
                    badge: None,
                    stats: vec![
                        stat(if battery.charging { "st.until_full" } else { "st.remaining" }, battery.time_remaining_min.filter(|m| *m > 0).map(|m| format_duration(m * 60)).unwrap_or_else(dash)),
                        stat("st.power_draw", battery.power_w.map(f64::abs).or(m.power.system_w).map(pw).unwrap_or_else(dash)),
                        stat("st.health", battery.health_pct.map(|h| format!("{h:.0}%")).unwrap_or_else(dash)),
                    ],
                    footer: Footer::Bars(sparks.get(MetricKind::Battery), Some(100.0)),
                }
            } else {
                CardSpec {
                    tab: Tab::Battery,
                    kind: MetricKind::Power,
                    title: t("st.power"),
                    caption: t("st.system_power_draw"),
                    value: m.power.system_w.map(pw).unwrap_or_else(dash),
                    badge: None,
                    stats: vec![
                        stat("tab.cpu", m.power.cpu_w.map(pw).unwrap_or_else(dash)),
                        stat("tab.gpu", m.power.gpu_w.map(pw).unwrap_or_else(dash)),
                        stat("tab.neural", m.power.ane_w.map(|w| i18n.num(format!("{w:.2} W"))).unwrap_or_else(dash)),
                    ]
                    .into_iter()
                    // Linux machines have no Neural Engine to report (see `Tab::supported`).
                    .take(if Tab::Neural.supported() { 3 } else { 2 })
                    .collect(),
                    footer: Footer::Bars(sparks.get(MetricKind::Power), None),
                }
            },
            CardSpec {
                tab: Tab::Neural,
                kind: MetricKind::Neural,
                title: "Neural Engine".into(),
                caption: if m.neural.available { "Apple Neural Engine".into() } else { t("not_available_mac") },
                value: if m.neural.available { format!("{:.0} %", m.neural.utilization_pct) } else { dash() },
                badge: None,
                stats: vec![
                    stat("st.power", if m.neural.available { i18n.num(format!("{:.2} W", m.neural.power_w)) } else { dash() }),
                    stat("st.average", ane_today.map(|s| format!("{:.0}%", s.0)).unwrap_or_else(dash)),
                    stat("peak", ane_today.map(|s| format!("{:.0}%", s.1)).unwrap_or_else(dash)),
                ],
                footer: Footer::Bars(sparks.get(MetricKind::Neural), Some(100.0)),
            },
        ];
        cards.push(self.agents_card(snap, now));
        cards.retain(|c| c.tab.supported());

        let gap = 14.0;
        let card_h = 214.0;
        let (cols4, w4) = Self::grid(ui.available_width(), gap, 180.0, 4);
        let mut go: Option<Tab> = None;
        for row in cards.chunks(cols4) {
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
        let (cols3, w3) = Self::grid(ui.available_width(), gap, 240.0, 3);
        let (gray, free_c) = widgets::muted_colors(theme, dark);
        let by_type = Breakdown3 {
            tab: Tab::Memory,
            glyph: "☰",
            title: t("memory_by_type"),
            color: col(MetricKind::Cpu),
            big: format!("{:.0}%", used_frac * 100.0),
            small: t("in_use"),
            rows: vec![
                LegendRow { name: t("mem.app"), value: u.bytes(mem.app), weight: mem.app as f64, color: mem_app_c, bundle: None },
                LegendRow { name: t("mem.wired"), value: u.bytes(mem.wired), weight: mem.wired as f64, color: mem_wired_c, bundle: None },
                LegendRow { name: t("mem.compressed"), value: u.bytes(mem.compressed), weight: mem.compressed as f64, color: mem_comp_c, bundle: None },
                LegendRow { name: t("mem.cached"), value: u.bytes(mem.cached), weight: mem.cached as f64, color: gray, bundle: None },
                LegendRow { name: t("free"), value: u.bytes(mem.free), weight: mem.free as f64, color: free_c, bundle: None },
            ],
        };
        let mem_c = col(MetricKind::Memory);
        let by_app = self.app_breakdown(snap, RankMetric::Memory, "memory_by_app", "⊞", mem_c, Tab::Memory, &|v| u.bytes(v as u64), gray);
        let pow_c = col(MetricKind::Battery);
        // Linux has no per-process energy counter: this card would only ever show zeros.
        let by_power = if cfg!(target_os = "linux") {
            let cores = m.cpu.core_count;
            self.app_breakdown(snap, RankMetric::Cpu, "cpu_by_app", "▣", pow_c, Tab::Cpu, &|v| u.cpu(v, cores), gray)
        } else {
            self.app_breakdown(snap, RankMetric::Energy, "power_by_app", "⚡", pow_c, Tab::Battery, &|w| format_power(&i18n, w), gray)
        };
        for row in [by_type, by_app, by_power].chunks(cols3) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for b in row {
                    if self.breakdown_card(ui, Vec2::new(w3, 196.0), b) {
                        go = Some(b.tab);
                    }
                }
            });
            ui.add_space(gap - ui.spacing().item_spacing.y);
        }

        if let Some(b) = loc::idle_banner(&self.i18n, &u, &snap.projects) {
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
        let window = |w: &str| loc::window_label(&self.i18n, w);
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
                value: self.tf("pct_left", &[("v", &format!("{:.0}", (100.0 - p.used_pct).max(0.0)))]),
                badge: None,
                stats: vec![
                    Stat { label: self.t("st.resets"), value: p.resets_at.map(|t| format_duration(t - now)).unwrap_or("—".into()), dot: None },
                    Stat { label: self.t("st.sessions"), value: sessions, dot: None },
                    Stat { label: self.t("st.pace"), value: if p.pace_text.is_empty() { "—".into() } else { loc::pace_text(&self.i18n, &p.pace_text) }, dot: None },
                ],
                footer: Footer::Plans(meters),
            },
            None => CardSpec {
                tab: Tab::Agents,
                kind: MetricKind::Bluetooth,
                title: self.i18n.t("tab.agents"),
                caption: self.t("no_plan_usage"),
                value: "—".into(),
                badge: None,
                stats: vec![
                    Stat { label: self.t("st.sessions"), value: sessions, dot: None },
                    Stat { label: self.t("st.working"), value: snap.agents.iter().filter(|a| a.state == AgentState::Working).count().to_string(), dot: None },
                    Stat { label: self.t("tab.memory"), value: self.units().bytes(snap.agents.iter().map(|a| a.mem_bytes).sum()), dot: None },
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
        rows.push(LegendRow { name: self.t("other"), value: fmt(other), weight: other, color: other_c, bundle: None });
        Breakdown3 { tab, glyph, title: self.t(title), color, big: fmt(total), small: self.t("all_apps"), rows }
    }

    fn card_frame(ui: &mut egui::Ui, size: Vec2) -> (egui::Response, Rect) {
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        let dark = ui.visuals().dark_mode;
        let mut fill = widgets::card_fill(ui.visuals());
        if resp.hovered() {
            fill = if nothing::is_active(ui.ctx()) {
                ui.visuals().faint_bg_color
            } else if dark {
                Color32::from_rgb(36, 36, 40)
            } else {
                Color32::from_rgb(250, 250, 252)
            };
        }
        widgets::paint_card(ui, rect, fill);
        (resp.on_hover_cursor(egui::CursorIcon::PointingHand), rect.shrink(16.0))
    }

    fn card_title(&self, ui: &mut egui::Ui, glyph: &str, title: &str, color: Color32, chevron: bool) {
        ui.horizontal(|ui| {
            if nothing::is_active(ui.ctx()) {
                ui.label(widgets::caption(ui.ctx(), title, 15.0).color(color));
            } else {
                widgets::icon_badge(ui, glyph, color, 26.0);
                ui.add_space(4.0);
                ui.label(RichText::new(title).size(15.0).color(color).strong());
            }
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
            let nothing_on = nothing::is_active(ui.ctx());
            let caption = egui::Label::new(widgets::caption(ui.ctx(), &c.caption, 12.5));
            // All-caps data type runs wide; a wrapped line would push the card's contents down.
            ui.add(if nothing_on { caption.truncate() } else { caption });
            let (num, unit) = split_unit(&c.value);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.with_layout(egui::Layout::left_to_right(Align::Max), |ui| {
                    ui.label(widgets::num(&self.settings, num, 32.0).strong().color(ui.visuals().strong_text_color()));
                    if !unit.is_empty() {
                        ui.label(widgets::unit(ui.ctx(), unit, 17.0));
                    }
                    if let Some((text, col)) = &c.badge {
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| widgets::pill(ui, text, *col));
                    }
                });
            });
            ui.add_space(6.0);
            let col_w = inner.width() / 3.0;
            // Top-aligned: a column taller than its slot (fonts differ) must not shift the next one.
            ui.horizontal_top(|ui| {
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
                            ui.add(egui::Label::new(widgets::small_caption(ui.ctx(), &s.label, 12.0)).truncate());
                        });
                        ui.add(egui::Label::new(widgets::num(&self.settings, s.value.clone(), if nothing_on { 11.0 } else { 13.5 }).strong().color(ui.visuals().strong_text_color())).truncate());
                    });
                }
            });
            ui.add_space(6.0);
            let footer = Vec2::new(inner.width(), (inner.bottom() - ui.cursor().top()).clamp(20.0, 40.0));
            match &c.footer {
                Footer::Bars(v, max) => {
                    // The Nothing theme paints the card's chart bars in its signal red.
                    let bar_c = if nothing_on { nothing::ACCENT } else { color };
                    widgets::bars(ui, v, footer, bar_c, *max);
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
                                ui.add(egui::Label::new(widgets::caption(ui.ctx(), label, 11.0)).truncate());
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
                // Space Mono values need more of the row: the gauge gives way, and is thin anyway.
                let nothing_on = nothing::is_active(ui.ctx());
                let d = (inner.bottom() - ui.cursor().top()).clamp(90.0, if nothing_on { 104.0 } else { 128.0 });
                let value_size = if nothing_on { 12.0 } else { 13.5 };
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
                            let value_w = ui.painter().layout_no_wrap(r.value.clone(), widgets::num_font(&self.settings, value_size), Color32::WHITE).size().x;
                            let name_w = (ui.available_width() - value_w - 10.0).max(40.0);
                            ui.allocate_ui_with_layout(Vec2::new(name_w, 18.0), egui::Layout::left_to_right(Align::Center), |ui| {
                                ui.set_width(name_w);
                                ui.add(egui::Label::new(RichText::new(&r.name).size(13.5)).truncate());
                            });
                            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                ui.label(widgets::num(&self.settings, r.value.clone(), value_size).weak());
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
        let u = self.units();
        let i18n = self.i18n.clone();
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("hmetric").selected_text(i18n.t(&format!("metric.{}", self.history_ui.metric))).show_ui(ui, |ui| {
                for k in MetricKind::ALL {
                    ui.selectable_value(&mut self.history_ui.metric, k.as_str().to_string(), i18n.t(&format!("metric.{}", k.as_str())));
                }
            });
            for (label, s) in [("range.12h", 12 * 3600), ("range.24h", 86_400), ("range.7d", 7 * 86_400), ("range.30d", 30 * 86_400), ("range.12m", 365 * 86_400)] {
                ui.selectable_value(&mut self.history_ui.range_s, s, i18n.t(label));
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
        let loc_i18n = i18n.clone();
        let fmt = move |v: f64| match metric.as_str() {
            "memory" => u.bytes(v as u64),
            "disk" | "network" => u.rate(v),
            "temperature" => u.temp(v),
            "power" | "neural" => loc_i18n.num(format!("{v:.1} W")),
            _ => format!("{v:.0}"),
        };
        if let Some((_, _, pts, rank)) = &self.history_ui.cache {
            let kind = MetricKind::parse(&self.history_ui.metric).unwrap_or(MetricKind::Cpu);
            let dark = ui.visuals().dark_mode;
            widgets::card(ui, |ui| widgets::history_chart(ui, &i18n, pts, 220.0, widgets::chart_color(self.settings.theme, dark, kind), &fmt));
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(i18n.t("hist.ranking"));
                    ui.selectable_value(&mut self.history_ui.rank_metric, "app_memory".to_string(), i18n.t("tab.memory"));
                    ui.selectable_value(&mut self.history_ui.rank_metric, "app_energy".to_string(), i18n.t("hist.energy"));
                    ui.selectable_value(&mut self.history_ui.rank_metric, "app_cpu".to_string(), "CPU");
                });
                let rm = self.history_ui.rank_metric.clone();
                for (i, r) in rank.iter().enumerate() {
                    let v = match rm.as_str() {
                        "app_memory" => i18n.tf("hist.avg_peak", &[("avg", &u.bytes(r.avg as u64)), ("peak", &u.bytes(r.peak as u64))]),
                        "app_energy" => i18n.tf("hist.wh", &[("wh", &i18n.num(format!("{:.1}", r.integral / 3600.0))), ("w", &i18n.num(format!("{:.2}", r.avg)))]),
                        _ => i18n.tf("hist.avg_peak", &[("avg", &i18n.num(format!("{:.1} %", r.avg))), ("peak", &format!("{:.0} %", r.peak))]),
                    };
                    ui.label(format!("{}. {}  —  {v}", i + 1, r.app));
                }
                if rank.is_empty() {
                    ui.label(RichText::new(i18n.t("hist.no_data")).weak());
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
    fn move_buttons(ui: &mut egui::Ui, i18n: &I18n, i: usize, n: usize, what: &str) -> Option<(usize, usize)> {
        let mut mv = None;
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            let down_tip = i18n.tf("move_down", &[("what", what)]);
            let up_tip = i18n.tf("move_up", &[("what", what)]);
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
        self.login_section(ui);
        let mut changed = false;
        let i18n = self.i18n.clone();
        let s = &mut self.settings;
        Self::section(ui, &i18n.t("set.units"), "", |ui| {
            ui.horizontal_wrapped(|ui| {
                changed |= ui.radio_value(&mut s.units.temperature, TempUnit::Celsius, "°C").changed();
                changed |= ui.radio_value(&mut s.units.temperature, TempUnit::Fahrenheit, "°F").changed();
                ui.separator();
                changed |= ui.radio_value(&mut s.units.data, DataUnit::Bytes, i18n.t("set.bytes")).changed();
                changed |= ui.radio_value(&mut s.units.data, DataUnit::Bits, i18n.t("set.bits")).changed();
                ui.separator();
                changed |= ui.radio_value(&mut s.units.cpu, CpuUnit::PercentOfMac, i18n.t("set.pct_of_mac")).changed();
                changed |= ui.radio_value(&mut s.units.cpu, CpuUnit::PerCore, i18n.t("set.per_core")).changed();
            });
        });
        Self::section(ui, &i18n.t("set.appearance"), "", |ui| {
            ui.horizontal_wrapped(|ui| {
                changed |= ui.radio_value(&mut s.theme, Theme::Monochrome, i18n.t("set.monochrome")).changed();
                changed |= ui.radio_value(&mut s.theme, Theme::Brand, i18n.t("set.brand")).changed();
                changed |= ui.radio_value(&mut s.theme, Theme::ColorBlind, i18n.t("set.colorblind")).changed();
                changed |= ui.radio_value(&mut s.theme, Theme::Nothing, i18n.t("set.nothing")).changed();
            });
            ui.horizontal_wrapped(|ui| {
                // The Nothing theme always sets numbers in Space Mono, so this choice would do nothing.
                if s.theme != Theme::Nothing {
                    changed |= ui.radio_value(&mut s.number_font, NumberFont::Proportional, i18n.t("set.proportional")).changed();
                    changed |= ui.radio_value(&mut s.number_font, NumberFont::Monospace, i18n.t("set.monospaced")).changed();
                }
                changed |= ui.checkbox(&mut s.compact, i18n.t("set.compact")).changed();
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(i18n.t("set.language")).weak());
                let partial = |code: &str| PARTIAL.iter().find(|(c, _)| *c == code).map(|(_, n)| i18n.tf("set.lang_partial", &[("name", n)]));
                let cur = match s.language.as_deref() {
                    None => i18n.t("set.lang_system"),
                    Some("en") => "English".to_string(),
                    Some("es") => "Español".to_string(),
                    Some(other) => partial(other).unwrap_or_else(|| other.to_string()),
                };
                egui::ComboBox::from_id_salt("lang").selected_text(cur).show_ui(ui, |ui| {
                    changed |= ui.selectable_value(&mut s.language, None, i18n.t("set.lang_system")).changed();
                    for (code, name) in [("en", "English"), ("es", "Español")] {
                        debug_assert!(COMPLETE.contains(&code));
                        changed |= ui.selectable_value(&mut s.language, Some(code.to_string()), name).changed();
                    }
                    // Incomplete translations stay selectable only for someone who already has one.
                    if let Some(code) = s.language.clone().filter(|c| PARTIAL.iter().any(|(p, _)| p == c)) {
                        let label = partial(&code).unwrap_or_else(|| code.clone());
                        changed |= ui.selectable_value(&mut s.language, Some(code), label).changed();
                    }
                });
            });
        });
        Self::section(ui, &i18n.t("set.menu_bar"), "", |ui| {
            let mut mv: Option<(usize, usize)> = None;
            let n = s.menubar.len();
            for (i, it) in s.menubar.iter_mut().enumerate() {
                if !it.metric.supported() {
                    continue;
                }
                if i > 0 {
                    Self::settings_divider(ui);
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    Self::settings_cell(ui, 140.0, |ui| changed |= ui.checkbox(&mut it.visible, i18n.t(it.metric.key())).changed());
                    Self::settings_cell(ui, 104.0, |ui| {
                        egui::ComboBox::from_id_salt(format!("disp{i}"))
                            .width(92.0)
                            .selected_text(i18n.t(match it.display {
                                MenuDisplay::Number => "set.number",
                                MenuDisplay::Sparkline => "set.graph",
                                MenuDisplay::Both => "set.both",
                            }))
                            .show_ui(ui, |ui| {
                                changed |= ui.selectable_value(&mut it.display, MenuDisplay::Number, i18n.t("set.number")).changed();
                                changed |= ui.selectable_value(&mut it.display, MenuDisplay::Sparkline, i18n.t("set.graph")).changed();
                                changed |= ui.selectable_value(&mut it.display, MenuDisplay::Both, i18n.t("set.both")).changed();
                            });
                    });
                    Self::settings_cell(ui, 84.0, |ui| changed |= ui.checkbox(&mut it.framed, i18n.t("set.framed")).changed());
                    Self::settings_cell(ui, 112.0, |ui| {
                        let mut has = it.threshold.is_some();
                        if ui.checkbox(&mut has, i18n.t("set.red_above")).changed() {
                            it.threshold = if has { Some(it.metric.default_threshold().unwrap_or(80.0)) } else { None };
                            changed = true;
                        }
                    });
                    Self::settings_cell(ui, 64.0, |ui| {
                        if let Some(t) = it.threshold.as_mut() {
                            changed |= ui.add(egui::DragValue::new(t).speed(1.0)).changed();
                        }
                    });
                    if let Some(m) = Self::move_buttons(ui, &i18n, i, n, &i18n.t(it.metric.key())) {
                        mv = Some(m);
                    }
                });
            }
            if let Some((a, b)) = mv {
                s.move_menubar_item(a, b);
                changed = true;
            }
        });
        Self::section(ui, &i18n.t("set.tabs"), "", |ui| {
            let mut mv: Option<(usize, usize)> = None;
            // Same groups as the sidebar; order only matters (and can only change) within a group.
            let mut first = true;
            for (g, idx) in sidebar::grouped_indices(&s.tabs, false) {
                if let Some(h) = g.header_key() {
                    ui.add_space(if first { 0.0 } else { 8.0 });
                    ui.label(RichText::new(i18n.t(h).to_uppercase()).size(10.5).strong().color(ui.visuals().weak_text_color()));
                }
                first = false;
                let n = idx.len();
                for (pos, &i) in idx.iter().enumerate() {
                    if pos > 0 {
                        Self::settings_divider(ui);
                    }
                    let tab = s.tabs[i].tab;
                    let label = i18n.t(tab.key());
                    ui.horizontal(|ui| {
                        Self::settings_cell(ui, 200.0, |ui| {
                            changed |= ui.checkbox(&mut s.tabs[i].visible, label.clone()).changed();
                        });
                        if tab != Tab::History
                            && let Some((a, b)) = Self::move_buttons(ui, &i18n, pos, n, &label)
                        {
                            mv = Some((idx[a], idx[b]));
                        }
                    });
                }
            }
            // Settings is always shown and pinned last, so it has no checkbox state or move buttons.
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                Self::settings_cell(ui, 200.0, |ui| {
                    ui.add_enabled(false, egui::Checkbox::new(&mut true, i18n.t(Tab::Settings.key())));
                });
            });
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
        Self::section(ui, &i18n.t("set.history_alerts"), "", |ui| {
            ui.horizontal(|ui| {
                ui.label(i18n.t("set.keep_history"));
                for (r, l) in [(Retention::Days30, "set.days30"), (Retention::Days90, "set.days90"), (Retention::Months6, "set.months6"), (Retention::Year1, "set.year1")] {
                    changed |= ui.radio_value(&mut s.retention, r, i18n.t(l)).changed();
                }
            });
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut s.notifications, i18n.t("set.notifications")).changed();
                ui.label(i18n.t("set.idle_after"));
                changed |= ui.add(egui::DragValue::new(&mut s.idle_threshold_min).range(5..=1440)).changed();
                ui.label(i18n.t("set.disk_alert"));
                let mut mb = s.alert_disk_write_bps / 1e6;
                if ui.add(egui::DragValue::new(&mut mb).range(1.0..=10_000.0)).changed() {
                    s.alert_disk_write_bps = mb * 1e6;
                    changed = true;
                }
                ui.label(i18n.t("set.net_alert"));
                let mut nb = s.alert_network_bps / 1e6;
                if ui.add(egui::DragValue::new(&mut nb).range(1.0..=10_000.0)).changed() {
                    s.alert_network_bps = nb * 1e6;
                    changed = true;
                }
            });
        });
        Self::section(ui, &i18n.t("set.ai_plans"), "", |ui| {
            let mut remove = None;
            for (i, p) in s.plans.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(format!("prov{i}")).selected_text(&p.provider).show_ui(ui, |ui| {
                        for pr in ["claude", "codex", "copilot", "cursor"] {
                            changed |= ui.selectable_value(&mut p.provider, pr.to_string(), pr).changed();
                        }
                    });
                    changed |= ui.add(egui::TextEdit::singleline(&mut p.plan).desired_width(90.0)).changed();
                    ui.label(i18n.t("set.window_h"));
                    changed |= ui.add(egui::DragValue::new(&mut p.window_hours).range(1.0..=744.0)).changed();
                    if p.provider == "claude" {
                        ui.label(i18n.t("set.token_budget"));
                        let mut b = p.budget.unwrap_or(0.0);
                        if ui.add(egui::DragValue::new(&mut b).speed(100_000.0)).changed() {
                            p.budget = (b > 0.0).then_some(b);
                            changed = true;
                        }
                    }
                    if p.provider != "codex" {
                        ui.label(i18n.t("set.manual_used"));
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
            if ui.button(i18n.t("set.add_plan")).clicked() {
                s.plans.push(PlanConfig { provider: "copilot".into(), plan: "Pro".into(), window_hours: 720.0, budget: None, manual_used_pct: None, manual_resets_at: None });
                changed = true;
            }
            ui.label(RichText::new(i18n.t("set.plans_note")).small().weak());
        });
        Self::section(ui, &self.i18n.t("kbd.title"), "", |ui| {
            egui::Grid::new("shortcuts").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                for (keys, what) in shortcuts::HELP {
                    ui.label(RichText::new(*keys).monospace().strong());
                    ui.label(self.i18n.t(what));
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new(self.i18n.t("kbd.note")).small().weak());
        });
        let mut copy = None;
        Self::section(ui, &i18n.t("set.mcp"), "", |ui| {
            changed |= ui.checkbox(&mut s.mcp_enabled, i18n.t("set.mcp_enable")).changed();
            let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or("pulsebar".into());
            let cmd = format!("claude mcp add pulsebar -- \"{exe}\" mcp");
            ui.horizontal(|ui| {
                ui.code(&cmd);
                if ui.small_button(i18n.t("copy")).clicked() {
                    copy = Some(cmd.clone());
                }
            });
            ui.label(RichText::new(i18n.t("set.mcp_note")).small().weak());
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
        let u = self.units();
        let i18n = self.i18n.clone();
        let Some(st) = self.inspector.as_mut() else { return };
        if let Some(rx) = &st.rx
            && let Ok(d) = rx.try_recv() {
                if d.is_some() {
                    st.detail = d;
                }
                st.rx = None;
            }
        let cores = snap.machine.cpu.core_count;
        let mut close = false;
        let mut quit = None;
        let mut toast = None;
        egui::SidePanel::right("inspector").default_width(420.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(i18n.t("insp.title"));
                if ui.button("✕").clicked() {
                    close = true;
                }
            });
            let Some(d) = st.detail.clone() else {
                ui.label(i18n.t("insp.exited"));
                return;
            };
            ui.horizontal(|ui| {
                if ui.button(i18n.t("copy")).clicked() {
                    ctx.copy_text(inspector::to_text_in(&i18n, &d, &u, cores));
                    toast = Some(i18n.t("insp.copied"));
                }
                if ui.button(i18n.t("insp.export_csv")).clicked() {
                    let p = paths::exports_dir().join(format!("pulsebar-process-{}.csv", d.identity.pid));
                    toast = Some(match std::fs::write(&p, inspector::to_csv(&d)) {
                        Ok(()) => i18n.tf("saved", &[("path", &p.display().to_string())]),
                        Err(e) => e.to_string(),
                    });
                }
                if ui.button(i18n.t("insp.open_terminal")).clicked() {
                    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or("pulsebar".into());
                    sys::open_in_terminal(&[exe, "process".to_string(), d.identity.pid.to_string()]);
                }
                if ui.button(self.i18n.t("quit") + "…").clicked() {
                    quit = Some(d.identity.pid);
                }
            });
            if st.rx.is_some() {
                ui.label(RichText::new(i18n.t("insp.checking")).weak());
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.monospace(inspector::to_text_in(&i18n, &d, &u, cores));
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
                title: plan.headline_in(&self.i18n),
                subject: None,
                lines: if force { vec![self.t("dialog.force_note")] } else { vec![] },
                chips: plan.details_in(&self.i18n),
                cancel: self.i18n.t("cancel"),
                confirm: if force { self.i18n.t("force_quit") } else { self.i18n.t("quit") },
                confirm_color: if force { pal.danger } else { pal.accent },
                enter_confirms: true,
            };
            match dialog::show(ctx, &d) {
                Some(true) => {
                    let shown = plan.clone();
                    let o = actions::execute(actions::confirm(plan, HumanYes::from_ui_click()));
                    let m = actions::outcome_message(&self.i18n, &shown, &o);
                    self.toast(m);
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
                title: self.t("dialog.assistant_asks"),
                subject: Some(req.plan.headline_in(&self.i18n)),
                lines: vec![self.tf("dialog.request_from", &[("source", &req.source)])],
                chips: req.plan.details_in(&self.i18n),
                cancel: self.t("deny"),
                confirm: self.t("allow"),
                confirm_color: if force { pal.danger } else { pal.accent },
                // Unlike the quit confirmation, this one appears unprompted: a stray Return
                // must never approve it.
                enter_confirms: false,
            };
            if let Some(yes) = dialog::show(ctx, &d) {
                let plan = req.plan.clone();
                let r = actions::resolve_pending(req, yes.then(HumanYes::from_ui_click));
                if let Ok(r) = r {
                    let m = r.outcome.map(|o| actions::outcome_message(&self.i18n, &plan, &o)).unwrap_or_else(|| self.t("dialog.denied"));
                    self.toast(m);
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
                ui.label(RichText::new(format!("⚠ {}", a.text(&self.i18n))).color(pal.danger));
                if !a.app_key.is_empty() && ui.small_button(self.i18n.t("alert.quit_now")).clicked() {
                    quit = Some(a.app_key.clone());
                    dismiss = Some(i);
                }
                if ui.small_button(self.i18n.t("alert.silence")).clicked() {
                    crate::daemon::snooze_alert(a, sys::now_s() + 86_400);
                    dismiss = Some(i);
                }
                if ui.small_button(self.i18n.t("alert.dismiss")).clicked() {
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
        self.handle_nav(ctx);
        // ⌘F focuses the search field of the current tab's app list.
        // Ignored while a dialog is open, like Esc in the search field.
        if ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::F))) && !self.dialog_open() {
            self.focus_search = true;
        }
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
                ui.centered_and_justified(|ui| ui.label(self.i18n.t("sampling")));
            });
            return;
        };

        if let Some(k) = self.quit_request.take() {
            self.ask(&snap, Target::App(k), Mode::Quit);
        }

                self.handle_shortcuts(ctx, &snap);
        // The list on screen refills this while it is drawn.
        self.list_rows.clear();

        self.sidebar(ctx, &snap);
        if !self.toasts.is_empty() {
            egui::TopBottomPanel::bottom("toasts").show(ctx, |ui| {
                let nothing_on = nothing::is_active(ui.ctx());
                for (_, t) in &self.toasts {
                    // The Nothing language has no toasts, only inline status text.
                    ui.label(if nothing_on { RichText::new(format!("[{t}]")).monospace().size(12.0) } else { RichText::new(t).strong() });
                }
            });
        }
        self.inspector_panel(ctx, &snap);
        self.detail_panel(ctx, &snap);
        egui::CentralPanel::default().show(ctx, |ui| {
            self.login_banner(ui);
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
        // Tabs without an app list ignore ⌘F; don't let the request linger into the next tab.
        self.focus_search = false;
        self.dialogs(ctx, &snap);
        self.scroll_to_sel = false;
        self.text_focus = ctx.output(|o| o.ime.is_some());
    }
}
