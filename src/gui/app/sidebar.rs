//! Source-list sidebar: grouped navigation (System / Developer / History) with Settings pinned at
//! the bottom. Follows the order and visibility configured in Settings → Tabs, shows a live mini
//! value for CPU and Memory, and collapses to icons only on narrow windows.

use eframe::egui::{self, Align2, Color32, FontId, Rect, RichText, Sense, Vec2};

use super::{PulseApp, tab_glyph};
use crate::model::{MetricKind, Snapshot};
use crate::settings::{Tab, TabSetting};

/// Below this window width the sidebar shows icons only.
const COLLAPSE_BELOW: f32 = 900.0;
const WIDE: f32 = 208.0;
const NARROW: f32 = 52.0;
const ROW_H: f32 = 30.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Group {
    System,
    Developer,
    History,
}

impl Group {
    pub(super) const ALL: [Group; 3] = [Group::System, Group::Developer, Group::History];

    /// i18n key of the group header; History has none.
    pub(super) fn header_key(self) -> Option<&'static str> {
        match self {
            Group::System => Some("nav.system"),
            Group::Developer => Some("nav.developer"),
            Group::History => None,
        }
    }
}

/// Exhaustive on purpose: a new tab must be classified here. `None` means "pinned, not in a group"
/// (Settings).
pub(super) fn group(t: Tab) -> Option<Group> {
    match t {
        Tab::Overview | Tab::Cpu | Tab::Memory | Tab::Disk | Tab::Network | Tab::Gpu | Tab::Neural | Tab::Battery | Tab::Sound | Tab::Bluetooth => Some(Group::System),
        Tab::Projects | Tab::Agents | Tab::Containers => Some(Group::Developer),
        Tab::History => Some(Group::History),
        Tab::Settings => None,
    }
}

/// Indices into `tabs` per group, in the configured order, skipping empty groups. With `only_visible`
/// hidden tabs are left out. Settings never appears.
pub(super) fn grouped_indices(tabs: &[TabSetting], only_visible: bool) -> Vec<(Group, Vec<usize>)> {
    Group::ALL
        .iter()
        .filter_map(|&g| {
            let idx: Vec<usize> = tabs.iter().enumerate().filter(|(_, t)| group(t.tab) == Some(g) && (t.visible || !only_visible)).map(|(i, _)| i).collect();
            (!idx.is_empty()).then_some((g, idx))
        })
        .collect()
}

/// What the sidebar shows: visible tabs per group in configured order; empty groups are dropped.
pub(super) fn sections(tabs: &[TabSetting]) -> Vec<(Group, Vec<Tab>)> {
    grouped_indices(tabs, true).into_iter().map(|(g, idx)| (g, idx.into_iter().map(|i| tabs[i].tab).collect())).collect()
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
            let sections = sections(&self.settings.tabs);
            let mut go: Option<Tab> = None;

            // The list is drawn first (so keyboard focus visits it before Settings, matching what is
            // on screen), reserving room for the Settings row pinned at the bottom.
            let list_h = (ui.available_height() - ROW_H - 6.0 - ui.spacing().item_spacing.y).max(0.0);
            egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                let mut first = true;
                for (g, items) in sections {
                    match g.header_key() {
                        Some(_) if collapsed => {
                            if !first {
                                ui.add_space(6.0);
                                ui.separator();
                            }
                        }
                        Some(h) => {
                            ui.add_space(8.0);
                            ui.label(RichText::new(self.i18n.t(h).to_uppercase()).size(10.5).strong().color(ui.visuals().weak_text_color()));
                        }
                        None => {
                            ui.add_space(6.0);
                        }
                    }
                    first = false;
                    for t in items {
                        let mini = self.sidebar_mini(t, snap);
                        if self.sidebar_row(ui, t, inner, collapsed, mini.as_deref()) {
                            go = Some(t);
                        }
                    }
                }
            });
            ui.add_space(6.0);
            if self.sidebar_row(ui, Tab::Settings, inner, collapsed, None) {
                go = Some(Tab::Settings);
            }
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
            Tab::Cpu => Some(self.settings.units.cpu(m.cpu.total_pct, m.cpu.core_count)),
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
        // Include the live value so screen readers get the glance too (also when it is not drawn).
        let a11y = mini.map_or_else(|| label.clone(), |m| format!("{label}, {m}"));
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, a11y.clone()));
        // egui does not scroll to a focused custom widget on its own.
        if resp.gained_focus() {
            resp.scroll_to_me(None);
        }
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
        let resp = if collapsed {
            // Hover tooltips ignore keyboard focus, so show the label explicitly while focused.
            if resp.has_focus() && !resp.hovered() {
                resp.show_tooltip_text(label.clone());
            }
            resp.on_hover_text(label)
        } else {
            resp
        };
        resp.clicked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(list: &[(Tab, bool)]) -> Vec<TabSetting> {
        list.iter().map(|&(tab, visible)| TabSetting { tab, visible }).collect()
    }

    fn default_tabs() -> Vec<TabSetting> {
        Tab::DEFAULT_ORDER.iter().map(|&tab| TabSetting { tab, visible: true }).collect()
    }

    #[test]
    fn default_order_groups_tabs() {
        let s = sections(&default_tabs());
        assert_eq!(s.iter().map(|(g, _)| *g).collect::<Vec<_>>(), vec![Group::System, Group::Developer, Group::History]);
        assert_eq!(s[1].1, vec![Tab::Projects, Tab::Agents, Tab::Containers]);
        assert_eq!(s[2].1, vec![Tab::History]);
    }

    #[test]
    fn reordering_within_a_group_is_respected() {
        let tabs = ts(&[(Tab::Overview, true), (Tab::Memory, true), (Tab::Cpu, true), (Tab::Containers, true), (Tab::Projects, true)]);
        let s = sections(&tabs);
        assert_eq!(s[0], (Group::System, vec![Tab::Overview, Tab::Memory, Tab::Cpu]));
        assert_eq!(s[1], (Group::Developer, vec![Tab::Containers, Tab::Projects]));
    }

    #[test]
    fn hidden_tab_disappears_and_empty_group_is_dropped() {
        let tabs = ts(&[(Tab::Cpu, true), (Tab::Memory, false), (Tab::Projects, false), (Tab::Agents, false), (Tab::History, true)]);
        let s = sections(&tabs);
        assert_eq!(s, vec![(Group::System, vec![Tab::Cpu]), (Group::History, vec![Tab::History])]);
    }

    #[test]
    fn settings_never_enters_the_list() {
        let s = sections(&default_tabs());
        assert!(s.iter().all(|(_, v)| !v.contains(&Tab::Settings)));
        assert_eq!(group(Tab::Settings), None);
        let only_settings = ts(&[(Tab::Settings, true)]);
        assert!(sections(&only_settings).is_empty());
        assert!(grouped_indices(&only_settings, false).is_empty());
    }

    #[test]
    fn indices_include_hidden_when_asked() {
        let tabs = ts(&[(Tab::Cpu, false), (Tab::Projects, true), (Tab::Memory, true)]);
        assert_eq!(grouped_indices(&tabs, false), vec![(Group::System, vec![0, 2]), (Group::Developer, vec![1])]);
        assert_eq!(grouped_indices(&tabs, true), vec![(Group::System, vec![2]), (Group::Developer, vec![1])]);
    }
}
