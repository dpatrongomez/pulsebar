//! Small painter-based widgets: sparklines, line charts, meters, tiles; theme palettes.

use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use eframe::emath::GuiRounding;

use super::nothing;
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
        Theme::Nothing => Palette {
            accent: nothing::tokens(dark).text_display,
            danger: nothing::ACCENT,
            warn: nothing::WARNING,
        },
    }
}

pub fn num(settings: &Settings, s: impl Into<String>, size: f32) -> RichText {
    let t = RichText::new(s).size(size);
    if settings.theme == Theme::Nothing || settings.number_font == NumberFont::Monospace { t.monospace() } else { t }
}

/// The font [`num`] uses, for measuring and painting the same text.
pub fn num_font(settings: &Settings, size: f32) -> FontId {
    if settings.theme == Theme::Nothing || settings.number_font == NumberFont::Monospace { FontId::monospace(size) } else { FontId::proportional(size) }
}

/// The biggest number on a screen; dot-matrix in the Nothing theme.
pub fn hero_num(ctx: &egui::Context, settings: &Settings, s: impl Into<String>, size: f32) -> RichText {
    if nothing::is_active(ctx) { RichText::new(s).size(size).family(nothing::hero_family()) } else { num(settings, s, size) }
}

/// Font for numbers painted directly: Space Mono in the Nothing theme, one step smaller as it
/// runs wider than the proportional face the layouts were sized for.
pub fn data_font(ctx: &egui::Context, size: f32) -> FontId {
    if nothing::is_active(ctx) { FontId::monospace(size - 1.0) } else { FontId::proportional(size) }
}

/// Size of an all-caps instrument label, whatever size the other themes use at that spot.
const LABEL_SIZE: f32 = 11.0;
const LABEL_SPACING: f32 = 0.8;

/// Text and font of a label painted directly: all-caps Space Mono in the Nothing theme.
pub fn label_parts(ctx: &egui::Context, s: &str, size: f32) -> (String, FontId) {
    if nothing::is_active(ctx) { (s.to_uppercase(), FontId::monospace(LABEL_SIZE)) } else { (s.to_string(), FontId::proportional(size)) }
}

/// A weak label above or beside a value: all-caps Space Mono in the Nothing theme.
pub fn caption(ctx: &egui::Context, s: &str, size: f32) -> RichText {
    if nothing::is_active(ctx) {
        RichText::new(s.to_uppercase()).monospace().size(LABEL_SIZE).extra_letter_spacing(LABEL_SPACING).weak()
    } else {
        RichText::new(s).size(size).weak()
    }
}

/// [`caption`] for tight columns: one step smaller and without the extra tracking.
pub fn small_caption(ctx: &egui::Context, s: &str, size: f32) -> RichText {
    if nothing::is_active(ctx) { RichText::new(s.to_uppercase()).monospace().size(LABEL_SIZE - 1.0).weak() } else { RichText::new(s).size(size).weak() }
}

/// The unit next to a big number; label-sized in the Nothing theme.
pub fn unit(ctx: &egui::Context, s: &str, size: f32) -> RichText {
    if nothing::is_active(ctx) { RichText::new(s).monospace().size(LABEL_SIZE + 2.0).weak() } else { RichText::new(s).size(size).weak() }
}

/// The three memory kinds. Nothing tells them apart by opacity, not hue.
pub fn memory_colors(theme: Theme, dark: bool) -> [Color32; 3] {
    match theme {
        Theme::Nothing => {
            let c = nothing::tokens(dark).text_display;
            [c, c.gamma_multiply(0.65), c.gamma_multiply(0.42)]
        }
        _ => [metric_color(theme, dark, MetricKind::Cpu), metric_color(theme, dark, MetricKind::Sound), metric_color(theme, dark, MetricKind::Network)],
    }
}

/// (the "rest" of a breakdown, free space).
pub fn muted_colors(theme: Theme, dark: bool) -> (Color32, Color32) {
    match theme {
        Theme::Nothing => {
            let t = nothing::tokens(dark);
            (t.text_display.gamma_multiply(0.24), t.border_visible)
        }
        _ if dark => (Color32::from_gray(110), Color32::from_gray(60)),
        _ => (Color32::from_gray(170), Color32::from_gray(215)),
    }
}

