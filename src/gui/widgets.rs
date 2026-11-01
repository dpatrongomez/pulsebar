//! Small painter-based widgets: sparklines, line charts, meters, tiles; theme palettes.

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::model::MetricKind;
use crate::settings::{NumberFont, Settings, Theme};

#[derive(Clone, Copy)]
pub struct Palette {
    pub accent: Color32,
    pub danger: Color32,
    pub warn: Color32,
}

pub fn palette(theme: Theme, dark: bool) -> Palette {
    match theme {
        Theme::Monochrome => {
            let g = if dark { Color32::from_gray(220) } else { Color32::from_gray(40) };
            Palette {
                accent: g,
                danger: Color32::from_rgb(220, 60, 60),
                warn: Color32::from_gray(160),
            }
        }
        Theme::Brand => Palette {
            accent: Color32::from_rgb(22, 176, 110),
            danger: Color32::from_rgb(230, 70, 70),
            warn: Color32::from_rgb(245, 166, 35),
        },
        // Okabe–Ito: distinguishable for protanopia/deuteranopia/tritanopia.
        Theme::ColorBlind => Palette {
            accent: Color32::from_rgb(0, 114, 178),
            danger: Color32::from_rgb(213, 94, 0),
            warn: Color32::from_rgb(240, 228, 66),
        },
    }
}

pub fn num(settings: &Settings, s: impl Into<String>, size: f32) -> RichText {
    let t = RichText::new(s).size(size);
    match settings.number_font {
        NumberFont::Monospace => t.monospace(),
        NumberFont::Proportional => t,
    }
}

/// Line chart with min/max band for history buckets: (t, min, avg, max).
pub fn history_chart(ui: &mut egui::Ui, pts: &[(i64, f64, f64, f64)], height: f32, color: Color32, fmt: &dyn Fn(f64) -> String) {
    let width = ui.available_width();
    let (resp, painter) = ui.allocate_painter(Vec2::new(width, height), Sense::hover());
    let r = resp.rect.shrink2(Vec2::new(4.0, 8.0));
    painter.rect_stroke(resp.rect, 6.0, Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);
    if pts.len() < 2 {
        painter.text(r.center(), egui::Align2::CENTER_CENTER, "Not enough history yet", egui::FontId::proportional(14.0), ui.visuals().weak_text_color());
        return;
    }
    let t0 = pts[0].0 as f64;
    let t1 = pts[pts.len() - 1].0 as f64;
    let hi = pts.iter().map(|p| p.3).fold(0.0, f64::max).max(1e-9) * 1.1;
    let map = |t: i64, v: f64| Pos2::new(r.left() + r.width() * ((t as f64 - t0) / (t1 - t0).max(1.0)) as f32, r.bottom() - r.height() * (v / hi) as f32);
    let mut band: Vec<Pos2> = pts.iter().map(|p| map(p.0, p.3)).collect();
    band.extend(pts.iter().rev().map(|p| map(p.0, p.1)));
    // Band as thin quads (polygon may be concave).
    for w in pts.windows(2) {
        let quad = vec![map(w[0].0, w[0].3), map(w[1].0, w[1].3), map(w[1].0, w[1].1), map(w[0].0, w[0].1)];
        painter.add(egui::Shape::convex_polygon(quad, color.gamma_multiply(0.12), Stroke::NONE));
    }
    painter.add(egui::Shape::line(pts.iter().map(|p| map(p.0, p.2)).collect(), Stroke::new(1.8, color)));
    painter.text(r.left_top(), egui::Align2::LEFT_TOP, fmt(hi / 1.1), egui::FontId::proportional(11.0), ui.visuals().weak_text_color());
    if let Some(pos) = resp.hover_pos() {
        let frac = ((pos.x - r.left()) / r.width()).clamp(0.0, 1.0) as f64;
        let t = t0 + (t1 - t0) * frac;
        if let Some(p) = pts.iter().min_by_key(|p| ((p.0 as f64 - t).abs()) as i64) {
            let x = map(p.0, 0.0).x;
            painter.line_segment([Pos2::new(x, r.top()), Pos2::new(x, r.bottom())], Stroke::new(1.0, ui.visuals().weak_text_color()));
            let when = chrono::DateTime::from_timestamp(p.0, 0).map(|d| d.with_timezone(&chrono::Local).format("%b %d %H:%M").to_string()).unwrap_or_default();
            resp.on_hover_text(format!("{when}\navg {}  min {}  max {}", fmt(p.2), fmt(p.1), fmt(p.3)));
        }
    }
}

