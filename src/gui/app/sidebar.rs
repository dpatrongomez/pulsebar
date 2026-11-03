//! Source-list sidebar: grouped navigation (System / Developer / History) with Settings pinned at
//! the bottom. Follows the order and visibility configured in Settings → Tabs, shows a live mini
//! value for CPU and Memory, and collapses to icons only on narrow windows.

use eframe::egui::{self, Align2, Color32, FontId, Rect, RichText, Sense, Vec2};

use super::{PulseApp, tab_glyph};
use crate::model::{MetricKind, Snapshot};
use crate::settings::Tab;

/// Below this window width the sidebar shows icons only.
const COLLAPSE_BELOW: f32 = 900.0;
const WIDE: f32 = 208.0;
const NARROW: f32 = 52.0;
const ROW_H: f32 = 30.0;

#[derive(Clone, Copy, PartialEq)]
enum Group {
    System,
    Developer,
    History,
}

fn group(t: Tab) -> Group {
    match t {
        Tab::Projects | Tab::Agents | Tab::Containers => Group::Developer,
        Tab::History => Group::History,
        // Settings is pinned separately; anything else is a system resource.
        _ => Group::System,
    }
}

impl PulseApp {
    pub(super) fn sidebar(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let collapsed = ctx.content_rect().width() < COLLAPSE_BELOW;
        let dark = ctx.style().visuals.dark_mode;
        let fill = if dark { Color32::from_rgb(28, 28, 31) } else { Color32::from_rgb(230, 230, 235) };
        let frame = egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(8, 10));
        let width = if collapsed { NARROW } else { WIDE };
        egui::SidePanel::left("sidebar").frame(frame).resizable(false).exact_width(width).show_separator_line(false).show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let inner = width - 16.0;
            let tabs: Vec<Tab> = self.settings.tabs.iter().filter(|t| t.visible).map(|t| t.tab).collect();
            let mut go: Option<Tab> = None;

            // Settings is pinned to the bottom; the rest scrolls if the window is short.
            egui::TopBottomPanel::bottom("sidebar_bottom").frame(egui::Frame::new()).show_separator_line(false).show_inside(ui, |ui| {
                ui.add_space(6.0);
                if self.sidebar_row(ui, Tab::Settings, inner, collapsed, None) {
                    go = Some(Tab::Settings);
                }
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (g, header) in [(Group::System, Some("nav.system")), (Group::Developer, Some("nav.developer")), (Group::History, None)] {
                    let items: Vec<Tab> = tabs.iter().copied().filter(|t| *t != Tab::Settings && group(*t) == g).collect();
                    if items.is_empty() {
                        continue;
                    }
                    if let Some(h) = header {
                        if collapsed {
                            ui.add_space(6.0);
                            ui.separator();
                        } else {
                            ui.add_space(8.0);
                            ui.label(RichText::new(self.i18n.t(h).to_uppercase()).size(10.5).strong().color(ui.visuals().weak_text_color()));
                        }
                    } else {
                        ui.add_space(6.0);
                    }
                    for t in items {
                        let mini = self.sidebar_mini(t, snap);
                        if self.sidebar_row(ui, t, inner, collapsed, mini.as_deref()) {
                            go = Some(t);
                        }
                    }
                }
            });
            if let Some(t) = go {
                self.tab = t;
                self.highlight = None;
            }
        });
    }

    /// Live glanceable value shown on the right of an entry.
    fn sidebar_mini(&self, t: Tab, snap: &Snapshot) -> Option<String> {
        let m = &snap.machine;
        match t {
            Tab::Cpu => Some(format!("{:.0}%", m.cpu.total_pct)),
            Tab::Memory if m.memory.total > 0 => Some(format!("{:.0}%", m.memory.used as f64 / m.memory.total as f64 * 100.0)),
            _ => None,
        }
    }

    /// One focusable, keyboard-activatable row. Returns true when chosen.
    fn sidebar_row(&self, ui: &mut egui::Ui, t: Tab, width: f32, collapsed: bool, mini: Option<&str>) -> bool {
        let label = self.i18n.t(t.key());
        let dark = ui.visuals().dark_mode;
        let accent = super::widgets::metric_color(self.settings.theme, dark, MetricKind::Cpu);
        let selected = self.tab == t;
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label.clone()));
        if ui.is_rect_visible(rect) {
            let p = ui.painter();
            let bg = if selected {
                accent.gamma_multiply(0.18)
            } else if resp.hovered() {
                ui.visuals().widgets.hovered.weak_bg_fill.gamma_multiply(0.6)
            } else {
                Color32::TRANSPARENT
            };
            p.rect_filled(rect, 8.0, bg);
            if resp.has_focus() {
                p.rect_stroke(rect, 8.0, ui.visuals().selection.stroke, egui::StrokeKind::Inside);
            }
            let fg = if selected { accent } else { ui.visuals().text_color() };
            let glyph = FontId::proportional(14.0);
            if collapsed {
                p.text(rect.center(), Align2::CENTER_CENTER, tab_glyph(t), glyph, fg);
            } else {
                p.text(egui::pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, tab_glyph(t), glyph, fg);
                let mini_w = mini.map_or(0.0, |m| p.layout_no_wrap(m.to_string(), FontId::proportional(11.5), fg).size().x + 8.0);
                let text_rect = Rect::from_min_max(egui::pos2(rect.left() + 34.0, rect.top()), egui::pos2(rect.right() - 8.0 - mini_w, rect.bottom()));
                let galley = p.layout_no_wrap(label.clone(), FontId::proportional(13.0), fg);
                // Clip so a long translated label can never push past the mini value or wrap.
                p.with_clip_rect(text_rect).galley(egui::pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0), galley, fg);
                if let Some(m) = mini {
                    p.text(egui::pos2(rect.right() - 8.0, rect.center().y), Align2::RIGHT_CENTER, m, FontId::proportional(11.5), ui.visuals().weak_text_color());
                }
            }
        }
        let resp = if collapsed { resp.on_hover_text(label) } else { resp };
        resp.clicked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tab_has_a_group() {
        assert!(group(Tab::Cpu) == Group::System);
        assert!(group(Tab::Containers) == Group::Developer);
        assert!(group(Tab::History) == Group::History);
        // The pinned Settings tab is handled separately but must never land in a header group by accident.
        assert!(Tab::DEFAULT_ORDER.iter().filter(|t| **t != Tab::Settings).all(|t| matches!(group(*t), Group::System | Group::Developer | Group::History)));
    }
}
