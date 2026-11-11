//! The Nothing theme: design tokens, bundled fonts and egui visuals. Monochrome, flat, type-led:
//! OLED black or off-white paper, 1 px borders instead of shadows, Space Grotesk for text,
//! Space Mono for data and labels, Doto for the one hero number on a screen.
//!
//! Painter-drawn widgets ask [`is_active`] so the other themes keep their exact look.

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Id, Shadow, Stroke, TextStyle};

/// Signal red: errors, destructive actions, the active marker. Same in both modes.
pub const ACCENT: Color32 = Color32::from_rgb(0xD7, 0x19, 0x21);
/// Same in both modes.
pub const WARNING: Color32 = Color32::from_rgb(0xD4, 0xA8, 0x43);

const FONT_TEXT: &str = "SpaceGrotesk-Regular";
const FONT_DATA: &str = "SpaceMono-Regular";
const FONT_HERO: &str = "Doto-Bold";
const FAMILY_HERO: &str = "nothing-hero";

#[derive(Clone, Copy)]
pub struct Tokens {
    pub page: Color32,
    pub surface: Color32,
    pub surface_raised: Color32,
    pub border: Color32,
    pub border_visible: Color32,
    pub text_secondary: Color32,
    pub text_primary: Color32,
    pub text_display: Color32,
    pub interactive: Color32,
}

pub const fn tokens(dark: bool) -> Tokens {
    const fn gray(v: u8) -> Color32 {
        Color32::from_rgb(v, v, v)
    }
    if dark {
        Tokens {
            page: gray(0x00),
            surface: gray(0x11),
            surface_raised: gray(0x1A),
            border: gray(0x22),
            border_visible: gray(0x33),
            text_secondary: gray(0x99),
            text_primary: gray(0xE8),
            text_display: gray(0xFF),
            interactive: Color32::from_rgb(0x5B, 0x9B, 0xF6),
        }
    } else {
        Tokens {
            page: gray(0xF5),
            surface: gray(0xFF),
            surface_raised: gray(0xF0),
            border: gray(0xE8),
            border_visible: gray(0xCC),
            text_secondary: gray(0x66),
            text_primary: gray(0x1A),
            text_display: gray(0x00),
            interactive: Color32::from_rgb(0x00, 0x7A, 0xFF),
        }
    }
}

fn flag() -> Id {
    Id::unique("nothing-theme-active")
}

/// Records the theme where widgets that only have a `Ui` can read it.
pub fn set_active(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(flag(), on));
}

pub fn is_active(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp(flag())).unwrap_or(false)
}

/// The family for hero numbers (falls back to Space Mono for glyphs Doto lacks).
pub fn hero_family() -> FontFamily {
    FontFamily::Name(FAMILY_HERO.into())
}

/// egui's default fonts, with the bundled ones in front when `on`. The defaults stay as fallbacks
/// for the symbols and scripts the bundled fonts do not cover.
///
/// The hero family is bound either way: new fonts only take effect on the next frame while
/// [`is_active`] flips at once, and egui panics on a family with no fonts.
pub fn fonts(on: bool) -> FontDefinitions {
    let mut f = FontDefinitions::default();
    if !on {
        let mono = f.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
        f.families.insert(hero_family(), mono);
        return f;
    }
    let mut add = |name: &str, bytes: &'static [u8]| {
        f.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add(FONT_TEXT, include_bytes!("../../assets/fonts/SpaceGrotesk-Regular.ttf"));
    add(FONT_DATA, include_bytes!("../../assets/fonts/SpaceMono-Regular.ttf"));
    add(FONT_HERO, include_bytes!("../../assets/fonts/Doto-Bold.ttf"));
    let mut hero = vec![FONT_HERO.to_owned(), FONT_DATA.to_owned()];
    if let Some(mono) = f.families.get_mut(&FontFamily::Monospace) {
        hero.extend(mono.iter().cloned());
        mono.insert(0, FONT_DATA.to_owned());
    }
    if let Some(prop) = f.families.get_mut(&FontFamily::Proportional) {
        prop.insert(0, FONT_TEXT.to_owned());
    }
    f.families.insert(hero_family(), hero);
    f
}