/// Horizontal fill meter 0..1.
pub fn meter(ui: &mut egui::Ui, frac: f64, width: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 8.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let mut f = rect;
    f.set_width(rect.width() * frac.clamp(0.0, 1.0) as f32);
    p.rect_filled(f, 4.0, color);
}

/// Stacked breakdown bar: (label, value, color).
pub fn stacked_bar(ui: &mut egui::Ui, parts: &[(String, f64, Color32)], total: f64) {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 14.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let total = total.max(parts.iter().map(|x| x.1).sum::<f64>()).max(1e-9);
    let mut x = rect.left();
    let mut tip = String::new();
    for (label, v, c) in parts {
        let w = rect.width() * (*v / total) as f32;
        p.rect_filled(Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(w, rect.height())), 0.0, *c);
        x += w;
        tip.push_str(&format!("{label}: {:.0} %\n", v / total * 100.0));
    }
    resp.on_hover_text(tip.trim_end());
}

/// Card background for the current light/dark mode.
pub fn card_fill(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(30, 30, 33) } else { Color32::WHITE }
}

/// Window background behind the cards.
pub fn window_fill(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(20, 20, 22) } else { Color32::from_rgb(242, 242, 245) }
}

/// Card frame.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new().fill(card_fill(ui.visuals().dark_mode)).corner_radius(14.0).inner_margin(14.0).show(ui, add).inner
}

/// One hue per metric so each Overview card is recognisable at a glance. The colour-blind
/// theme maps to Okabe–Ito hues and the monochrome theme to its accent.
pub fn metric_color(theme: Theme, dark: bool, kind: MetricKind) -> Color32 {
    match theme {
        Theme::Monochrome => palette(theme, dark).accent,
        Theme::ColorBlind => match kind {
            MetricKind::Cpu | MetricKind::Sound => Color32::from_rgb(0, 114, 178),
            MetricKind::Memory | MetricKind::Neural => Color32::from_rgb(204, 121, 167),
            MetricKind::Gpu | MetricKind::Temperature => Color32::from_rgb(213, 94, 0),
            MetricKind::Disk | MetricKind::Fans => Color32::from_rgb(230, 159, 0),
            MetricKind::Network | MetricKind::Bluetooth => Color32::from_rgb(86, 180, 233),
            MetricKind::Battery | MetricKind::Power => Color32::from_rgb(0, 158, 115),
        },
        Theme::Brand => match kind {
            MetricKind::Cpu | MetricKind::Bluetooth => Color32::from_rgb(59, 130, 246),
            MetricKind::Memory => Color32::from_rgb(139, 124, 246),
            MetricKind::Gpu | MetricKind::Temperature => Color32::from_rgb(232, 72, 122),
            MetricKind::Disk | MetricKind::Fans => Color32::from_rgb(224, 160, 40),
            MetricKind::Network => Color32::from_rgb(34, 180, 110),
            MetricKind::Battery | MetricKind::Power => Color32::from_rgb(52, 190, 80),
            MetricKind::Neural => Color32::from_rgb(124, 108, 240),
            MetricKind::Sound => Color32::from_rgb(236, 110, 60),
        },
    }
}

