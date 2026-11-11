//! Linux status icons: one StatusNotifierItem per metric shown in Settings → Menu bar, over D-Bus
//! (KDE, Cinnamon, XFCE, sway bars, and GNOME with the AppIndicator extension). Status icons have
//! no text beside them as macOS menu bar items do, so a "number" is drawn into the icon and the
//! full figures are in the tooltip and the menu.

use std::sync::Arc;

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, ToolTip};

use super::tray::{LINES, TOP, lines, rank_for, render_icon, spark_kind, tab_for, title_and_value};
use crate::daemon::Shared;
use crate::i18n::I18n;
use crate::output::metric_value;
use crate::settings::{MenuBarItem, MenuDisplay, MenuMetric, Tab};
use crate::units::Units;

const SIZE: usize = 32;

/// 5×7 bitmap digits, one row per byte.
const DIGITS: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

/// The leading number of a menu bar title ("42%" → "42"), when it's short enough to draw.
fn leading_digits(title: &str) -> Option<String> {
    let d: String = title.chars().take_while(char::is_ascii_digit).collect();
    (!d.is_empty() && d.len() <= 3).then_some(d)
}

/// Up to three digits, as large as they fit.
fn digits_icon(digits: &str, red: bool) -> (Vec<u8>, u32, u32) {
    let mut buf = vec![0u8; SIZE * SIZE * 4];
    let scale = if digits.len() <= 2 { 3 } else { 2 };
    let (cw, gap) = (5 * scale, scale);
    let width = digits.len() * cw + (digits.len().saturating_sub(1)) * gap;
    let (x0, y0) = (SIZE.saturating_sub(width) / 2, SIZE.saturating_sub(7 * scale) / 2);
    let c = if red { [230, 60, 60, 255] } else { [0, 0, 0, 255] };
    for (i, ch) in digits.chars().enumerate() {
        let Some(glyph) = ch.to_digit(10).map(|d| DIGITS[d as usize]) else { continue };
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                for sy in 0..scale {
                    for sx in 0..scale {
                        let (x, y) = (x0 + i * (cw + gap) + col * scale + sx, y0 + row * scale + sy);
                        if x < SIZE && y < SIZE {
                            buf[(y * SIZE + x) * 4..(y * SIZE + x) * 4 + 4].copy_from_slice(&c);
                        }
                    }
                }
            }
        }
    }
    (buf, SIZE as u32, SIZE as u32)
}

/// The shared renderer draws black for a macOS template image. Panels here may be dark or light
/// and nothing says which, so draw light pixels with a dark outline: readable on both.
fn for_any_panel(buf: &mut [u8], w: usize) {
    let h = buf.len() / 4 / w;
    let src = buf.to_vec();
    let alpha = |x: i32, y: i32| {
        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
            return 0;
        }
        src[(y as usize * w + x as usize) * 4 + 3]
    };
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if src[i + 3] > 0 {
                if src[i..i + 3] == [0, 0, 0] {
                    buf[i..i + 3].copy_from_slice(&[240, 240, 240]);
                }
            } else {
                let (xi, yi) = (x as i32, y as i32);
                if alpha(xi - 1, yi) > 128 || alpha(xi + 1, yi) > 128 || alpha(xi, yi - 1) > 128 || alpha(xi, yi + 1) > 128 {
                    buf[i..i + 4].copy_from_slice(&[0, 0, 0, 170]);
                }
            }
        }
    }
}

/// RGBA → the ARGB (network byte order) that StatusNotifierItem wants.
fn to_argb(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect()
}

fn status_icon(cfg: &MenuBarItem, title: &str, series: &[f64], red: bool) -> Icon {
    let fixed = matches!(cfg.metric, MenuMetric::Cpu | MenuMetric::Gpu | MenuMetric::Battery).then_some(100.0);
    let number = if matches!(cfg.display, MenuDisplay::Number) { leading_digits(title) } else { None };
    let (mut rgba, w, h) = match number {
        Some(d) => digits_icon(&d, red),
        None => render_icon(series, fixed, None, cfg.framed, red),
    };
    for_any_panel(&mut rgba, w as usize);
    Icon { width: w as i32, height: h as i32, data: to_argb(&rgba) }
}

#[derive(Clone)]
struct StatusItem {
    id: String,
    tab: Tab,
    icon: Option<Icon>,
    title: String,
    tooltip: String,
    /// The fixed part of the menu, in the current language.
    open_tab: String,
    open_app: String,
    quit: String,
    top_header: String,
    lines: Vec<String>,
    /// (label, app key)
    apps: Vec<(String, String)>,
}

fn action(label: &str, f: impl Fn(&mut StatusItem) + Send + 'static) -> MenuItem<StatusItem> {
    StandardItem { label: label.replace('_', "__"), activate: Box::new(f), ..Default::default() }.into()
}

fn info(label: &str) -> MenuItem<StatusItem> {
    StandardItem { label: label.replace('_', "__"), enabled: false, ..Default::default() }.into()
}

impl ksni::Tray for StatusItem {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icon.iter().map(|i| Icon { width: i.width, height: i.height, data: i.data.clone() }).collect()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: self.title.clone(), description: self.tooltip.clone(), ..Default::default() }
    }

    /// A left click opens the window on this metric's tab.
    fn activate(&mut self, _x: i32, _y: i32) {
        super::open_window(self.tab, None);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut m: Vec<MenuItem<Self>> = self.lines.iter().filter(|l| !l.is_empty()).map(|l| info(l)).collect();
        m.push(MenuItem::Separator);
        m.push(info(&self.top_header));
        for (label, key) in &self.apps {
            let (tab, key) = (self.tab, key.clone());
            m.push(action(label, move |_| super::open_window(tab, Some(key.clone()))));
        }
        m.push(MenuItem::Separator);
        let tab = self.tab;
        m.push(action(&self.open_tab, move |_| super::open_window(tab, None)));
        m.push(action(&self.open_app, |_| super::open_window(Tab::Overview, None)));
        m.push(action(&self.quit, |_| super::quit_agent()));
        m
    }
}

