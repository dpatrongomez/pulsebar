//! Modal alert used by the quit confirmation and the MCP approval prompt: dimmed backdrop,
//! fixed-width rounded card, icon + bold title + detail chips, Cancel and a primary action
//! right-aligned. Everything is painter-drawn (egui's default fonts have no symbol glyphs).

use eframe::egui::{self, Align, Color32, FontId, Key, Pos2, Rect, RichText, Sense, Stroke, TextureId, Vec2};

use super::widgets::card_fill;

const CONTENT_W: f32 = 340.0;
const ICON: f32 = 52.0;

pub enum DialogIcon {
    /// An app's own icon (with a small danger badge when `badge` is set).
    App { tex: TextureId, badge: Option<Color32> },
    /// Warning triangle on a tinted rounded square.
    Warning(Color32),
}

pub struct Dialog<'a> {
    pub id: &'a str,
    pub icon: DialogIcon,
    pub title: String,
    /// What the dialog is about, in regular weight under the title (optional).
    pub subject: Option<String>,
    /// Weak lines under the title.
    pub lines: Vec<String>,
    /// Small pills (process count, memory, ports).
    pub chips: Vec<String>,
    pub cancel: String,
    pub confirm: String,
    pub confirm_color: Color32,
    /// Whether Return confirms (Escape always cancels).
    pub enter_confirms: bool,
}

/// Shows the dialog. `Some(true)` = confirmed, `Some(false)` = cancelled, `None` = still open.
pub fn show(ctx: &egui::Context, d: &Dialog) -> Option<bool> {
    let dark = ctx.global_style().visuals.dark_mode;
    // Keys only count once the dialog was on screen in the previous frame, so the Return
    // that opened it (keyboard-activated button) cannot also confirm it.
    let key = egui::Id::unique(("dialog-open-frame", d.id));
    let frame_nr = ctx.cumulative_frame_nr();
    let armed = ctx.data(|m| m.get_temp::<u64>(key)).is_some_and(|f| f + 1 == frame_nr);
    ctx.data_mut(|m| m.insert_temp(key, frame_nr));

    let frame = egui::Frame::new()
        .fill(if dark { Color32::from_rgb(38, 38, 42) } else { card_fill(false) })
        .stroke(Stroke::new(1.0, if dark { Color32::from_white_alpha(24) } else { Color32::from_black_alpha(28) }))
        .corner_radius(16.0)
        .inner_margin(22.0)
        .shadow(egui::epaint::Shadow { offset: [0, 10], blur: 36, spread: 0, color: Color32::from_black_alpha(if dark { 150 } else { 70 }) });
    let mut decided: Option<bool> = None;
    let mut cancel_focused = false;
    egui::Modal::new(egui::Id::unique(("dialog", d.id)))
        .backdrop_color(Color32::from_black_alpha(if dark { 150 } else { 90 }))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(CONTENT_W);
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                icon(ui, &d.icon, dark);
                ui.vertical(|ui| {
                    ui.set_width(CONTENT_W - ICON - 16.0);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.add(egui::Label::new(RichText::new(&d.title).size(17.0).strong()).wrap());
                    if let Some(sub) = &d.subject {
                        ui.add(egui::Label::new(RichText::new(sub).size(14.0)).wrap());
                    }
                    for l in &d.lines {
                        ui.add(egui::Label::new(RichText::new(l).size(13.0).weak()).wrap());
                    }
                    if !d.chips.is_empty() {
                        ui.add_space(6.0);
                        chips(ui, &d.chips, dark);
                    }
                });
            });
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if button(ui, &d.confirm, Some(d.confirm_color), dark).clicked() {
                    decided = Some(true);
                }
                let cancel = button(ui, &d.cancel, None, dark);
                cancel_focused = cancel.has_focus();
                if cancel.clicked() {
                    decided = Some(false);
                }
            });
        });
    if decided.is_none() && armed {
        let (esc, enter) = ctx.input(|i| (i.key_pressed(Key::Escape), i.key_pressed(Key::Enter)));
        if esc || (enter && cancel_focused) {
            decided = Some(false);
        } else if enter && d.enter_confirms {
            decided = Some(true);
        }
    }
    if decided.is_some() {
        ctx.data_mut(|m| m.remove::<u64>(key));
    }
    decided
}

