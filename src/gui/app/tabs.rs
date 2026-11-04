//! Resource and developer tabs, built from three shared pieces so every tab reads the same way:
//! a hero card (Now + side stats + area chart), a row of stat tiles, and a list (apps, devices,
//! sessions…). Each tab keeps its own extras below: per-core load, volumes, interfaces, fans…

use super::*;

/// Hero chart source: the last hour of 1-minute history, or the live 2-minute sparkline while
/// the agent hasn't recorded enough yet.
const HERO_RANGE_S: i64 = 3600;

pub(super) enum TileSub {
    Text(String),
    Chips(Vec<String>),
}

pub(super) struct Tile {
    pub glyph: &'static str,
    pub label: String,
    pub value: String,
    pub sub: TileSub,
    /// App icon shown before the value (Top App tile).
    pub bundle: Option<String>,
    /// Clicking highlights this app in the list below.
    pub app_key: Option<String>,
    pub value_color: Option<Color32>,
}

impl Tile {
    pub fn new(glyph: &'static str, label: impl Into<String>, value: impl Into<String>, sub: TileSub) -> Self {
        Tile { glyph, label: label.into(), value: value.into(), sub, bundle: None, app_key: None, value_color: None }
    }
    pub fn text(glyph: &'static str, label: impl Into<String>, value: impl Into<String>, sub: impl Into<String>) -> Self {
        Self::new(glyph, label, value, TileSub::Text(sub.into()))
    }
    fn colored(mut self, c: Option<Color32>) -> Self {
        self.value_color = c;
        self
    }
}

pub(super) struct Hero<'a> {
    pub kind: MetricKind,
    /// History metric name ("cpu", "memory"…).
    pub metric: &'a str,
    pub caption: String,
    pub value: String,
    pub badge: Option<(String, Color32)>,
    pub rows: Vec<(String, String)>,
    pub max: Option<f64>,
    pub fmt: &'a dyn Fn(f64) -> String,
}

fn pct(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.0}%")).unwrap_or("—".into())
}

/// Sub-list layout: process names start under the app name; the four numeric columns
/// (PID, CPU, Memory, Energy) are right-aligned at these x positions; the row actions
/// (Inspect, Quit) live in the reserved strip at the far right.
const SUB_INDENT: f32 = 46.0;
const SUB_ACTIONS_W: f32 = 58.0;

fn sub_columns(inner: Rect) -> [f32; 4] {
    let energy = inner.right() - SUB_ACTIONS_W - 8.0;
    let memory = energy - 84.0;
    let cpu = memory - 84.0;
    let pid = cpu - 64.0;
    [pid, cpu, memory, energy]
}

/// Width reserved for each optional CPU / Memory column in the app list.
const EXTRA_COL_W: f32 = 92.0;

fn meter_width(row_inner_width: f32) -> f32 {
    240.0_f32.min(row_inner_width * 0.3)
}

/// Right edge of optional column `i` of `n`, left of the meter; the last one sits next to it.
fn extra_col_right(inner_right: f32, meter_w: f32, n: usize, i: usize) -> f32 {
    inner_right - meter_w - 24.0 - (n - 1 - i) as f32 * EXTRA_COL_W
}