/// Vertical bar sparkline on a tinted rounded background; newest value on the right.
/// `max = None` scales to the series' own max.
pub fn bars(ui: &mut egui::Ui, values: &[f64], size: Vec2, color: Color32, max: Option<f64>) -> egui::Response {
    let (resp, painter) = ui.allocate_painter(size, Sense::hover());
    let r = resp.rect;
    painter.rect_filled(r, 8.0, color.gamma_multiply(0.14));
    let inner = r.shrink2(Vec2::new(8.0, 5.0));
    let (bar_w, gap) = (4.0, 3.0);
    let slots = ((inner.width() + gap) / (bar_w + gap)).floor().max(1.0) as usize;
    let shown = &values[values.len().saturating_sub(slots)..];
    let hi = max.unwrap_or_else(|| shown.iter().cloned().fold(0.0, f64::max)).max(1e-9);
    // Right-align so the newest sample always sits at the right edge.
    let x0 = inner.right() - shown.len() as f32 * (bar_w + gap) + gap;
    for (i, v) in shown.iter().enumerate() {
        let h = (inner.height() * (v / hi).clamp(0.0, 1.0) as f32).max(2.0);
        let x = x0 + i as f32 * (bar_w + gap);
        let bar = Rect::from_min_max(Pos2::new(x, inner.bottom() - h), Pos2::new(x + bar_w, inner.bottom()));
        painter.rect_filled(bar, 1.5, color);
    }
    resp
}

/// Rounded square with a tinted background and a glyph in the metric colour.
pub fn icon_badge(ui: &mut egui::Ui, glyph: &str, color: Color32, size: f32) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, size * 0.28, color.gamma_multiply(0.2));
    p.text(r.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(size * 0.55), color);
}

/// Small rounded status pill ("✓ Normal").
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(13.0), color);
    let size = galley.size() + Vec2::new(18.0, 8.0);
    let (r, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(r, r.height() / 2.0, color.gamma_multiply(0.16));
    ui.painter().galley(r.center() - galley.size() / 2.0, galley, color);
}

/// Coloured dot used in legends and stat labels.
pub fn dot(ui: &mut egui::Ui, color: Color32) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(r.center(), 3.5, color);
}

/// Ring chart: segments drawn clockwise from 12 o'clock with a small gap between them,
/// two lines of text in the middle.
pub fn donut(ui: &mut egui::Ui, diameter: f32, parts: &[(f64, Color32)], big: &str, small: &str) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    let p = ui.painter();
    let thickness = diameter * 0.12;
    let radius = diameter / 2.0 - thickness / 2.0;
    let c = r.center();
    p.circle_stroke(c, radius, Stroke::new(thickness, ui.visuals().extreme_bg_color));
    let total: f64 = parts.iter().map(|x| x.0.max(0.0)).sum();
    if total > 0.0 {
        let tau = std::f32::consts::TAU;
        let gap = if parts.iter().filter(|x| x.0 > 0.0).count() > 1 { 0.03 } else { 0.0 };
        let mut a = -tau / 4.0;
        for (v, color) in parts {
            let sweep = tau * (v.max(0.0) / total) as f32;
            if sweep - gap > 0.002 {
                let (a0, a1) = (a + gap / 2.0, a + sweep - gap / 2.0);
                let steps = ((a1 - a0) / 0.04).ceil().max(2.0) as usize;
                let pts: Vec<Pos2> = (0..=steps).map(|i| {
                    let t = a0 + (a1 - a0) * i as f32 / steps as f32;
                    c + Vec2::new(t.cos(), t.sin()) * radius
                }).collect();
                p.add(egui::Shape::line(pts, Stroke::new(thickness, *color)));
            }
            a += sweep;
        }
    }
    let strong = ui.visuals().strong_text_color();
    let weak = ui.visuals().weak_text_color();
    p.text(c - Vec2::new(0.0, 7.0), egui::Align2::CENTER_CENTER, big, egui::FontId::proportional(diameter * 0.15), strong);
    p.text(c + Vec2::new(0.0, diameter * 0.1), egui::Align2::CENTER_CENTER, small, egui::FontId::proportional(diameter * 0.1), weak);
}