/// Square-ended blocks with 2 px gaps, filled from the left. Never rounds a non-zero value down to
/// an empty bar. The instrument look of the Nothing theme.
fn segments(ui: &egui::Ui, rect: Rect, frac: f64, color: Color32) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let (seg, gap) = (4.0, 2.0);
    let n = ((rect.width() + gap) / (seg + gap)).floor().max(1.0) as usize;
    let frac = frac.clamp(0.0, 1.0);
    let filled = ((frac * n as f64).round() as usize).max(usize::from(frac > 0.0));
    let w = (rect.width() - gap * (n - 1) as f32) / n as f32;
    let empty = nothing::tokens(ui.visuals().dark_mode).border;
    let ppp = ui.ctx().pixels_per_point();
    let p = ui.painter();
    for i in 0..n {
        let x = rect.left() + i as f32 * (w + gap);
        let block = Rect::from_min_max(Pos2::new(x.round_to_pixels(ppp), rect.top()), Pos2::new((x + w).round_to_pixels(ppp), rect.bottom()));
        p.rect_filled(block, 0.0, if i < filled { color } else { empty });
    }
}

/// Line chart with min/max band for history buckets: (t, min, avg, max). The time axis spans
/// the first to the last point and the line is continuous.
pub fn history_chart(ui: &mut egui::Ui, i18n: &crate::i18n::I18n, pts: &[(i64, f64, f64, f64)], height: f32, color: Color32, fmt: &dyn Fn(f64) -> String) {
    let range = match (pts.first(), pts.last()) {
        (Some(a), Some(b)) => (a.0, b.0),
        _ => (0, 1),
    };
    history_chart_range(ui, i18n, pts, range, 0, height, color, fmt);
}

/// Like [`history_chart`], but the x axis covers `range` = (from, to) in seconds whatever data
/// there is, so partial history looks partial. With `step_s > 0` the line is broken wherever two
/// consecutive points are more than 1.5 steps apart (a gap with no data).
#[allow(clippy::too_many_arguments)]
pub fn history_chart_range(ui: &mut egui::Ui, i18n: &crate::i18n::I18n, pts: &[(i64, f64, f64, f64)], range: (i64, i64), step_s: i64, height: f32, color: Color32, fmt: &dyn Fn(f64) -> String) {
    let width = ui.available_width();
    let (resp, painter) = ui.allocate_painter(Vec2::new(width, height), Sense::hover());
    // Charts scrolled out of view cost nothing.
    if !ui.is_rect_visible(resp.rect) {
        return;
    }
    let r = resp.rect.shrink2(Vec2::new(4.0, 8.0));
    painter.rect_stroke(resp.rect, 6.0, Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);
    if pts.len() < 2 {
        painter.text(r.center(), egui::Align2::CENTER_CENTER, i18n.t("hist.not_enough"), egui::FontId::proportional(14.0), ui.visuals().weak_text_color());
        return;
    }
    let (t0, t1) = (range.0 as f64, range.1 as f64);
    let hi = pts.iter().map(|p| p.3).fold(0.0, f64::max).max(1e-9) * 1.1;
    let map = |t: i64, v: f64| Pos2::new(r.left() + r.width() * (((t as f64 - t0) / (t1 - t0).max(1.0)).clamp(0.0, 1.0)) as f32, r.bottom() - r.height() * (v / hi) as f32);
    // Split into runs of consecutive points.
    let gap = (step_s as f64 * 1.5) as i64;
    let mut runs: Vec<&[(i64, f64, f64, f64)]> = Vec::new();
    let mut start = 0;
    for i in 1..pts.len() {
        if step_s > 0 && pts[i].0 - pts[i - 1].0 > gap {
            runs.push(&pts[start..i]);
            start = i;
        }
    }
    runs.push(&pts[start..]);
    for run in runs {
        if run.len() == 1 {
            painter.circle_filled(map(run[0].0, run[0].2), 1.8, color);
            continue;
        }
        // Min/max band as thin quads (a polygon may be concave).
        for w in run.windows(2) {
            let quad = vec![map(w[0].0, w[0].3), map(w[1].0, w[1].3), map(w[1].0, w[1].1), map(w[0].0, w[0].1)];
            painter.add(egui::Shape::convex_polygon(quad, color.gamma_multiply(0.12), Stroke::NONE));
        }
        painter.add(egui::Shape::line(run.iter().map(|p| map(p.0, p.2)).collect(), Stroke::new(1.8, color)));
    }
    painter.text(r.left_top(), egui::Align2::LEFT_TOP, fmt(hi / 1.1), data_font(ui.ctx(), 11.0), ui.visuals().weak_text_color());
    if let Some(pos) = resp.hover_pos() {
        let frac = ((pos.x - r.left()) / r.width()).clamp(0.0, 1.0) as f64;
        let t = t0 + (t1 - t0) * frac;
        if let Some(p) = pts.iter().min_by_key(|p| ((p.0 as f64 - t).abs()) as i64) {
            let x = map(p.0, 0.0).x;
            painter.line_segment([Pos2::new(x, r.top()), Pos2::new(x, r.bottom())], Stroke::new(1.0, ui.visuals().weak_text_color()));
            let when = chrono::DateTime::from_timestamp(p.0, 0).map(|d| d.with_timezone(&chrono::Local).format(&i18n.t("hist.date_fmt")).to_string()).unwrap_or_default();
            resp.on_hover_text(format!("{when}\n{}", i18n.tf("hist.tip", &[("avg", &fmt(p.2)), ("min", &fmt(p.1)), ("max", &fmt(p.3))])));
        }
    }
}