/// A clickable column header: `pos` is the left edge (left-aligned) or the right edge
/// (right-aligned) of the text. The active column is emphasised and shows ⏶/⏷ for the direction.
fn sort_header(ui: &mut egui::Ui, id: impl std::hash::Hash, pos: egui::Pos2, right: bool, label: &str, key: SortKey, spec: SortSpec) -> bool {
    let arrow = spec.arrow(key);
    let text = match (arrow.is_empty(), right) {
        (true, _) => label.to_string(),
        (false, true) => format!("{arrow} {label}"),
        (false, false) => format!("{label} {arrow}"),
    };
    let active = spec.key == key;
    let galley = ui.painter().layout_no_wrap(text, egui::FontId::proportional(11.5), Color32::PLACEHOLDER);
    let anchor = if right { egui::Align2::RIGHT_CENTER } else { egui::Align2::LEFT_CENTER };
    let rect = anchor.anchor_size(pos, galley.size());
    // CLICK, not click(): a focusable header would keep focus after a mouse click and silently
    // pause the list keys (see `shortcuts::allowed`), like focusable rows would.
    let resp = ui.interact(rect.expand2(Vec2::new(4.0, 4.0)), ui.id().with(id), egui::Sense::CLICK);
    let color = if active || resp.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
    ui.painter().galley(rect.min, galley, color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// Below this list width the optional CPU / Memory columns are not painted.
const MIN_WIDTH_FOR_EXTRAS: f32 = 560.0;

/// Text of the last numeric cell of a process row: Disk / Network rate, else Energy.
fn last_proc_cell(i18n: &I18n, p: &Process, key: SortKey, u: &crate::units::Units) -> String {
    match key {
        SortKey::Disk => u.rate(p.disk_read_bps + p.disk_write_bps),
        SortKey::Network => u.rate(p.net_rx_bps + p.net_tx_bps),
        _ => watts(i18n, Some(p.energy_w)),
    }
}

fn watts(i18n: &I18n, v: Option<f64>) -> String {
    v.map(|w| if w < 1.0 { format!("{:.0} mW", w * 1000.0) } else { i18n.num(format!("{w:.1} W")) }).unwrap_or("—".into())
}

impl PulseApp {
    fn color(&self, ui: &egui::Ui, k: MetricKind) -> Color32 {
        widgets::metric_color(self.settings.theme, ui.visuals().dark_mode, k)
    }

    /// Last hour of history for a machine metric, cached for 15 s.
    fn hero_series(&mut self, metric: &str) -> Vec<f64> {
        let key = format!("hero:{metric}");
        if let Some((t, v)) = self.series_cache.get(&key)
            && t.elapsed() < Duration::from_secs(15) {
                return v.clone();
            }
        let now = sys::now_s();
        let v: Vec<f64> = self
            .shared
            .history
            .lock()
            .ok()
            .and_then(|h| h.as_ref().and_then(|h| h.series(metric, None, now - HERO_RANGE_S, now, now).ok()))
            .map(|rows| rows.iter().map(|b| b.value_avg).collect())
            .unwrap_or_default();
        self.series_cache.insert(key, (Instant::now(), v.clone()));
        v
    }

    // ---- shared pieces ------------------------------------------------------------------------

    pub(super) fn hero(&mut self, ui: &mut egui::Ui, h: Hero) {
        let color = self.color(ui, h.kind);
        let stats = self.today_stats(h.metric);
        let history = self.hero_series(h.metric);
        let live = self.shared.sparks.read().unwrap().get(h.kind);
        let (series, range_label) = if history.len() >= 10 { (history, self.t("hero.last_hour")) } else { (live, self.t("hero.last_2min")) };
        let mut rows = vec![
            (self.i18n.t("avg_today"), stats.map(|s| (h.fmt)(s.0)).unwrap_or("—".into())),
            (self.i18n.t("peak"), stats.map(|s| (h.fmt)(s.1)).unwrap_or("—".into())),
        ];
        rows.extend(h.rows);

        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 230.0), egui::Sense::hover());
        let dark = ui.visuals().dark_mode;
        ui.painter().rect_filled(rect, 16.0, widgets::card_fill(dark));
        let inner = rect.shrink2(Vec2::new(22.0, 20.0));
        let left_w = 250.0_f32.min(inner.width() * 0.35);
        let left = Rect::from_min_size(inner.min, Vec2::new(left_w, inner.height()));
        let chart = Rect::from_min_max(egui::pos2(left.right() + 24.0, inner.top() + 8.0), inner.max);

        ui.new_child(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::top_down(Align::Min))).scope(|ui| {
            ui.style_mut().interaction.selectable_labels = false;
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.label(RichText::new(&h.caption).size(13.0).weak());
            let (num, unit) = split_unit(&h.value);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.with_layout(egui::Layout::left_to_right(Align::Max), |ui| {
                    ui.label(widgets::num(&self.settings, num, 46.0).strong().color(ui.visuals().strong_text_color()));
                    if !unit.is_empty() {
                        ui.label(RichText::new(unit).size(20.0).weak());
                    }
                });
            });
            if let Some((text, c)) = &h.badge {
                widgets::pill(ui, text, *c);
            }
            // Side stats sit at the bottom of the column.
            let row_h = 22.0;
            let top = left.bottom() - row_h * rows.len() as f32;
            for (i, (k, v)) in rows.iter().enumerate() {
                let r = Rect::from_min_size(egui::pos2(left.left(), top + i as f32 * row_h), Vec2::new(left_w, row_h));
                let p = ui.painter();
                p.text(r.left_center(), egui::Align2::LEFT_CENTER, k, egui::FontId::proportional(13.0), ui.visuals().weak_text_color());
                p.text(r.right_center(), egui::Align2::RIGHT_CENTER, v, egui::FontId::proportional(13.5), ui.visuals().strong_text_color());
            }
        });
        let resp = ui.interact(chart, ui.id().with(("hero", h.metric)), egui::Sense::hover());
        widgets::area_chart(ui.painter(), chart, &series, color, h.max, ui.visuals().weak_text_color(), &self.t("collecting"));
        ui.painter().text(chart.right_top() + Vec2::new(0.0, -6.0), egui::Align2::RIGHT_BOTTOM, &range_label, egui::FontId::proportional(11.0), ui.visuals().weak_text_color());
        if let Some(pos) = resp.hover_pos()
            && series.len() >= 2 {
                let i = (((pos.x - chart.left()) / chart.width()).clamp(0.0, 1.0) * (series.len() - 1) as f32).round() as usize;
                resp.on_hover_text((h.fmt)(series[i]));
            }
        ui.add_space(14.0);
    }

    pub(super) fn tiles(&mut self, ui: &mut egui::Ui, kind: MetricKind, tiles: Vec<Tile>) {
        if tiles.is_empty() {
            return;
        }
        let color = self.color(ui, kind);
        let gap = 14.0;
        let n = tiles.len() as f32;
        let w = ((ui.available_width() - gap * (n - 1.0)) / n).floor();
        let mut clicked: Option<String> = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for t in &tiles {
                let (resp, inner) = Self::card_frame(ui, Vec2::new(w, 120.0));
                ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(Align::Min))).scope(|ui| {
                    ui.style_mut().interaction.selectable_labels = false;
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.horizontal(|ui| {
                        widgets::icon_badge(ui, t.glyph, color, 24.0);
                        ui.add_space(2.0);
                        ui.add(egui::Label::new(RichText::new(&t.label).size(14.0)).truncate());
                    });
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if let Some(b) = t.bundle.as_deref()
                            && let Some(tex) = self.icons.get(ui.ctx(), Some(b)) {
                                ui.image((tex, Vec2::splat(24.0)));
                            }
                        let size = if t.bundle.is_some() || t.app_key.is_some() { 17.0 } else { 24.0 };
                        let c = t.value_color.unwrap_or(ui.visuals().strong_text_color());
                        ui.add(egui::Label::new(widgets::num(&self.settings, t.value.clone(), size).strong().color(c)).truncate());
                    });
                    match &t.sub {
                        TileSub::Text(s) => {
                            ui.add(egui::Label::new(RichText::new(s).size(13.0).weak()).truncate());
                        }
                        TileSub::Chips(chips) => {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 5.0;
                                for c in chips {
                                    widgets::chip(ui, c, color);
                                }
                            });
                        }
                    }
                });
                if t.app_key.is_some() {
                    ui.painter().text(inner.right_bottom(), egui::Align2::RIGHT_BOTTOM, "›", egui::FontId::proportional(16.0), ui.visuals().weak_text_color());
                    if resp.clicked() {
                        clicked = t.app_key.clone();
                    }
                }
            }
        });
        if let Some(k) = clicked {
            self.highlight = Some(k);
            self.scroll_to_highlight = true;
        }
        ui.add_space(14.0);
    }

    /// Tile for the busiest app by `m`; click scrolls the list to it.
    pub(super) fn top_app_tile(&self, snap: &Snapshot, m: RankMetric, label: &str) -> Tile {
        let u = self.units();
        let label = self.t(label);
        match snap.top_apps(m, 1).into_iter().next() {
            Some(a) => Tile {
                glyph: "⊞",
                label,
                value: a.app.name.clone(),
                sub: TileSub::Text(output::metric_value(a, m, &u, snap.machine.cpu.core_count)),
                bundle: a.app.bundle_path.clone(),
                app_key: Some(a.key.clone()),
                value_color: None,
            },
            None => Tile::text("⊞", label, "—", ""),
        }
    }

    fn app_icon(&mut self, ui: &mut egui::Ui, bundle: Option<&str>, name: &str, size: f32) {
        if let Some(t) = self.icons.get(ui.ctx(), bundle) {
            ui.image((t, Vec2::splat(size)));
        } else {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
            let c = if ui.visuals().dark_mode { Color32::from_gray(60) } else { Color32::from_gray(200) };
            ui.painter().rect_filled(r, size * 0.25, c);
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, name.chars().next().unwrap_or('?'), egui::FontId::proportional(size * 0.5), ui.visuals().strong_text_color());
        }
    }

    /// Section card with a header row: title on the left, an optional right-hand label.
    pub(super) fn section<R>(ui: &mut egui::Ui, title: &str, right: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        let r = widgets::card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(14.0).weak());
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(right).size(14.0).weak()));
            });
            ui.add_space(4.0);
            add(ui)
        });
        ui.add_space(14.0);
        r
    }

    /// App list: icon, name, process count, value and a thin meter. Hover shows Quit / Force
    /// Quit; a click expands the processes. Both quits go through the confirmation dialog.
    pub(super) fn app_list(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>, m: RankMetric, n: usize, col: &str) {
        let u = self.units();
        let cores = snap.machine.cpu.core_count;
        let color = self.color(ui, match m {
            RankMetric::Cpu => MetricKind::Cpu,
            RankMetric::Memory => MetricKind::Memory,
            RankMetric::Disk => MetricKind::Disk,
            RankMetric::Network => MetricKind::Network,
            _ => MetricKind::Battery,
        });
        let mut action: Option<(Target, Mode)> = None;
        let mut inspect: Option<i32> = None;
        let mut export: Option<bool> = None;
        let (m_key, ls) = (SortKey::from_metric(m), self.settings.list_sort.get(&self.tab).cloned().unwrap_or_default());
        let app_sort = ls.apps.unwrap_or(SortSpec::for_metric(m));
        let proc_sort = ls.procs.unwrap_or(SortSpec::for_metric(m));
        // CPU and Memory side by side with the tab's own metric (which is always shown).
        // Not painted (header and rows alike) when the list is too narrow for the name to fit.
        let fits = ui.available_width() >= MIN_WIDTH_FOR_EXTRAS;
        let extras: Vec<SortKey> = if ls.extra_columns && fits { [SortKey::Cpu, SortKey::Memory].into_iter().filter(|k| *k != m_key).collect() } else { vec![] };
        // Disk and Network lists are ranked by their own metric, so their process lists show it
        // in place of Energy (otherwise the default order could not be selected again).
        let last_proc_key = if matches!(m, RankMetric::Disk | RankMetric::Network) { m_key } else { SortKey::Energy };
        let last_proc_label = match last_proc_key {
            SortKey::Disk => self.i18n.t("tab.disk"),
            SortKey::Network => self.i18n.t("tab.network"),
            _ => self.i18n.t("col.energy"),
        };
        let mut app_click: Option<SortKey> = None;
        let mut proc_click: Option<SortKey> = None;
        let mut toggle_extras = false;
        let mut shown_apps: Vec<String> = vec![];
        let mut shown_procs: HashMap<String, Vec<i32>> = HashMap::new();
        let danger = self.pal(ui.ctx()).danger;
        let card_fill = widgets::card_fill(ui.visuals().dark_mode);
        // Search (⌘F): one pass over the snapshot per frame; the query lives per tab.
        let query = self.search_field(ui);
        // The frozen order only applies to the sort and the query it was captured under.
        let frozen_ok = self.frozen.as_ref().is_some_and(|f| f.applies_to(m, app_sort, proc_sort) && f.query == query);
        // While the pointer is over the list the matches stay frozen with the order (same query).
        let frozen_matches = self.frozen.as_mut().filter(|f| f.metric == m && f.query == query).and_then(|f| f.matches.take());
        let matches = frozen_matches.or_else(|| crate::search::Matcher::new(&query, snap).map(|mt| crate::search::filter(snap, &mt)));
        let card = egui::Frame::new().fill(card_fill).corner_radius(16.0).inner_margin(egui::Margin::symmetric(10, 14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(self.t("mem.app")).size(14.0).weak());
                let summary = match &matches {
                    Some(mt) => format!("· {}", self.tf("search.apps_count", &[("n", &mt.apps.len().to_string())])),
                    None => self.tf("applist.summary", &[("procs", &self.i18n.tn("n.processes", snap.processes.len(), &[])), ("apps", &self.i18n.tn("n.apps", snap.apps.len(), &[]))]),
                };
                ui.label(RichText::new(summary).size(12.0).weak());
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(10.0);
                    if ui.add(egui::Button::new(RichText::new(self.t("share.dark")).size(12.0)).frame(false)).on_hover_text(self.t("share.dark_tip")).clicked() {
                        export = Some(true);
                    }
                    if ui.add(egui::Button::new(RichText::new(self.t("share.light")).size(12.0)).frame(false)).on_hover_text(self.t("share.light_tip")).clicked() {
                        export = Some(false);
                    }
                    let mark = if ls.extra_columns { "☑" } else { "☐" };
                    if ui.add(egui::Button::new(RichText::new(format!("{mark} {}", self.i18n.t("col.cpu_mem"))).size(12.0)).frame(false)).clicked() {
                        toggle_extras = true;
                    }
                });
            });
            // Column headers of the app list; positions match the painted row cells below.
            let (hdr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), egui::Sense::hover());
            let hin = hdr.shrink2(Vec2::new(14.0, 0.0));
            let hmeter_w = meter_width(hin.width());
            let cy = hin.center().y;
            if sort_header(ui, "app-hdr-name", egui::pos2(hin.left() + 40.0, cy), false, &self.i18n.t("col.name"), SortKey::Name, app_sort) {
                app_click = Some(SortKey::Name);
            }
            for (i, k) in extras.iter().enumerate() {
                let x = extra_col_right(hin.right(), hmeter_w, extras.len(), i);
                let label = if *k == SortKey::Cpu { self.i18n.t("tab.cpu") } else { self.i18n.t("tab.memory") };
                if sort_header(ui, ("app-hdr", *k), egui::pos2(x, cy), true, &label, *k, app_sort) {
                    app_click = Some(*k);
                }
            }
            if sort_header(ui, "app-hdr-metric", egui::pos2(hin.right(), cy), true, &self.t(col), m_key, app_sort) {
                app_click = Some(m_key);
            }
            ui.painter().hline(hin.x_range(), hdr.bottom(), egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6)));
            ui.add_space(4.0);
            // While the pointer is over the list (or after keyboard navigation), keep last frame's
            // order: values keep updating, rows don't move. New apps appear once the pointer leaves.
            // The list is the top `n` apps by the chosen metric column (the tab's metric when
            // sorting by name or PID), ordered by that column. A search shows every match (not
            // just the top `n`); a frozen order captured under another query does not apply.
            let mut top: Vec<&AppGroup> = match self.frozen.as_ref().filter(|_| frozen_ok) {
                Some(f) => sorting::resolve_frozen_apps(&f.apps, &snap.apps),
                None => {
                    let mut v = snap.top_apps(app_sort.key.rank_metric().unwrap_or(m), if matches.is_some() { usize::MAX } else { n });
                    sorting::sort_apps(&mut v, app_sort);
                    v
                }
            };
            if let Some(mt) = &matches {
                top.retain(|a| mt.apps.contains_key(&a.key));
                if top.is_empty() {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(10.0);
                        ui.label(RichText::new(self.i18n.t("search.no_matches")).weak());
                    });
                    ui.add_space(8.0);
                }
            }
            let max = top.iter().map(|a| a.metric(m)).fold(0.0, f64::max).max(1e-9);
            let hover_fill = if ui.visuals().dark_mode { Color32::from_rgb(38, 38, 42) } else { Color32::from_rgb(244, 244, 247) };
            for a in top {
                shown_apps.push(a.key.clone());
                let highlighted = self.highlight.as_deref() == Some(a.key.as_str());
                // The focus ring marks the one selected row: the app itself, or one of its processes.
                let selected_app = highlighted && self.sel_proc.is_none();
                // An app that matched only through some of its processes opens to show them.
                let hit = matches.as_ref().and_then(|mt| mt.apps.get(&a.key));
                let process_hits = matches!(hit, Some(crate::search::AppHit::Processes(_)));
                let open = process_hits || *self.expanded.get(&a.key).unwrap_or(&false);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 58.0), egui::Sense::CLICK);
                // contains_pointer, not hovered: hovered() turns false while the pointer is on the
                // row's own Quit buttons, which would hide them under the cursor.
                let over_row = resp.contains_pointer();
                if highlighted || over_row || open {
                    ui.painter().rect_filled(rect, 12.0, hover_fill);
                }
                if selected_app {
                    ui.painter().rect_stroke(rect.shrink(1.0), 12.0, egui::Stroke::new(2.0, color), egui::StrokeKind::Inside);
                    if self.scroll_to_sel {
                        ui.scroll_to_rect(rect, None);
                        self.scroll_to_sel = false;
                    }
                }
                resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected_app, a.app.name.clone()));
                let inner = rect.shrink2(Vec2::new(14.0, 8.0));
                let meter_w = meter_width(inner.width());
                // Value and meter are painted at fixed positions on the right so every row lines up.
                let value = output::metric_value(a, m, &u, cores);
                let strong = ui.visuals().strong_text_color();
                ui.painter().text(egui::pos2(inner.right(), inner.top() + 10.0), egui::Align2::RIGHT_CENTER, &value, egui::FontId::proportional(15.0), strong);
                for (i, k) in extras.iter().enumerate() {
                    let text = if *k == SortKey::Cpu { u.cpu(a.cpu_pct, cores) } else { u.bytes(a.mem_bytes) };
                    let x = extra_col_right(inner.right(), meter_w, extras.len(), i);
                    ui.painter().text(egui::pos2(x, inner.top() + 10.0), egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(14.0), ui.visuals().text_color());
                }
                let mrect = Rect::from_min_size(egui::pos2(inner.right() - meter_w, inner.bottom() - 9.0), Vec2::new(meter_w, 5.0));
                ui.painter().rect_filled(mrect, 2.5, ui.visuals().extreme_bg_color);
                let frac = (a.metric(m) / max).clamp(0.0, 1.0) as f32;
                if frac > 0.0 {
                    ui.painter().rect_filled(Rect::from_min_size(mrect.min, Vec2::new((meter_w * frac).max(3.0), 5.0)), 2.5, color);
                }
                let left = Rect::from_min_max(inner.min, egui::pos2(inner.right() - meter_w - 16.0 - extras.len() as f32 * EXTRA_COL_W, inner.bottom()));
                ui.new_child(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::left_to_right(Align::Center))).scope(|ui| {
                    ui.style_mut().interaction.selectable_labels = false;
                    self.app_icon(ui, a.app.bundle_path.as_deref(), &a.app.name, 32.0);
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        let name = RichText::new(&a.app.name).size(15.0).strong();
                        ui.add(egui::Label::new(if highlighted { name.color(color) } else { name.color(strong) }).truncate());
                        let procs = match (process_hits, &matches) {
                            (true, Some(mt)) => {
                                let hits = a.pids.iter().filter(|p| mt.pids.contains(p)).count();
                                self.i18n.tn("search.procs_of", a.process_count, &[("hits", &hits.to_string())])
                            }
                            _ => self.i18n.tn("n.processes", a.process_count, &[]),
                        };
                        ui.label(RichText::new(procs).size(12.5).weak());
                    });
                    // Groups opened by a search show only some processes: no standing whole-app
                    // buttons there, just on hover.
                    if (over_row || highlighted || (open && !process_hits)) && a.key != crate::grouping::SYSTEM_KEY {
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            if widgets::icon_button(ui, ActionIcon::ForceQuit, danger, &(self.i18n.t("force_quit") + "…")).clicked() {
                                action = Some((Target::App(a.key.clone()), Mode::ForceQuit));
                            }
                            if widgets::icon_button(ui, ActionIcon::Quit, strong, &(self.i18n.t("quit") + "…")).clicked() {
                                action = Some((Target::App(a.key.clone()), Mode::Quit));
                            }
                        });
                    }
                });
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !process_hits {
                    self.expanded.insert(a.key.clone(), !open);
                    self.highlight = Some(a.key.clone());
                    self.sel_proc = None;
                }
                if highlighted && self.scroll_to_highlight {
                    ui.scroll_to_rect(rect, Some(Align::Center));
                    self.scroll_to_highlight = false;
                    self.expanded.insert(a.key.clone(), true);
                }
                if open {
                    let mut procs: Vec<&Process> = a.pids.iter().filter_map(|p| snap.process(*p)).collect();
                    if let (true, Some(mt)) = (process_hits, &matches) {
                        procs.retain(|p| mt.pids.contains(&p.pid));
                    }
                    let frozen_pids = self.frozen.as_ref().filter(|_| frozen_ok).and_then(|f| f.procs.get(&a.key));
                    match frozen_pids {
                        Some(order) => sorting::follow_frozen(&mut procs, order),
                        None => sorting::sort_procs(&mut procs, proc_sort),
                    }
                    shown_procs.insert(a.key.clone(), procs.iter().take(200).map(|p| p.pid).collect());
                    // Painter-drawn sub-list: rows span the same width as the app rows, numeric
                    // columns are right-aligned at fixed offsets from the right edge, and the
                    // row actions show only while the pointer is over the row.
                    let weak = ui.visuals().weak_text_color();
                    let text_col = ui.visuals().text_color();
                    let (hdr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), egui::Sense::hover());
                    let hin = hdr.shrink2(Vec2::new(14.0, 0.0));
                    let cols = sub_columns(hin);
                    let hy = hin.center().y + 2.0;
                    if sort_header(ui, ("proc-hdr-name", &a.key), egui::pos2(hin.left() + SUB_INDENT, hy), false, &self.i18n.t("col.process"), SortKey::Name, proc_sort) {
                        proc_click = Some(SortKey::Name);
                    }
                    let proc_cols = [(SortKey::Pid, self.i18n.t("col.pid")), (SortKey::Cpu, self.i18n.t("tab.cpu")), (SortKey::Memory, self.i18n.t("tab.memory")), (last_proc_key, last_proc_label.clone())];
                    for (x, (key, label)) in cols.iter().zip(proc_cols) {
                        if sort_header(ui, ("proc-hdr", &a.key, key), egui::pos2(*x, hy), true, &label, key, proc_sort) {
                            proc_click = Some(key);
                        }
                    }
                    ui.painter().hline(hin.x_range(), hdr.bottom(), egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6)));
                    let (quit_tip, inspect_tip) = (self.i18n.t("quit") + "…", self.i18n.t("inspect"));
                    let font = egui::FontId::proportional(13.5);
                    for p in procs.iter().take(200) {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), egui::Sense::CLICK);
                        let selected = highlighted && self.sel_proc == Some(p.pid);
                        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, p.name.clone()));
                        if resp.clicked() {
                            self.highlight = Some(a.key.clone());
                            self.sel_proc = Some(p.pid);
                        }
                        // contains_pointer, not hovered: hovered() is false while the pointer is on
                        // the row's own buttons, which would hide them under the cursor.
                        let over = resp.contains_pointer();
                        if matches.as_ref().is_some_and(|mt| mt.pids.contains(&p.pid)) {
                            ui.painter().rect_filled(rect, 10.0, color.gamma_multiply(0.22));
                        } else if over || selected {
                            ui.painter().rect_filled(rect, 10.0, hover_fill);
                        }
                        if selected {
                            ui.painter().rect_stroke(rect.shrink(1.0), 10.0, egui::Stroke::new(2.0, color), egui::StrokeKind::Inside);
                            if self.scroll_to_sel {
                                ui.scroll_to_rect(rect, None);
                                self.scroll_to_sel = false;
                            }
                        }
                        let inner = rect.shrink2(Vec2::new(14.0, 0.0));
                        let cols = sub_columns(inner);
                        let cy = inner.center().y;
                        let name_x = inner.left() + SUB_INDENT;
                        let mut job = egui::text::LayoutJob::simple_singleline(p.name.clone(), font.clone(), text_col);
                        job.wrap = egui::text::TextWrapping { max_width: (cols[0] - 40.0 - name_x).max(20.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
                        let galley = ui.painter().layout_job(job);
                        ui.painter().galley(egui::pos2(name_x, cy - galley.size().y / 2.0), galley, text_col);
                        let cells = [p.pid.to_string(), u.cpu(p.cpu_pct, cores), u.bytes(p.mem_bytes), last_proc_cell(&self.i18n, p, last_proc_key, &u)];
                        for (i, (x, text)) in cols.iter().zip(cells).enumerate() {
                            ui.painter().text(egui::pos2(*x, cy), egui::Align2::RIGHT_CENTER, text, font.clone(), if i == 0 { weak } else { text_col });
                        }
                        if over || selected {
                            let actions = Rect::from_min_max(egui::pos2(inner.right() - SUB_ACTIONS_W, rect.top()), egui::pos2(inner.right(), rect.bottom()));
                            ui.new_child(egui::UiBuilder::new().id_salt(("proc-actions", p.pid)).max_rect(actions).layout(egui::Layout::right_to_left(Align::Center))).scope(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                if widgets::icon_button(ui, ActionIcon::Quit, strong, &quit_tip).clicked() {
                                    action = Some((Target::Pid(p.pid), Mode::Quit));
                                }
                                if widgets::icon_button(ui, ActionIcon::Inspect, strong, &inspect_tip).clicked() {
                                    inspect = Some(p.pid);
                                }
                            });
                        }
                    }
                    if procs.len() > 200 {
                        ui.horizontal(|ui| {
                            ui.add_space(14.0 + SUB_INDENT);
                            ui.label(RichText::new(self.tf("search.more", &[("n", &(procs.len() - 200).to_string())])).size(12.5).weak());
                        });
                    }
                    ui.add_space(6.0);
                }
            }
        });
        // A selected process that is gone (or never belonged to the highlighted app) falls back to the app.
        self.sel_proc = shortcuts::keep_sel_proc(self.highlight.as_deref(), self.sel_proc, &shown_procs);
        // Remember what is on screen so ↑ ↓ → ← can move through it.
        self.list_rows = shown_apps
            .iter()
            .flat_map(|k| {
                let procs = shown_procs.get(k).into_iter().flatten().map(|pid| Row::Proc { app: k.clone(), pid: *pid });
                std::iter::once(Row::App(k.clone())).chain(procs)
            })
            .collect();
        // A confirmation dialog on top of the list doesn't count as hovering it.
        let over_list = (ui.rect_contains_pointer(card.response.rect) || self.key_nav_active()) && self.confirm.is_none() && action.is_none();
        self.frozen = over_list.then_some(FrozenOrder { metric: m, apps: shown_apps, procs: shown_procs, query: query.clone(), matches, apps_sort: app_sort, procs_sort: proc_sort });
        // Header clicks are explicit: persist the choice. A changed sort invalidates the frozen
        // order by itself (`FrozenOrder::applies_to`), so the next frame re-sorts immediately.
        if app_click.is_some() || proc_click.is_some() || toggle_extras {
            let entry = self.settings.list_sort.entry(self.tab).or_default();
            if let Some(k) = app_click {
                entry.apps = Some(app_sort.clicked(k));
            }
            if let Some(k) = proc_click {
                entry.procs = Some(proc_sort.clicked(k));
            }
            if toggle_extras {
                entry.extra_columns = !entry.extra_columns;
                // Hiding the CPU/Memory columns must not leave the list sorted by one of them.
                if !entry.extra_columns && entry.apps.is_some_and(|s| s.key != SortKey::Name && s.key != m_key) {
                    entry.apps = None;
                }
            }
            self.save_settings(ui.ctx());
            ui.ctx().request_repaint();
        }
        ui.add_space(14.0);
        if let Some(dark) = export {
            let card = crate::cli::card_for_in(&self.i18n, &u, snap, m, dark);
            match share_card::export(&card, &paths::exports_dir(), &format!("pulsebar-{}", m.as_str())) {
                Ok(p) => {
                    let m = self.tf("saved", &[("path", &p.display().to_string())]);
                    self.toast(m)
                }
                Err(e) => {
                    let m = self.tf("export_failed", &[("err", &e.to_string())]);
                    self.toast(m)
                }
            }
        }
        if let Some((t, md)) = action {
            self.ask(snap, t, md);
        }
        if let Some(pid) = inspect {
            self.open_inspector(snap, pid);
        }
    }

    /// The search field above an app list. Returns the current query (stored per tab). ⌘F focuses
    /// it; Esc clears it (unless a dialog is open).
    fn search_field(&mut self, ui: &mut egui::Ui) -> String {
        let tab = self.tab;
        let mut query = self.search.get(&tab).cloned().unwrap_or_default();
        let hint = self.i18n.t("search.placeholder");
        ui.horizontal(|ui| {
            let clear_w = if query.is_empty() { 0.0 } else { 28.0 };
            // Match the cards: rounded, card-coloured field.
            let fill = widgets::card_fill(ui.visuals().dark_mode);
            ui.visuals_mut().extreme_bg_color = fill;
            let wd = &mut ui.visuals_mut().widgets;
            for w in [&mut wd.inactive, &mut wd.hovered, &mut wd.active] {
                w.corner_radius = 10.0.into();
            }
            let edit = egui::TextEdit::singleline(&mut query)
                .id(egui::Id::new(("app-search", tab)))
                .hint_text(hint)
                .margin(egui::Margin::symmetric(10, 6))
                .desired_width(ui.available_width() - clear_w - if clear_w > 0.0 { ui.spacing().item_spacing.x } else { 0.0 });
            let resp = ui.add(edit);
            if std::mem::take(&mut self.focus_search) {
                resp.request_focus();
                resp.scroll_to_me(None);
            }
            // Esc is a global "clear search" unless a dialog is open (it denies/cancels there); the
            // field must not keep receiving keys behind a dialog.
            if self.dialog_open() {
                if resp.has_focus() {
                    resp.surrender_focus();
                }
            } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                query.clear();
                resp.surrender_focus();
            }
            if !query.is_empty() && ui.add_sized([clear_w, resp.rect.height()], egui::Button::new("✕").frame(false)).clicked() {
                query.clear();
            }
        });
        ui.add_space(6.0);
        if query.is_empty() {
            self.search.remove(&tab);
        } else {
            self.search.insert(tab, query.clone());
        }
        query
    }

    /// One list row inside a section: leading glyph/icon, title + subtitle, trailing widgets.
    pub(super) fn row(&self, ui: &mut egui::Ui, lead: &str, lead_color: Color32, title: &str, sub: &str, trailing: impl FnOnce(&mut egui::Ui)) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), egui::Sense::hover());
        ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(4.0, 6.0))).layout(egui::Layout::left_to_right(Align::Center))).scope(|ui| {
            ui.style_mut().interaction.selectable_labels = false;
            widgets::icon_badge(ui, lead, lead_color, 30.0);
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(egui::Label::new(RichText::new(title).size(14.5).strong().color(ui.visuals().strong_text_color())).truncate());
                if !sub.is_empty() {
                    ui.add(egui::Label::new(RichText::new(sub).size(12.5).weak()).truncate());
                }
            });
            ui.with_layout(egui::Layout::right_to_left(Align::Center), trailing);
        });
    }

    // ---- tabs -----------------------------------------------------------------------------------

    pub(super) fn cpu(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let c = snap.machine.cpu.clone();
        let u = self.units();
        let cores = c.core_count;
        let fmt = move |v: f64| u.cpu(v, cores);
        self.hero(ui, Hero {
            kind: MetricKind::Cpu,
            metric: "cpu",
            caption: self.i18n.t("now"),
            value: u.cpu(c.total_pct, cores),
            badge: None,
            rows: vec![(self.t("load"), self.i18n.num(format!("{:.2}", c.load_avg[0])))],
            max: Some(100.0),
            fmt: &fmt,
        });
        let chips = match sys::perf_levels() {
            Some((p, e)) if e > 0 => TileSub::Chips(vec![format!("{p} P"), format!("{e} E")]),
            _ => TileSub::Text(self.tf("tray.load", &[("a", &self.i18n.num(format!("{:.2}", c.load_avg[0]))), ("b", &self.i18n.num(format!("{:.2}", c.load_avg[1]))), ("c", &self.i18n.num(format!("{:.2}", c.load_avg[2])))])),
        };
        let tiles = vec![
            Tile::text("▣", self.t("st.user"), u.cpu(c.user_pct, cores), self.t("tl.your_apps")),
            Tile::text("▣", self.t("st.system"), u.cpu(c.system_pct, cores), "macOS"),
            Tile::new("⚡", self.t("tl.cores"), cores.to_string(), chips),
            self.top_app_tile(snap, RankMetric::Cpu, "tl.top_app"),
        ];
        self.tiles(ui, MetricKind::Cpu, tiles);

        // Extras: per-core load, temperatures and fans.
        let color = self.color(ui, MetricKind::Cpu);
        let pal = self.pal(ui.ctx());
        let sensors = snap.machine.sensors.clone();
        let mut right = String::new();
        if let Some(t) = sensors.cpu_temp_c {
            right.push_str(&format!("CPU {}", u.temp(t)));
        }
        for f in &sensors.fans {
            right.push_str(&format!("   {}", self.tf("fan_rpm", &[("i", &(f.index + 1).to_string()), ("rpm", &format!("{:.0}", f.rpm))])));
        }
        Self::section(ui, &self.t("sec.per_core"), right.trim(), |ui| {
            let n = c.per_core.len().max(1);
            let h = 70.0;
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h + 18.0), egui::Sense::hover());
            let slot = rect.width() / n as f32;
            let bar_w = (slot * 0.6).min(28.0);
            for (i, v) in c.per_core.iter().enumerate() {
                let x = rect.left() + slot * i as f32 + (slot - bar_w) / 2.0;
                let bg = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(bar_w, h));
                ui.painter().rect_filled(bg, 4.0, color.gamma_multiply(0.14));
                let fh = h * (*v / 100.0).clamp(0.0, 1.0) as f32;
                let fill = Rect::from_min_max(egui::pos2(x, bg.bottom() - fh), bg.max);
                ui.painter().rect_filled(fill, 4.0, if *v > 90.0 { pal.danger } else { color });
                ui.painter().text(egui::pos2(x + bar_w / 2.0, rect.bottom()), egui::Align2::CENTER_BOTTOM, format!("{:.0}", v), egui::FontId::proportional(10.5), ui.visuals().weak_text_color());
            }
        });
        self.app_list(ui, snap, RankMetric::Cpu, 25, "tab.cpu");
    }

    pub(super) fn memory(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let m = snap.machine.memory.clone();
        let u = self.units();
        let pal = self.pal(ui.ctx());
        let fmt = move |v: f64| u.bytes(v as u64);
        let used_frac = m.used as f64 / m.total.max(1) as f64;
        let (pressure, pc) = if m.swap_used > m.total / 8 || used_frac > 0.95 {
            (self.t("pressure.high_long"), pal.danger)
        } else if m.swap_used > 0 && used_frac > 0.85 {
            (self.t("pressure.elevated_long"), pal.warn)
        } else {
            (self.t("pressure.normal_long"), self.color(ui, MetricKind::Battery))
        };
        self.hero(ui, Hero {
            kind: MetricKind::Memory,
            metric: "memory",
            caption: self.tf("in_use_of", &[("total", &u.bytes(m.total))]),
            value: u.bytes(m.used),
            badge: Some((pressure, pc)),
            rows: vec![(self.t("swap"), u.bytes(m.swap_used))],
            max: Some(m.total as f64),
            fmt: &fmt,
        });
        let tiles = vec![
            Tile::text("☰", self.t("mem.app"), u.bytes(m.app), self.t("tl.used_by_apps")),
            Tile::text("☰", self.t("mem.wired"), u.bytes(m.wired), self.t("tl.kept_in_ram")),
            Tile::text("☰", self.t("mem.compressed"), u.bytes(m.compressed), self.tf("tl.cached", &[("v", &u.bytes(m.cached))])),
            self.top_app_tile(snap, RankMetric::Memory, "tl.top_app"),
        ];
        self.tiles(ui, MetricKind::Memory, tiles);
        let dark = ui.visuals().dark_mode;
        let parts = [
            (self.t("mem.app"), m.app as f64, self.color(ui, MetricKind::Cpu)),
            (self.t("mem.wired"), m.wired as f64, self.color(ui, MetricKind::Sound)),
            (self.t("mem.compressed"), m.compressed as f64, self.color(ui, MetricKind::Network)),
            (self.t("mem.cached"), m.cached as f64, if dark { Color32::from_gray(110) } else { Color32::from_gray(170) }),
        ];
        Self::section(ui, &self.t("memory_by_type"), &self.tf("n_free", &[("v", &u.bytes(m.free))]), |ui| {
            widgets::stacked_bar(ui, &parts, m.total as f64);
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                for (label, v, c) in &parts {
                    widgets::dot(ui, *c);
                    ui.label(RichText::new(label).weak());
                    ui.label(widgets::num(&self.settings, u.bytes(*v as u64), 13.0));
                    ui.add_space(14.0);
                }
            });
        });
        self.app_list(ui, snap, RankMetric::Memory, 60, "tab.memory");
    }

    pub(super) fn disk(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let d = snap.machine.disk.clone();
        let u = self.units();
        let fmt = move |v: f64| u.rate(v);
        let written = self.history_total("disk_write", local_midnight());
        self.hero(ui, Hero {
            kind: MetricKind::Disk,
            metric: "disk",
            caption: self.t("disk.read_write"),
            value: u.rate(d.read_bps + d.write_bps),
            badge: None,
            rows: vec![(self.t("disk.written_today"), written.filter(|v| *v > 0.0).map(|v| u.transferred(v as u64)).unwrap_or("—".into()))],
            max: None,
            fmt: &fmt,
        });
        let boot = d.boot_free().cloned();
        let tiles = vec![
            Tile::text("🖴", self.t("st.reading"), u.rate(d.read_bps), self.t("disk.from_disk")),
            Tile::text("🖴", self.t("st.writing"), u.rate(d.write_bps), self.t("disk.to_disk")),
            Tile::text("🖴", self.t("free"), boot.as_ref().map(|v| u.bytes(v.free)).unwrap_or("—".into()), boot.as_ref().map(|v| self.tf("tl.of", &[("v", &u.bytes(v.total))])).unwrap_or_default()),
            self.top_app_tile(snap, RankMetric::Disk, "tl.top_app"),
        ];
        self.tiles(ui, MetricKind::Disk, tiles);
        let color = self.color(ui, MetricKind::Disk);
        Self::section(ui, &self.t("disk.volumes"), &self.t("disk.used"), |ui| {
            for v in &d.volumes {
                let used = 1.0 - v.free as f64 / v.total.max(1) as f64;
                let sub = self.tf("disk.volume_sub", &[("mount", &v.mount_point), ("free", &u.bytes(v.free)), ("total", &u.bytes(v.total)), ("removable", &if v.removable { self.t("disk.removable") } else { String::new() })]);
                self.row(ui, "🖴", color, &v.name, &sub, |ui| {
                    ui.allocate_ui_with_layout(Vec2::new(200.0, 34.0), egui::Layout::top_down(Align::Max), |ui| {
                        ui.set_width(200.0);
                        ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| ui.label(RichText::new(format!("{:.0}%", used * 100.0)).strong()));
                        widgets::thin_meter(ui, used, 200.0, color);
                    });
                });
            }
        });
        self.app_list(ui, snap, RankMetric::Disk, 25, "tab.disk");
    }

    pub(super) fn network(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let n = snap.machine.network.clone();
        let u = self.units();
        let fmt = move |v: f64| u.rate(v);
        let today = self.history_total("net_down", local_midnight());
        let week = self.history_total("net_down", sys::now_s() - 7 * 86_400);
        let tot = |v: Option<f64>| v.filter(|v| *v > 0.0).map(|v| u.transferred(v as u64)).unwrap_or("—".into());
        self.hero(ui, Hero {
            kind: MetricKind::Network,
            metric: "network",
            caption: self.t("net.down_up"),
            value: u.rate(n.rx_bps + n.tx_bps),
            badge: None,
            rows: vec![(self.t("net.downloaded_7d"), tot(week))],
            max: None,
            fmt: &fmt,
        });
        let tiles = vec![
            Tile::text("🌐", self.t("st.downloading"), u.rate(n.rx_bps), self.t("net.all_interfaces")),
            Tile::text("🌐", self.t("st.uploading"), u.rate(n.tx_bps), self.t("net.all_interfaces")),
            Tile::text("🌐", self.t("st.today"), tot(today), self.t("net.since_midnight")),
            self.top_app_tile(snap, RankMetric::Network, "tl.top_app"),
        ];
        self.tiles(ui, MetricKind::Network, tiles);
        let color = self.color(ui, MetricKind::Network);
        Self::section(ui, &self.t("net.interfaces"), &self.t("net.down_up_short"), |ui| {
            for i in &n.interfaces {
                let sub = self.tf("net.since_boot", &[("down", &u.transferred(i.total_rx)), ("up", &u.transferred(i.total_tx))]);
                self.row(ui, "🌐", color, &i.name, &sub, |ui| {
                    ui.label(RichText::new(format!("{}  ·  {}", u.rate(i.rx_bps), u.rate(i.tx_bps))).strong());
                });
            }
        });
        self.app_list(ui, snap, RankMetric::Network, 25, "net.list_title");
    }

    pub(super) fn gpu(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let g = snap.machine.gpu.clone();
        let u = self.units();
        let fmt = |v: f64| format!("{v:.0}%");
        self.hero(ui, Hero {
            kind: MetricKind::Gpu,
            metric: "gpu",
            caption: if g.name.is_empty() { "GPU".into() } else { g.name.clone() },
            value: format!("{:.0} %", g.utilization_pct),
            badge: (!g.available).then(|| (self.t("gpu.no_stats"), self.pal(ui.ctx()).warn)),
            rows: vec![],
            max: Some(100.0),
            fmt: &fmt,
        });
        let s = &snap.machine.sensors;
        let tiles = vec![
            Tile::text("🖥", self.t("gpu.memory"), if g.vram_used > 0 { u.bytes(g.vram_used) } else { "—".into() }, if g.vram_total > 0 { self.tf("tl.of", &[("v", &u.bytes(g.vram_total))]) } else { self.t("gpu.shared") }),
            Tile::text("⚡", self.t("st.power"), watts(&self.i18n, g.power_w.or(snap.machine.power.gpu_w)), self.t("gpu.package")),
            Tile::text("🖥", self.t("temperature"), s.gpu_temp_c.map(|t| u.temp(t)).unwrap_or("—".into()), self.t("gpu.die")),
            self.top_app_tile(snap, RankMetric::Energy, "tl.top_energy_app"),
        ];
        self.tiles(ui, MetricKind::Gpu, tiles);
        self.app_list(ui, snap, RankMetric::Energy, 15, "gpu.list_title");
    }

    pub(super) fn neural(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let n = snap.machine.neural.clone();
        let p = snap.machine.power.clone();
        let fmt = |v: f64| format!("{v:.0}%");
        self.hero(ui, Hero {
            kind: MetricKind::Neural,
            metric: "neural",
            caption: if n.available { self.t("neural.estimated_load") } else { self.t("neural.not_reported") },
            value: if n.available { format!("{:.0} %", n.utilization_pct) } else { "—".into() },
            badge: None,
            rows: vec![(self.t("st.power"), watts(&self.i18n, Some(n.power_w)))],
            max: Some(100.0),
            fmt: &fmt,
        });
        let total = [p.cpu_w, p.gpu_w, p.ane_w].iter().flatten().sum::<f64>();
        let share = p.ane_w.filter(|_| total > 0.0).map(|a| self.tf("neural.chip_share", &[("v", &format!("{:.0}", a / total * 100.0))])).unwrap_or_default();
        let tiles = vec![
            Tile::text("⚛", "Neural Engine", watts(&self.i18n, p.ane_w), share),
            Tile::text("▣", "CPU", watts(&self.i18n, p.cpu_w), self.t("neural.package_power")),
            Tile::text("🖥", "GPU", watts(&self.i18n, p.gpu_w), self.t("neural.package_power")),
            Tile::text("⚡", self.t("st.system"), watts(&self.i18n, p.system_w), self.t("neural.whole_mac")),
        ];
        self.tiles(ui, MetricKind::Neural, tiles);
        Self::section(ui, &self.t("neural.about"), "", |ui| {
            ui.label(RichText::new(self.t(if n.available { "neural.about_text" } else { "neural.about_unavailable" })).weak());
        });
    }

    pub(super) fn battery(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let b = snap.machine.battery.clone();
        let u = self.units();
        let pal = self.pal(ui.ctx());
        let fmt = |v: f64| format!("{v:.0}%");
        let state = self.t(if !b.present { "battery.none" } else if b.charging { "battery.charging" } else if b.on_ac { "battery.on_adapter" } else { "battery.on_battery" });
        self.hero(ui, Hero {
            kind: MetricKind::Battery,
            metric: "battery",
            caption: state.clone(),
            value: if b.present { format!("{:.0} %", b.percent) } else { watts(&self.i18n, snap.machine.power.system_w) },
            badge: None,
            rows: vec![(self.t("battery.system_power"), watts(&self.i18n, snap.machine.power.system_w))],
            max: Some(100.0),
            fmt: &fmt,
        });
        let low_health = b.health_pct.filter(|h| *h < 80.0).map(|_| pal.warn);
        let tiles = vec![
            Tile::text("🔋", self.t(if b.charging { "st.until_full" } else { "st.remaining" }), b.time_remaining_min.filter(|m| *m > 0).map(|m| format_duration(m * 60)).unwrap_or("—".into()), state),
            Tile::text("⚡", self.t("st.power_draw"), watts(&self.i18n, b.power_w.map(f64::abs)), self.t(if b.charging { "battery.into" } else { "battery.from" })),
            Tile::text("🔋", self.t("st.health"), pct(b.health_pct), b.cycle_count.map(|c| self.i18n.tn("n.cycles", c as usize, &[])).unwrap_or_default()).colored(low_health),
            Tile::text("🔋", self.t("temperature"), b.temperature_c.map(|t| u.temp(t)).unwrap_or("—".into()), self.t("battery.pack")),
        ];
        self.tiles(ui, MetricKind::Battery, tiles);
        self.app_list(ui, snap, RankMetric::Energy, 25, "hist.energy");
    }

    pub(super) fn sound(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let a = snap.machine.audio.clone();
        let color = self.color(ui, MetricKind::Sound);
        let playing = a.apps.iter().filter(|x| x.state == AudioState::Playing).count();
        let mics = a.apps.iter().filter(|x| x.using_mic).count();
        let tiles = vec![
            Tile::text("🔊", self.t("sound.output"), a.output_volume.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or("—".into()), if a.output_muted { self.t("sound.muted") } else { a.output_device.clone().unwrap_or(self.t("sound.no_output")) }),
            Tile::text("🔊", self.t("sound.microphone"), a.input_volume.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or("—".into()), self.t("sound.input_level")),
            Tile::text("🔊", self.t("sound.playing"), playing.to_string(), self.i18n.tn("sound.apps_audio", a.apps.len(), &[])),
            Tile::text("🔊", self.t("sound.using_mic"), mics.to_string(), self.t(if mics > 0 { "sound.mic_in_use" } else { "sound.nobody" })).colored((mics > 0).then_some(color)),
        ];
        self.tiles(ui, MetricKind::Sound, tiles);
        Self::section(ui, &self.t("sound.volume"), a.output_device.as_deref().unwrap_or(""), |ui| {
            if let Some(mut v) = a.output_volume {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(self.t("sound.output")).size(14.0));
                    ui.spacing_mut().slider_width = 320.0;
                    if ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false)).changed() {
                        crate::collectors::devices::set_volume(false, v);
                    }
                    let mut muted = a.output_muted;
                    if ui.checkbox(&mut muted, self.t("sound.muted")).changed() {
                        crate::collectors::devices::set_muted(muted);
                    }
                });
            }
            if let Some(mut v) = a.input_volume {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(self.t("sound.microphone")).size(14.0));
                    ui.spacing_mut().slider_width = 320.0;
                    if ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false)).changed() {
                        crate::collectors::devices::set_volume(true, v);
                    }
                });
            }
            ui.label(RichText::new(self.t("sound.per_app_note")).small().weak());
        });
        Self::section(ui, &self.t("sound.apps_using"), &self.t("sound.state"), |ui| {
            if a.apps.is_empty() {
                ui.label(RichText::new(self.t("sound.none_using")).weak());
            }
            for app in &a.apps {
                let bundle = snap.app_for_pid(app.pid).and_then(|g| g.app.bundle_path.clone());
                ui.horizontal(|ui| {
                    ui.set_min_height(44.0);
                    self.app_icon(ui, bundle.as_deref(), &app.app_name, 28.0);
                    ui.add_space(6.0);
                    ui.label(RichText::new(&app.app_name).size(14.5).strong().color(ui.visuals().strong_text_color()));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let s = self.t(match app.state {
                            AudioState::Playing => "sound.playing",
                            AudioState::Silent => "sound.silent",
                            AudioState::Muted => "sound.muted",
                        });
                        widgets::chip(ui, &s, color);
                        if app.using_mic {
                            widgets::chip(ui, &self.t("sound.mic"), self.pal(ui.ctx()).warn);
                        }
                    });
                });
            }
        });
    }

    pub(super) fn bluetooth(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let b = snap.machine.bluetooth.clone();
        let color = self.color(ui, MetricKind::Bluetooth);
        let pal = self.pal(ui.ctx());
        let batt = |d: &BluetoothDevice| [d.battery_main, d.battery_left, d.battery_right].into_iter().flatten().fold(f64::NAN, f64::min);
        let lowest = b.devices.iter().filter(|d| d.connected).map(|d| (d.name.clone(), batt(d))).filter(|x| !x.1.is_nan()).min_by(|x, y| x.1.total_cmp(&y.1));
        let connected = b.devices.iter().filter(|d| d.connected).count();
        let tiles = vec![
            Tile::text("📶", "Bluetooth", self.t(if b.powered { "bt.on" } else { "bt.off" }), self.t("bt.refreshed")),
            Tile::text("📶", self.t("bt.connected"), connected.to_string(), self.t("bt.nearby")),
            Tile::text("📶", self.t("bt.paired"), b.devices.len().to_string(), self.tf("bt.away", &[("n", &(b.devices.len() - connected).to_string())])),
            Tile::text("🔋", self.t("bt.lowest"), lowest.as_ref().map(|x| format!("{:.0}%", x.1)).unwrap_or("—".into()), lowest.as_ref().map(|x| x.0.clone()).unwrap_or_default())
                .colored(lowest.as_ref().filter(|x| x.1 < 20.0).map(|_| pal.danger)),
        ];
        self.tiles(ui, MetricKind::Bluetooth, tiles);
        let mut open_settings = false;
        Self::section(ui, &self.t("bt.devices"), &self.t("tab.battery"), |ui| {
            for d in &b.devices {
                let sub = format!("{}{}", self.t(if d.connected { "bt.connected" } else { "bt.not_connected" }), if d.kind.is_empty() { String::new() } else { format!(" · {}", d.kind) });
                let lead = if d.connected { color } else { ui.visuals().weak_text_color() };
                self.row(ui, "📶", lead, &d.name, &sub, |ui| {
                    let parts: Vec<(&str, Option<f64>)> = if d.battery_left.is_some() || d.battery_right.is_some() {
                        vec![("bt.case", d.battery_case), ("R", d.battery_right), ("L", d.battery_left)]
                    } else {
                        vec![("", d.battery_main)]
                    };
                    for (label, v) in parts {
                        let label = if label.starts_with("bt.") { self.t(label) } else { label.to_string() };
                        if let Some(v) = v {
                            ui.allocate_ui_with_layout(Vec2::new(70.0, 34.0), egui::Layout::top_down(Align::Max), |ui| {
                                ui.set_width(70.0);
                                ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| ui.label(RichText::new(format!("{label} {v:.0}%").trim().to_string()).strong()));
                                widgets::thin_meter(ui, v / 100.0, 70.0, if v < 20.0 { pal.danger } else { color });
                            });
                            ui.add_space(10.0);
                        }
                    }
                });
            }
            if b.devices.is_empty() {
                ui.label(RichText::new(self.t("bt.no_devices")).weak());
            }
            ui.add_space(4.0);
            open_settings = ui.button(self.t("bt.settings")).clicked();
        });
        if open_settings {
            let _ = std::process::Command::new("/usr/bin/open").arg("x-apple.systempreferences:com.apple.BluetoothSettings").spawn();
        }
    }

    pub(super) fn projects_tab(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let u = self.units();
        let pal = self.pal(ui.ctx());
        let color = self.color(ui, MetricKind::Cpu);
        let idle = projects::idle_servers(&snap.projects);
        let servers: usize = snap.projects.iter().map(|p| p.servers.len()).sum();
        let tiles = vec![
            Tile::text("🗀", self.t("tab.projects"), snap.projects.len().to_string(), self.t("proj.with_processes")),
            Tile::text("🗀", self.t("proj.dev_servers"), servers.to_string(), self.t("proj.listening")),
            Tile::text("🗀", self.t("proj.idle_servers"), idle.len().to_string(), self.t("proj.no_traffic")).colored((!idle.is_empty()).then_some(pal.warn)),
            Tile::text("☰", self.t("tab.memory"), u.bytes(snap.projects.iter().map(|p| p.mem_bytes).sum()), self.t("proj.all_processes")),
        ];
        self.tiles(ui, MetricKind::Cpu, tiles);
        let mut ask: Vec<Target> = vec![];
        if let Some(b) = loc::idle_banner(&self.i18n, &u, &snap.projects) {
            Self::section(ui, &self.t("proj.idle"), "", |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&b).color(pal.warn));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(self.i18n.t("stop_all")).clicked() {
                            ask = idle.iter().map(|s| Target::DevServer(s.process_pid)).collect();
                        }
                    });
                });
            });
        }
        if snap.projects.is_empty() {
            Self::section(ui, &self.t("tab.projects"), "", |ui| {
                ui.label(RichText::new(self.t("proj.none")).weak());
            });
        }
        for p in &snap.projects {
            let right = format!("{} · {} · {}", u.bytes(p.mem_bytes), self.tf("proj.proc_short", &[("n", &p.process_count.to_string())]), loc::project_state(&self.i18n, &p.state));
            Self::section(ui, &p.project.name, &right, |ui| {
                ui.label(RichText::new(&p.project.folder_path).small().weak());
                for s in &p.servers {
                    let lead = if s.idle { pal.warn } else { color };
                    let sub = format!("{} · {}", u.bytes(s.mem_bytes), if s.idle { self.tf("proj.idle_for", &[("t", &format_duration(s.idle_secs))]) } else { self.t("proj.active") });
                    self.row(ui, "🗀", lead, &format!(":{}  {}", s.port, s.command), &sub, |ui| {
                        if ui.button(self.i18n.t("stop")).clicked() {
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
                self.confirm = Some(Plan { title: self.i18n.tn("n.idle_servers", n, &[]), pids, ports, mem_bytes: mem, ..first });
            }
        }
    }

    pub(super) fn agents_tab(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let u = self.units();
        let now = snap.timestamp / 1000;
        let pal = self.pal(ui.ctx());
        let color = self.color(ui, MetricKind::Memory);
        let working = snap.agents.iter().filter(|a| a.state == AgentState::Working).count();
        let tightest = snap.plans.iter().filter(|p| p.pace_state != PaceState::Unknown).min_by(|a, b| b.used_pct.total_cmp(&a.used_pct));
        let tiles = vec![
            Tile::text("👾", self.t("st.sessions"), snap.agents.len().to_string(), self.tf("agents.n_working", &[("n", &working.to_string())])),
            Tile::text("☰", self.t("tab.memory"), u.bytes(snap.agents.iter().map(|a| a.mem_bytes).sum()), self.t("agents.and_started")),
            Tile::text("▣", "CPU", u.cpu(snap.agents.iter().map(|a| a.cpu_percent).sum(), snap.machine.cpu.core_count), self.t("agents.all_sessions")),
            match tightest {
                Some(p) => Tile::text("👾", self.t("agents.closest_limit"), self.tf("pct_left", &[("v", &format!("{:.0}", 100.0 - p.used_pct))]), format!("{} · {}", p.provider, loc::window_label(&self.i18n, &p.window)))
                    .colored((p.pace_state == PaceState::RunsOut).then_some(pal.danger)),
                None => Tile::text("👾", self.t("agents.closest_limit"), "—", self.t("no_plan_usage")),
            },
        ];
        self.tiles(ui, MetricKind::Memory, tiles);
        Self::section(ui, &self.t("agents.ai_plans"), &self.t("agents.used"), |ui| {
            if snap.plans.is_empty() {
                ui.label(RichText::new(self.t("agents.configure")).weak());
            }
            for p in &snap.plans {
                let c = match p.pace_state {
                    PaceState::RunsOut => pal.danger,
                    PaceState::Ahead => pal.warn,
                    PaceState::OnPace => color,
                    PaceState::Unknown => ui.visuals().weak_text_color(),
                };
                let headline = loc::plan_headline(&self.i18n, p, now);
                let pace = loc::pace_text(&self.i18n, &p.pace_text);
                let sub = if headline.contains(&pace) { "" } else { pace.as_str() };
                self.row(ui, "👾", c, &headline, sub, |ui| {
                    ui.allocate_ui_with_layout(Vec2::new(220.0, 34.0), egui::Layout::top_down(Align::Max), |ui| {
                        ui.set_width(220.0);
                        ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| ui.label(RichText::new(format!("{:.0}%", p.used_pct)).strong()));
                        widgets::thin_meter(ui, p.used_pct / 100.0, 220.0, c);
                    });
                });
            }
        });
        let mut quit: Option<i32> = None;
        let mut toggle: Option<String> = None;
        let expanded = self.expanded.clone();
        Self::section(ui, &self.t("st.sessions"), &self.t("agents.memory_cpu"), |ui| {
            if snap.agents.is_empty() {
                ui.label(RichText::new(self.t("agents.none")).weak());
            }
            for a in &snap.agents {
                let (st, c) = match a.state {
                    AgentState::Working => (loc::agent_state(&self.i18n, a.state), color),
                    AgentState::Waiting => (loc::agent_state(&self.i18n, a.state), pal.warn),
                    AgentState::Idle => (loc::agent_state(&self.i18n, a.state), ui.visuals().weak_text_color()),
                };
                let key = format!("agent:{}", a.pid);
                let open = expanded.get(&key).copied().unwrap_or(false);
                let sub = format!("{}{} · {} · {}", a.project, a.branch_or_task.as_ref().map(|b| format!(" · {b}")).unwrap_or_default(), st, format_duration(now - a.started_at));
                self.row(ui, "👾", c, a.agent_kind.label(), &sub, |ui| {
                    if ui.button(self.t("agents.quit_session")).clicked() {
                        quit = Some(a.pid);
                    }
                    if !a.started.is_empty() {
                        let label = if open { self.t("agents.hide_started") } else { self.i18n.tn("agents.n_started", a.started.len(), &[]) };
                        if ui.button(label).clicked() {
                            toggle = Some(key.clone());
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new(format!("{}  ·  {}", u.bytes(a.mem_bytes), u.cpu(a.cpu_percent, snap.machine.cpu.core_count))).strong());
                });
                if open {
                    egui::Grid::new(&key).striped(true).num_columns(4).spacing([18.0, 3.0]).show(ui, |ui| {
                        for s in &a.started {
                            ui.label(RichText::new(&s.role).weak());
                            ui.label(&s.name);
                            ui.label(RichText::new(format!("pid {}", s.pid)).weak());
                            ui.label(u.bytes(s.mem_bytes));
                            ui.end_row();
                        }
                    });
                }
            }
        });
        if let Some(k) = toggle {
            let v = self.expanded.get(&k).copied().unwrap_or(false);
            self.expanded.insert(k, !v);
        }
        if let Some(pid) = quit {
            self.ask(snap, Target::AgentSession(pid), Mode::Quit);
        }
    }

    /// `docker::card_line` in the UI language: "rabbitmq — 206 MB — 86 % of 240 MB — up 3 hours — port 5672".
    fn container_line(&self, c: &Container) -> String {
        let u = self.units();
        let mut parts = vec![c.name.clone(), u.bytes(c.mem_bytes)];
        if let Some(p) = c.mem_pct_of_limit() {
            parts.push(self.tf("docker.pct_of", &[("p", &format!("{p:.0}")), ("limit", &u.bytes(c.mem_limit))]));
        }
        parts.push(c.status.to_lowercase());
        if !c.published_ports.is_empty() {
            let ps: Vec<String> = c.published_ports.iter().map(u16::to_string).collect();
            parts.push(self.i18n.tn("n.port_list", ps.len(), &[("ports", &ps.join(", "))]));
        }
        parts.join(" — ")
    }

    pub(super) fn containers_tab(&mut self, ui: &mut egui::Ui, snap: &Arc<Snapshot>) {
        let u = self.units();
        let pal = self.pal(ui.ctx());
        let color = self.color(ui, MetricKind::Network);
        if !snap.docker_available {
            self.tiles(ui, MetricKind::Network, vec![Tile::text("📦", "Docker", self.t("docker.not_running"), self.t("docker.no_socket"))]);
            return;
        }
        let near = docker::near_limit(&snap.containers);
        let running = snap.containers.iter().filter(|c| c.up()).count();
        let stacks = docker::stacks(&snap.containers);
        let tiles = vec![
            Tile::text("📦", self.t("docker.running"), running.to_string(), self.tf("docker.of_n", &[("n", &self.i18n.tn("n.containers", snap.containers.len(), &[]))])),
            Tile::text("📦", self.t("docker.stacks"), stacks.len().to_string(), self.t("docker.compose")),
            Tile::text("☰", self.t("tab.memory"), u.bytes(snap.containers.iter().map(|c| c.mem_bytes).sum()), self.t("docker.all")),
            Tile::text("📦", self.t("docker.near_limit"), near.len().to_string(), self.t("docker.above_85")).colored((!near.is_empty()).then_some(pal.danger)),
        ];
        self.tiles(ui, MetricKind::Network, tiles);
        for c in &near {
            ui.label(RichText::new(format!("⚠ {}", self.container_line(c))).color(pal.danger));
        }
        let mut stop: Option<String> = None;
        for (stack, cs) in stacks {
            let right = stack.project_hint.clone().unwrap_or_default();
            let stack_name = if stack.name == "standalone" { self.t("docker.standalone") } else { stack.name.clone() };
            Self::section(ui, &stack_name, &right, |ui| {
                for c in cs {
                    let lead = if c.up() { color } else { ui.visuals().weak_text_color() };
                    let ports = c.published_ports.iter().map(|p| format!(":{p}")).collect::<Vec<_>>().join(" ");
                    let sub = format!("{} · {}{}", c.image, c.status, if ports.is_empty() { String::new() } else { format!(" · {ports}") });
                    self.row(ui, "📦", lead, &c.name, &sub, |ui| {
                        if c.up() && ui.button(self.i18n.t("stop")).clicked() {
                            stop = Some(c.id.clone());
                        }
                        ui.add_space(8.0);
                        ui.allocate_ui_with_layout(Vec2::new(220.0, 34.0), egui::Layout::top_down(Align::Max), |ui| {
                            ui.set_width(220.0);
                            let limit = c.mem_pct_of_limit();
                            let txt = match limit {
                                Some(p) => self.tf("docker.mem_limit", &[("mem", &u.bytes(c.mem_bytes)), ("p", &format!("{p:.0}")), ("limit", &u.bytes(c.mem_limit)), ("cpu", &self.i18n.num(format!("{:.1}", c.cpu_pct)))]),
                                None => self.tf("docker.no_limit", &[("mem", &u.bytes(c.mem_bytes)), ("cpu", &self.i18n.num(format!("{:.1}", c.cpu_pct)))]),
                            };
                            ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| ui.label(RichText::new(txt).strong()));
                            if let Some(p) = limit {
                                widgets::thin_meter(ui, p / 100.0, 220.0, if p >= 85.0 { pal.danger } else { color });
                            }
                        });
                    });
                }
            });
        }
        if let Some(id) = stop {
            self.ask(snap, Target::Container(id), Mode::Quit);
        }
    }
}