/// Area chart for the hero cards: faint horizontal grid, gradient fill, 2 px line.
pub fn area_chart(p: &egui::Painter, r: Rect, values: &[f64], color: Color32, max: Option<f64>, grid: Color32) {
    for i in 0..4 {
        let y = r.top() + r.height() * i as f32 / 3.0;
        p.line_segment([Pos2::new(r.left(), y), Pos2::new(r.right(), y)], Stroke::new(1.0, grid.gamma_multiply(0.25)));
    }
    if values.len() < 2 {
        p.text(r.center(), egui::Align2::CENTER_CENTER, "Collecting data…", egui::FontId::proportional(13.0), grid);
        return;
    }
    let hi = max.unwrap_or_else(|| values.iter().cloned().fold(0.0, f64::max) * 1.15).max(1e-9);
    let n = values.len();
    let pts: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| Pos2::new(r.left() + r.width() * i as f32 / (n - 1) as f32, r.bottom() - r.height() * (v / hi).clamp(0.0, 1.0) as f32))
        .collect();
    // Gradient fill as a triangle strip: line colour fading to transparent at the bottom.
    let mut mesh = egui::Mesh::default();
    let top_c = color.gamma_multiply(0.35);
    let bottom_c = color.gamma_multiply(0.02);
    for (i, pt) in pts.iter().enumerate() {
        mesh.colored_vertex(*pt, top_c);
        mesh.colored_vertex(Pos2::new(pt.x, r.bottom()), bottom_c);
        if i > 0 {
            let k = (i * 2) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k, k + 1);
        }
    }
    p.add(egui::Shape::mesh(mesh));
    p.add(egui::Shape::line(pts, Stroke::new(2.0, color)));
}

/// Thin rounded progress bar (5 px) used in lists.
pub fn thin_meter(ui: &mut egui::Ui, frac: f64, width: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 5.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.5, ui.visuals().extreme_bg_color);
    let mut f = rect;
    f.set_width((rect.width() * frac.clamp(0.0, 1.0) as f32).max(if frac > 0.0 { 3.0 } else { 0.0 }));
    p.rect_filled(f, 2.5, color);
}

/// Small tinted chip ("8 P", "Playing").
pub fn chip(ui: &mut egui::Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(12.5), color);
    let size = galley.size() + Vec2::new(14.0, 6.0);
    let (r, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(r, 7.0, color.gamma_multiply(0.18));
    ui.painter().galley(r.center() - galley.size() / 2.0, galley, color);
}

#[derive(Clone, Copy)]
pub enum ActionIcon {
    /// × — ask the process to quit.
    Quit,
    /// Power symbol — force quit (SIGKILL).
    ForceQuit,
}

/// Round, frameless icon button for row actions. The glyph is weak until hovered, then
/// takes `color` over a tinted circle. `tip` is the tooltip and the accessible name.
pub fn icon_button(ui: &mut egui::Ui, icon: ActionIcon, color: Color32, tip: &str) -> egui::Response {
    let size = 26.0;
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tip));
    let resp = resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand);
    let p = ui.painter();
    let active = resp.hovered() || resp.has_focus();
    if resp.is_pointer_button_down_on() {
        p.circle_filled(r.center(), size / 2.0, color.gamma_multiply(0.32));
    } else if active {
        p.circle_filled(r.center(), size / 2.0, color.gamma_multiply(0.18));
    }
    let fg = if active { color } else { ui.visuals().weak_text_color() };
    let stroke = Stroke::new(1.8, fg);
    let c = r.center();
    match icon {
        ActionIcon::Quit => {
            let d = 4.5;
            p.line_segment([c + Vec2::new(-d, -d), c + Vec2::new(d, d)], stroke);
            p.line_segment([c + Vec2::new(d, -d), c + Vec2::new(-d, d)], stroke);
        }
        ActionIcon::ForceQuit => {
            // Arc open at the top (from 1 o'clock round to 11 o'clock) plus a stem.
            let rad = 6.0;
            let (a0, a1) = (-std::f32::consts::FRAC_PI_2 + 0.75, 3.0 * std::f32::consts::FRAC_PI_2 - 0.75);
            let pts: Vec<Pos2> = (0..=24).map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / 24.0;
                c + Vec2::new(a.cos(), a.sin()) * rad
            }).collect();
            p.add(egui::Shape::line(pts, stroke));
            p.line_segment([c + Vec2::new(0.0, -rad - 1.0), c + Vec2::new(0.0, -1.0)], stroke);
        }
    }
    resp
}