/// Horizontal fill meter 0..1.
pub fn meter(ui: &mut egui::Ui, frac: f64, width: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 8.0), Sense::hover());
    if nothing::is_active(ui.ctx()) {
        segments(ui, rect, frac, color);
        return;
    }
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
    // Nothing: square ends, a gap between the parts, no rounded track.
    let (radius, gap, track) = if nothing::is_active(ui.ctx()) { (0.0, 2.0, nothing::tokens(ui.visuals().dark_mode).border) } else { (4.0, 0.0, ui.visuals().extreme_bg_color) };
    p.rect_filled(rect, radius, track);
    let total = total.max(parts.iter().map(|x| x.1).sum::<f64>()).max(1e-9);
    let mut x = rect.left();
    let mut tip = String::new();
    for (label, v, c) in parts {
        let w = rect.width() * (*v / total) as f32;
        p.rect_filled(Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new((w - gap).max(0.0), rect.height())), 0.0, *c);
        x += w;
        tip.push_str(&format!("{label}: {:.0} %\n", v / total * 100.0));
    }
    resp.on_hover_text(tip.trim_end());
}

/// Card background of the default look; the themes install theirs as `Visuals::window_fill`.
pub fn base_card_fill(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(30, 30, 33) } else { Color32::WHITE }
}

/// Card background for the current theme and light/dark mode.
pub fn card_fill(visuals: &egui::Visuals) -> Color32 {
    visuals.window_fill
}

/// Cards have a hairline only in the Nothing theme, which separates surfaces with borders.
pub fn card_stroke(ui: &egui::Ui) -> Stroke {
    if nothing::is_active(ui.ctx()) { Stroke::new(1.0, nothing::tokens(ui.visuals().dark_mode).border) } else { Stroke::NONE }
}

/// A clickable or hero card's background, for cards laid out by hand.
pub fn paint_card(ui: &egui::Ui, rect: Rect, fill: Color32) {
    ui.painter().rect(rect, 16.0, fill, card_stroke(ui), egui::StrokeKind::Inside);
}

/// Window background behind the cards.
pub fn window_fill(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(20, 20, 22) } else { Color32::from_rgb(242, 242, 245) }
}

/// Card frame.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new().fill(card_fill(ui.visuals())).stroke(card_stroke(ui)).corner_radius(14.0).inner_margin(14.0).show(ui, add).inner
}