fn icon(ui: &mut egui::Ui, icon: &DialogIcon, dark: bool) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(ICON), Sense::hover());
    let p = ui.painter();
    match icon {
        DialogIcon::App { tex, badge } => {
            let img = r.shrink(2.0);
            p.image(*tex, img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            if let Some(c) = badge {
                let ring = if dark { Color32::from_rgb(38, 38, 42) } else { Color32::WHITE };
                let at = r.right_bottom() - Vec2::splat(9.0);
                p.circle_filled(at, 11.0, ring);
                p.circle_filled(at, 9.0, *c);
                bang(p, at, 9.0, Color32::WHITE, 1.8);
            }
        }
        DialogIcon::Warning(c) => {
            p.rect_filled(r, 14.0, c.gamma_multiply(if dark { 0.20 } else { 0.16 }));
            let ctr = r.center() + Vec2::new(0.0, 1.0);
            let s = 13.0;
            let pts = vec![ctr + Vec2::new(0.0, -s), ctr + Vec2::new(s * 1.1, s * 0.8), ctr + Vec2::new(-s * 1.1, s * 0.8)];
            let stroke = Stroke::new(2.6, *c);
            p.add(egui::Shape::closed_line(pts.iter().map(|q| ctr + (*q - ctr) * 0.86).collect(), stroke));
            // Round the joints.
            for q in &pts {
                p.circle_filled(ctr + (*q - ctr) * 0.86, 1.3, *c);
            }
            bang(p, ctr + Vec2::new(0.0, 2.0), 9.0, *c, 2.4);
        }
    }
}

/// An exclamation mark centred on `c`, `h` tall.
fn bang(p: &egui::Painter, c: Pos2, h: f32, color: Color32, w: f32) {
    let stroke = Stroke::new(w, color);
    p.line_segment([c + Vec2::new(0.0, -h * 0.45), c + Vec2::new(0.0, h * 0.1)], stroke);
    p.circle_filled(c + Vec2::new(0.0, h * 0.42), w * 0.62, color);
}

/// Chips in rows that never exceed the text column (egui's own wrapped layout lets a wide
/// frame push the row, and the dialog, wider).
fn chips(ui: &mut egui::Ui, items: &[String], dark: bool) {
    let col = CONTENT_W - ICON - 16.0;
    let (gap, pad) = (6.0, 18.0);
    let font = FontId::proportional(12.5);
    let mut rows: Vec<Vec<&String>> = vec![vec![]];
    let mut x = 0.0;
    for it in items {
        let w = (ui.painter().layout_no_wrap(it.clone(), font.clone(), Color32::WHITE).size().x + pad).min(col);
        if x > 0.0 && x + gap + w > col {
            rows.push(vec![]);
            x = 0.0;
        }
        x += if x > 0.0 { gap + w } else { w };
        rows.last_mut().unwrap().push(it);
    }
    ui.spacing_mut().item_spacing = Vec2::new(gap, 6.0);
    for row in rows {
        ui.horizontal(|ui| {
            for c in row {
                chip(ui, c, dark, col - pad);
            }
        });
    }
}

fn chip(ui: &mut egui::Ui, text: &str, dark: bool, max_text_w: f32) {
    let fill = if dark { Color32::from_white_alpha(16) } else { Color32::from_black_alpha(12) };
    egui::Frame::new().fill(fill).corner_radius(8.0).inner_margin(egui::Margin::symmetric(9, 3)).show(ui, |ui| {
        // A chip never grows past the text column; a very long one wraps inside it.
        ui.set_max_width(max_text_w);
        ui.add(egui::Label::new(RichText::new(text).size(12.5).color(ui.visuals().text_color().gamma_multiply(0.9))).wrap());
    });
}

/// Rounded 32 px button: `primary` is the fill colour of the prominent action, `None` gives the
/// quiet secondary style.
fn button(ui: &mut egui::Ui, text: &str, primary: Option<Color32>, dark: bool) -> egui::Response {
    let fg0 = match primary {
        Some(c) => on_color(c),
        None => ui.visuals().text_color(),
    };
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(14.0), fg0);
    let size = Vec2::new((galley.size().x + 36.0).max(88.0), 32.0);
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), text));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let mut fill = match primary {
        Some(c) => c,
        None => if dark { Color32::from_white_alpha(20) } else { Color32::from_black_alpha(15) },
    };
    if resp.is_pointer_button_down_on() {
        fill = if primary.is_some() { fill.lerp_to_gamma(Color32::BLACK, 0.2) } else { fill.gamma_multiply(1.8) };
    } else if resp.hovered() {
        fill = if primary.is_some() { fill.lerp_to_gamma(Color32::WHITE, 0.12) } else { fill.gamma_multiply(1.4) };
    }
    let p = ui.painter();
    p.rect_filled(r, 9.0, fill);
    if resp.has_focus() {
        p.rect_stroke(r.expand(2.0), 11.0, Stroke::new(2.0, primary.unwrap_or(ui.visuals().text_color()).gamma_multiply(0.6)), egui::StrokeKind::Outside);
    }
    let fg = if primary.is_some() { fg0 } else { ui.visuals().strong_text_color() };
    let galley = p.layout_no_wrap(text.to_string(), FontId::proportional(14.0), fg);
    let pos = r.center() - galley.size() / 2.0;
    p.galley(pos, galley, fg);
    resp
}

/// Text colour with the better contrast on a solid fill.
fn on_color(c: Color32) -> Color32 {
    let lin = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    let l = 0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b());
    // Contrast against white is 1.05 / (l + 0.05); against near-black (l ≈ 0.01) it is (l + 0.05) / 0.06.
    if 1.05 / (l + 0.05) >= (l + 0.05) / 0.06 { Color32::WHITE } else { Color32::from_rgb(18, 18, 20) }
}