/// The running status icons, kept in step with the settings and the latest snapshot.
pub struct Agent {
    shared: Arc<Shared>,
    items: Vec<(MenuMetric, Handle<StatusItem>)>,
    signature: String,
}

fn signature(items: &[MenuBarItem], i18n: &I18n) -> String {
    format!("{}|{}", i18n.lang(), serde_json::to_string(items).unwrap_or_default())
}

impl Agent {
    pub fn new(shared: Arc<Shared>) -> Self {
        let mut a = Self { shared, items: vec![], signature: String::new() };
        a.refresh();
        a
    }

    /// Whether anything can show the icons. Without a session bus or a status notifier host (stock
    /// GNOME), there is no way to reach the app from a panel.
    pub fn has_host(&self) -> bool {
        !self.items.is_empty()
    }

    fn rebuild(&mut self, settings: &[MenuBarItem], i18n: &I18n) {
        for (_, h) in self.items.drain(..) {
            let _ = h.shutdown();
        }
        for cfg in settings.iter().filter(|c| c.visible && c.metric.supported()) {
            let key = format!("{:?}", cfg.metric).to_lowercase();
            let item = StatusItem {
                id: format!("pulsebar-{key}"),
                tab: tab_for(cfg.metric),
                icon: None,
                title: i18n.t(cfg.metric.key()),
                tooltip: String::new(),
                open_tab: i18n.tf("tray.open", &[("name", &i18n.t(cfg.metric.key()))]),
                open_app: i18n.t("tray.open_app"),
                quit: i18n.t("tray.quit_app"),
                top_header: i18n.t("top_apps"),
                lines: vec![String::new(); LINES],
                apps: vec![],
            };
            match item.spawn() {
                Ok(h) => self.items.push((cfg.metric, h)),
                Err(e) => {
                    eprintln!("pulsebar: no status icons: {e}");
                    break;
                }
            }
        }
    }

    /// Update every icon from the latest snapshot.
    pub fn refresh(&mut self) {
        let settings = self.shared.settings.read().unwrap().clone();
        let i18n = I18n::new(settings.language.as_deref());
        let sig = signature(&settings.menubar, &i18n);
        if sig != self.signature {
            self.rebuild(&settings.menubar, &i18n);
            self.signature = sig;
        }
        let Some(snap) = self.shared.snapshot.read().unwrap().clone() else { return };
        let sparks = self.shared.sparks.read().unwrap().clone();
        let u = Units { decimal_comma: i18n.decimal_comma(), ..settings.units };
        for (metric, handle) in &self.items {
            let Some(cfg) = settings.menubar.iter().find(|c| c.metric == *metric) else { continue };
            let (title, value, _) = title_and_value(*metric, &snap, &u);
            let red = cfg.threshold.is_some_and(|t| value > t);
            let icon = status_icon(cfg, &title, &sparks.get(spark_kind(*metric)), red);
            let figures = lines(*metric, &snap, &u, &i18n);
            let rank = rank_for(*metric);
            let apps: Vec<(String, String)> = snap
                .top_apps(rank, TOP)
                .iter()
                .map(|a| {
                    let text = i18n.tf("tray.app_line", &[("name", &a.app.name), ("value", &metric_value(a, rank, &u, snap.machine.cpu.core_count)), ("n", &a.process_count.to_string())]);
                    (text, a.key.clone())
                })
                .collect();
            let tooltip = figures.iter().filter(|l| !l.is_empty()).cloned().collect::<Vec<_>>().join("\n");
            let new_title = format!("PulseBar · {} · {title}", i18n.t(metric.key()));
            handle.update(move |t| {
                t.icon = Some(icon);
                t.title = new_title;
                t.tooltip = tooltip;
                t.lines = figures;
                t.apps = apps;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_come_from_the_start_of_the_title() {
        assert_eq!(leading_digits("42%").as_deref(), Some("42"));
        assert_eq!(leading_digits("100%").as_deref(), Some("100"));
        assert_eq!(leading_digits("↓1K ↑2K"), None);
        assert_eq!(leading_digits("12345rpm"), None, "doesn't fit");
    }

    #[test]
    fn icons_fit_and_read_on_any_panel() {
        let (mut rgba, w, h) = digits_icon("100", false);
        assert_eq!((w, h), (32, 32));
        assert!(rgba.chunks_exact(4).any(|p| p[3] == 255), "drew something");
        for_any_panel(&mut rgba, w as usize);
        assert!(rgba.chunks_exact(4).any(|p| p == [240, 240, 240, 255]), "light digits");
        assert!(rgba.chunks_exact(4).any(|p| p == [0, 0, 0, 170]), "with a dark outline");
        let (mut red, w, _) = digits_icon("9", true);
        for_any_panel(&mut red, w as usize);
        assert!(red.chunks_exact(4).any(|p| p == [230, 60, 60, 255]), "red stays red");
    }

    #[test]
    fn argb_is_in_network_order() {
        assert_eq!(to_argb(&[1, 2, 3, 4]), vec![4, 1, 2, 3]);
    }
}
