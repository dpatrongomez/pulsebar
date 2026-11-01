//! Small painter-based widgets: sparklines, line charts, meters, tiles; theme palettes.

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::settings::{NumberFont, Settings, Theme};

#[derive(Clone, Copy)]
pub struct Palette {
    pub accent: Color32,
    pub series: [Color32; 4],
    pub danger: Color32,
    pub warn: Color32,
    pub ok: Color32,
}

pub fn palette(theme: Theme, dark: bool) -> Palette {
    match theme {
        Theme::Monochrome => {
            let g = if dark { Color32::from_gray(220) } else { Color32::from_gray(40) };
            Palette {
                accent: g,
                series: [g, Color32::from_gray(150), Color32::from_gray(110), Color32::from_gray(80)],
                danger: Color32::from_rgb(220, 60, 60),
                warn: Color32::from_gray(160),
                ok: g,
            }
        }
        Theme::Brand => Palette {
            accent: Color32::from_rgb(22, 176, 110),
            series: [
                Color32::from_rgb(22, 176, 110),
                Color32::from_rgb(64, 132, 245),
                Color32::from_rgb(245, 166, 35),
                Color32::from_rgb(170, 100, 230),
            ],
            danger: Color32::from_rgb(230, 70, 70),
            warn: Color32::from_rgb(245, 166, 35),
            ok: Color32::from_rgb(22, 176, 110),
        },
        // Okabe–Ito: distinguishable for protanopia/deuteranopia/tritanopia.
        Theme::ColorBlind => Palette {
            accent: Color32::from_rgb(0, 114, 178),
            series: [
                Color32::from_rgb(0, 114, 178),
                Color32::from_rgb(230, 159, 0),
                Color32::from_rgb(0, 158, 115),
                Color32::from_rgb(204, 121, 167),
            ],
            danger: Color32::from_rgb(213, 94, 0),
            warn: Color32::from_rgb(240, 228, 66),
            ok: Color32::from_rgb(0, 158, 115),
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

/// Sparkline; `max = None` scales to the series' own max.
pub fn sparkline(ui: &mut egui::Ui, values: &[f64], size: Vec2, color: Color32, max: Option<f64>) -> egui::Response {
    let (resp, painter) = ui.allocate_painter(size, Sense::hover());
    let r = resp.rect;
    if values.len() >= 2 {
        let hi = max.unwrap_or_else(|| values.iter().cloned().fold(0.0, f64::max)).max(1e-9);
        let n = values.len();
        let pts: Vec<Pos2> = values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = r.left() + r.width() * i as f32 / (n - 1) as f32;
                let y = r.bottom() - r.height() * (v / hi).clamp(0.0, 1.0) as f32;
                Pos2::new(x, y)
            })
            .collect();
        let mut fill = pts.clone();
        fill.push(Pos2::new(r.right(), r.bottom()));
        fill.push(Pos2::new(r.left(), r.bottom()));
        painter.add(egui::Shape::convex_polygon(fill, color.gamma_multiply(0.15), Stroke::NONE));
        painter.add(egui::Shape::line(pts, Stroke::new(1.5, color)));
    }
    resp
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

/// Card frame.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::group(ui.style()).corner_radius(10.0).inner_margin(12.0).show(ui, add).inner
}