/// Applies the tokens to one mode's style. Expects a style freshly reset to egui's defaults.
pub fn apply(style: &mut egui::Style, dark: bool) {
    let t = tokens(dark);
    if let Some(button) = style.text_styles.get_mut(&TextStyle::Button) {
        button.family = FontFamily::Monospace;
    }
    let v = &mut style.visuals;
    v.panel_fill = t.page;
    v.window_fill = t.surface;
    v.faint_bg_color = t.surface_raised;
    v.extreme_bg_color = t.page;
    v.code_bg_color = t.surface_raised;
    v.window_stroke = Stroke::new(1.0, t.border_visible);
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.window_corner_radius = CornerRadius::same(16);
    v.menu_corner_radius = CornerRadius::same(8);
    v.hyperlink_color = t.interactive;
    v.warn_fg_color = WARNING;
    v.error_fg_color = ACCENT;
    v.weak_text_color = Some(t.text_secondary);
    v.selection.bg_fill = t.border_visible;
    v.selection.stroke = Stroke::new(1.0, t.text_display);
    v.text_cursor.stroke = Stroke::new(2.0, t.text_display);

    let w = &mut v.widgets;
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = CornerRadius::same(4);
        // Hover brightens; nothing grows.
        s.expansion = 0.0;
    }
    w.noninteractive.bg_fill = t.surface;
    w.noninteractive.weak_bg_fill = t.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text_primary);
    w.inactive.bg_fill = t.border_visible;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::new(1.0, t.border_visible);
    w.inactive.fg_stroke = Stroke::new(1.0, t.text_primary);
    w.hovered.bg_fill = t.border_visible;
    w.hovered.weak_bg_fill = t.surface_raised;
    w.hovered.bg_stroke = Stroke::new(1.0, t.text_primary);
    w.hovered.fg_stroke = Stroke::new(1.0, t.text_display);
    w.active.bg_fill = t.border_visible;
    w.active.weak_bg_fill = t.surface_raised;
    w.active.bg_stroke = Stroke::new(1.0, t.text_display);
    w.active.fg_stroke = Stroke::new(1.0, t.text_display);
    w.open.bg_fill = t.surface_raised;
    w.open.weak_bg_fill = t.surface_raised;
    w.open.bg_stroke = Stroke::new(1.0, t.border_visible);
    w.open.fg_stroke = Stroke::new(1.0, t.text_display);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_fonts_lead_their_families() {
        let f = fonts(true);
        assert_eq!(f.families[&FontFamily::Proportional][0], FONT_TEXT);
        assert_eq!(f.families[&FontFamily::Monospace][0], FONT_DATA);
        assert_eq!(f.families[&hero_family()][..2], [FONT_HERO.to_owned(), FONT_DATA.to_owned()]);
        // The defaults stay behind them for symbols the bundled fonts lack.
        assert!(f.families[&FontFamily::Proportional].len() > 1);
        assert!(f.families[&hero_family()].len() > 2);
    }

    #[test]
    fn hero_family_exists_without_the_bundled_fonts() {
        let f = fonts(false);
        assert!(!f.families[&hero_family()].is_empty());
        assert!(!f.font_data.contains_key(FONT_HERO), "other themes do not load the bundled fonts");
    }

    #[test]
    fn modes_invert_text_and_page() {
        assert_eq!(tokens(true).page, Color32::BLACK);
        assert_eq!(tokens(true).text_display, Color32::WHITE);
        assert_eq!(tokens(false).surface, Color32::WHITE);
        assert_eq!(tokens(false).text_display, Color32::BLACK);
    }

    #[test]
    fn flag_defaults_off_and_follows_the_setting() {
        let ctx = egui::Context::default();
        assert!(!is_active(&ctx));
        set_active(&ctx, true);
        assert!(is_active(&ctx));
        set_active(&ctx, false);
        assert!(!is_active(&ctx));
    }
}