/// One hue per metric so each Overview card is recognisable at a glance. The colour-blind
/// theme maps to Okabe–Ito hues and the monochrome theme to its accent.
pub fn metric_color(theme: Theme, dark: bool, kind: MetricKind) -> Color32 {
    match theme {
        Theme::Monochrome | Theme::Nothing => palette(theme, dark).accent,
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

/// A chart's line and band colour. The Nothing theme paints every chart in its signal red;
/// the other themes keep each metric's hue.
pub fn chart_color(theme: Theme, dark: bool, kind: MetricKind) -> Color32 {
    match theme {
        Theme::Nothing => nothing::ACCENT,
        _ => metric_color(theme, dark, kind),
    }
}

/// Vertical bar sparkline on a tinted rounded background; newest value on the right.
/// `max = None` scales to the series' own max.
pub fn bars(ui: &mut egui::Ui, values: &[f64], size: Vec2, color: Color32, max: Option<f64>) -> egui::Response {
    let (resp, painter) = ui.allocate_painter(size, Sense::hover());
    let r = resp.rect;
    if nothing::is_active(ui.ctx()) {
        // Square columns on the bare card: each slot shows its remainder, like an LED bar graph.
        let (bar_w, gap) = (4.0, 2.0);
        let slots = ((r.width() + gap) / (bar_w + gap)).floor().max(1.0) as usize;
        let shown = &values[values.len().saturating_sub(slots)..];
        let hi = max.unwrap_or_else(|| shown.iter().cloned().fold(0.0, f64::max)).max(1e-9);
        let empty = nothing::tokens(ui.visuals().dark_mode).border;
        let x0 = r.right() - slots as f32 * (bar_w + gap) + gap;
        let missing = slots - shown.len();
        for i in 0..slots {
            let x = x0 + i as f32 * (bar_w + gap);
            painter.rect_filled(Rect::from_min_max(Pos2::new(x, r.top()), Pos2::new(x + bar_w, r.bottom())), 0.0, empty);
            if let Some(v) = i.checked_sub(missing).map(|k| shown[k]) {
                let h = (r.height() * (v / hi).clamp(0.0, 1.0) as f32).max(2.0);
                painter.rect_filled(Rect::from_min_max(Pos2::new(x, r.bottom() - h), Pos2::new(x + bar_w, r.bottom())), 0.0, color);
            }
        }
        return resp;
    }
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
    // Nothing: no tinted tile behind the glyph.
    if !nothing::is_active(ui.ctx()) {
        p.rect_filled(r, size * 0.28, color.gamma_multiply(0.2));
    }
    p.text(r.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(size * 0.55), color);
}

/// Small rounded status pill ("✓ Normal").
pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    if nothing::is_active(ui.ctx()) {
        tag(ui, text, color);
        return;
    }
    let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(13.0), color);
    let size = galley.size() + Vec2::new(18.0, 8.0);
    let (r, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(r, r.height() / 2.0, color.gamma_multiply(0.16));
    ui.painter().galley(r.center() - galley.size() / 2.0, galley, color);
}

/// Nothing's tag: an outline with a technical radius and all-caps data type, coloured text only.
fn tag(ui: &mut egui::Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_uppercase(), FontId::monospace(LABEL_SIZE), color);
    let size = galley.size() + Vec2::new(12.0, 6.0);
    let (r, _) = ui.allocate_exact_size(size, Sense::hover());
    let border = nothing::tokens(ui.visuals().dark_mode).border_visible;
    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, border), egui::StrokeKind::Inside);
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
    // Nothing: a thin instrument gauge rather than a thick ring.
    let nothing_on = nothing::is_active(ui.ctx());
    let thickness = diameter * if nothing_on { 0.05 } else { 0.12 };
    let radius = diameter / 2.0 - thickness / 2.0;
    let c = r.center();
    let track = if nothing_on { nothing::tokens(ui.visuals().dark_mode).border } else { ui.visuals().extreme_bg_color };
    p.circle_stroke(c, radius, Stroke::new(thickness, track));
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
    p.text(c - Vec2::new(0.0, 7.0), egui::Align2::CENTER_CENTER, big, data_font(ui.ctx(), diameter * 0.15), strong);
    // Inside the ring a full-size all-caps label would touch it.
    let (small, small_font) = if nothing_on { (small.to_uppercase(), FontId::monospace(9.0)) } else { (small.to_string(), FontId::proportional(diameter * 0.1)) };
    p.text(c + Vec2::new(0.0, diameter * 0.1), egui::Align2::CENTER_CENTER, small, small_font, weak);
}

/// Area chart for the hero cards: faint horizontal grid, gradient fill, 2 px line.
pub fn area_chart(p: &egui::Painter, r: Rect, values: &[f64], color: Color32, max: Option<f64>, grid: Color32, empty: &str) {
    for i in 0..4 {
        let y = r.top() + r.height() * i as f32 / 3.0;
        p.line_segment([Pos2::new(r.left(), y), Pos2::new(r.right(), y)], Stroke::new(1.0, grid.gamma_multiply(0.25)));
    }
    if values.len() < 2 {
        p.text(r.center(), egui::Align2::CENTER_CENTER, empty, egui::FontId::proportional(13.0), grid);
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
    // The Nothing theme has no gradients: the bare line is the chart.
    if !nothing::is_active(p.ctx()) {
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
    }
    p.add(egui::Shape::line(pts, Stroke::new(2.0, color)));
}

/// Thin rounded progress bar (5 px) used in lists.
pub fn thin_meter(ui: &mut egui::Ui, frac: f64, width: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 5.0), Sense::hover());
    paint_thin_meter(ui, rect, frac, color);
}

/// [`thin_meter`] in a rect laid out by the caller.
pub fn paint_thin_meter(ui: &egui::Ui, rect: Rect, frac: f64, color: Color32) {
    if nothing::is_active(ui.ctx()) {
        segments(ui, rect, frac, color);
        return;
    }
    let p = ui.painter();
    p.rect_filled(rect, 2.5, ui.visuals().extreme_bg_color);
    let mut f = rect;
    f.set_width((rect.width() * frac.clamp(0.0, 1.0) as f32).max(if frac > 0.0 { 3.0 } else { 0.0 }));
    p.rect_filled(f, 2.5, color);
}

/// Small tinted chip ("8 P", "Playing").
pub fn chip(ui: &mut egui::Ui, text: &str, color: Color32) {
    if nothing::is_active(ui.ctx()) {
        tag(ui, text, color);
        return;
    }
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
    /// Chevron pointing up — move a row earlier in a list.
    MoveUp,
    /// Chevron pointing down — move a row later in a list.
    MoveDown,
    /// Ring with an "i" — open the process inspector.
    Inspect,
    /// Three bars — open the app's detail view with its history.
    Details,
}

/// Round, frameless icon button for row actions. The glyph is weak until hovered, then
/// takes `color` over a tinted circle. `tip` is the tooltip and the accessible name.
pub fn icon_button(ui: &mut egui::Ui, icon: ActionIcon, color: Color32, tip: &str) -> egui::Response {
    let size = 26.0;
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tip));
    let enabled = ui.is_enabled();
    let resp = resp.on_hover_text(tip).on_disabled_hover_text(tip);
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    let p = ui.painter();
    let active = enabled && (resp.hovered() || resp.has_focus());
    if resp.is_pointer_button_down_on() {
        p.circle_filled(r.center(), size / 2.0, color.gamma_multiply(0.32));
    } else if active {
        p.circle_filled(r.center(), size / 2.0, color.gamma_multiply(0.18));
    }
    let fg = if active {
        color
    } else if enabled {
        ui.visuals().weak_text_color()
    } else {
        ui.visuals().weak_text_color().gamma_multiply(0.4)
    };
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
        ActionIcon::MoveUp | ActionIcon::MoveDown => {
            let k = if matches!(icon, ActionIcon::MoveUp) { -1.0 } else { 1.0 };
            // `k` puts the apex above (up) or below (down) the centre.
            let pts = vec![c + Vec2::new(-4.5, -2.0 * k), c + Vec2::new(0.0, 2.0 * k), c + Vec2::new(4.5, -2.0 * k)];
            p.add(egui::Shape::line(pts, stroke));
        }
        ActionIcon::Inspect => {
            p.circle_stroke(c, 7.0, Stroke::new(1.5, fg));
            p.line_segment([c + Vec2::new(0.0, -0.6), c + Vec2::new(0.0, 3.2)], stroke);
            p.circle_filled(c + Vec2::new(0.0, -3.2), 1.1, fg);
        }
        ActionIcon::Details => {
            for (dx, h) in [(-4.5, 5.0), (0.0, 9.0), (4.5, 7.0)] {
                p.line_segment([c + Vec2::new(dx, 5.0), c + Vec2::new(dx, 5.0 - h)], Stroke::new(2.4, fg));
            }
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_charts_use_the_signal_red_and_other_themes_keep_the_metric_hue() {
        assert_eq!(chart_color(Theme::Nothing, true, MetricKind::Cpu), nothing::ACCENT);
        assert_eq!(chart_color(Theme::Nothing, false, MetricKind::Disk), nothing::ACCENT);
        assert_eq!(chart_color(Theme::Brand, true, MetricKind::Cpu), metric_color(Theme::Brand, true, MetricKind::Cpu));
    }
}
