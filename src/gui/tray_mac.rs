//! macOS menu bar items.
//!
//! AppKit status items must be touched on the main thread: the sampler thread schedules
//! `refresh()` on the main dispatch queue; state lives in a main-thread `thread_local`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use super::tray::{LINES, TOP, lines, put, rank_for, render_icon, spark_kind, tab_for, title_and_value};
use crate::daemon::Shared;
use crate::i18n::I18n;
use crate::output::metric_value;
use crate::settings::{MenuBarItem, MenuDisplay, MenuMetric, Settings, Tab};
use crate::units::Units;

struct Item {
    metric: MenuMetric,
    tray: TrayIcon,
    lines: Vec<MenuItem>,
    apps: Vec<MenuItem>,
    /// Last values pushed to AppKit: only changes are sent (each push redraws the menu bar).
    last_title: RefCell<Option<String>>,
    last_icon: RefCell<Option<(Vec<u8>, bool)>>,
    last_texts: RefCell<Vec<String>>,
}

impl Item {
    fn set_text(&self, idx: usize, mi: &MenuItem, text: &str) {
        let mut t = self.last_texts.borrow_mut();
        if t.len() <= idx {
            t.resize(idx + 1, String::new());
        }
        if t[idx] != text {
            mi.set_text(text);
            t[idx] = text.to_string();
        }
    }
}

struct State {
    shared: Arc<Shared>,
    items: Vec<Item>,
    signature: String,
    /// menu id → (tab, app key)
    targets: HashMap<String, (Tab, String)>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn red_dot() -> (Vec<u8>, u32, u32) {
    let (w, h) = (16usize, 32usize);
    let mut buf = vec![0u8; w * h * 4];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            if (x - 8).pow(2) + (y - 16).pow(2) <= 25 {
                put(&mut buf, w, x, y, [230, 60, 60, 255]);
            }
        }
    }
    (buf, w as u32, h as u32)
}

// --- lifecycle -------------------------------------------------------------------------------

/// Menu bar layout plus language: either changing rebuilds the status items (their static
/// labels are set at build time).
fn signature(items: &[MenuBarItem], i18n: &I18n) -> String {
    format!("{}|{}", i18n.lang(), serde_json::to_string(items).unwrap_or_default())
}

fn build_item(it: &MenuBarItem, i18n: &I18n) -> Option<Item> {
    let menu = Menu::new();
    let id = |s: String| s;
    let m = it.metric;
    let key = format!("{m:?}").to_lowercase();
    let lines: Vec<MenuItem> = (0..LINES).map(|i| MenuItem::with_id(id(format!("pb:{key}:line:{i}")), "…", false, None)).collect();
    for l in &lines {
        let _ = menu.append(l);
    }
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(format!("pb:{key}:hdr"), i18n.t("top_apps"), false, None));
    let apps: Vec<MenuItem> = (0..TOP).map(|i| MenuItem::with_id(format!("pb:{key}:app:{i}"), "—", false, None)).collect();
    for a in &apps {
        let _ = menu.append(a);
    }
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(format!("pb:open:{key}"), i18n.tf("tray.open", &[("name", &i18n.t(m.key()))]), true, None));
    let _ = menu.append(&MenuItem::with_id("pb:open:overview", i18n.t("tray.open_app"), true, None));
    let _ = menu.append(&MenuItem::with_id("pb:quit", i18n.t("tray.quit_app"), true, None));
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_title("…")
        .with_tooltip(format!("PulseBar · {}", i18n.t(m.key())))
        .build()
        .ok()?;
    Some(Item { metric: m, tray, lines, apps, last_title: RefCell::new(None), last_icon: RefCell::new(None), last_texts: RefCell::new(vec![]) })
}

fn build(settings: &Settings, i18n: &I18n) -> Vec<Item> {
    // macOS places newer status items to the left: create right-most first.
    let visible: Vec<&MenuBarItem> = settings.menubar.iter().filter(|i| i.visible).collect();
    let mut items: Vec<Item> = visible.iter().rev().filter_map(|it| build_item(it, i18n)).collect();
    items.reverse();
    items
}

/// Main thread only.
pub fn install(shared: Arc<Shared>) {
    let settings = shared.settings.read().unwrap().clone();
    let i18n = I18n::new(settings.language.as_deref());
    let items = build(&settings, &i18n);
    STATE.with(|s| {
        *s.borrow_mut() = Some(State { shared, items, signature: signature(&settings.menubar, &i18n), targets: HashMap::new() });
    });
    refresh();
}

/// Resolve a dropdown click. Main thread only.
pub fn target_for(menu_id: &str) -> Option<(Tab, Option<String>)> {
    if menu_id == "pb:open:overview" {
        return Some((Tab::Overview, None));
    }
    if let Some(key) = menu_id.strip_prefix("pb:open:") {
        let m = MenuMetric::ALL.into_iter().find(|m| format!("{m:?}").to_lowercase() == key)?;
        return Some((tab_for(m), None));
    }
    STATE.with(|s| s.borrow().as_ref().and_then(|st| st.targets.get(menu_id).cloned())).map(|(t, k)| (t, Some(k)))
}

/// Update titles, icons and dropdowns from the latest snapshot. Main thread only.
pub fn refresh() {
    let t0 = std::time::Instant::now();
    refresh_inner();
    if std::env::var_os("PULSEBAR_PROFILE").is_some() {
        eprintln!("tray refresh {:.2} ms", t0.elapsed().as_secs_f64() * 1e3);
    }
}

fn refresh_inner() {
    STATE.with(|cell| {
        let mut guard = cell.borrow_mut();
        let Some(st) = guard.as_mut() else { return };
        let settings = st.shared.settings.read().unwrap().clone();
        // The agent process reloads settings.json when it changes (see daemon::run), so a
        // language switch made in the window reaches the menu bar within a sampling tick.
        let i18n = I18n::new(settings.language.as_deref());
        let sig = signature(&settings.menubar, &i18n);
        if sig != st.signature {
            st.items.clear(); // drops (removes) old status items
            st.items = build(&settings, &i18n);
            st.signature = sig;
        }
        let Some(snap) = st.shared.snapshot.read().unwrap().clone() else { return };
        let sparks = st.shared.sparks.read().unwrap().clone();
        let u = Units { decimal_comma: i18n.decimal_comma(), ..settings.units };
        st.targets.clear();
        for item in &st.items {
            let Some(cfg) = settings.menubar.iter().find(|c| c.metric == item.metric) else { continue };
            let (title, value, gauge) = title_and_value(item.metric, &snap, &u);
            let red = cfg.threshold.is_some_and(|t| value > t);
            let kind = spark_kind(item.metric);
            let fixed = matches!(item.metric, MenuMetric::Cpu | MenuMetric::Gpu | MenuMetric::Battery).then_some(100.0);
            let series = sparks.get(kind);
            let icon = match cfg.display {
                MenuDisplay::Number if cfg.framed => Some(render_icon(&[], None, Some(gauge), true, red)),
                MenuDisplay::Number if red => Some(red_dot()),
                MenuDisplay::Number => None,
                MenuDisplay::Sparkline | MenuDisplay::Both => Some(render_icon(&series, fixed, None, cfg.framed, red)),
            };
            let show_title = !matches!(cfg.display, MenuDisplay::Sparkline);
            let new_icon = icon.as_ref().map(|(rgba, _, _)| (rgba.clone(), red));
            if *item.last_icon.borrow() != new_icon {
                match icon {
                    Some((rgba, w, h)) => {
                        if let Ok(i) = Icon::from_rgba(rgba, w, h) {
                            let _ = if red { item.tray.set_icon(Some(i)) } else { item.tray.set_icon_templated(Some(i)) };
                        }
                    }
                    None => {
                        let _ = item.tray.set_icon(None);
                    }
                }
                *item.last_icon.borrow_mut() = new_icon;
            }
            let new_title = show_title.then_some(title);
            if *item.last_title.borrow() != new_title {
                item.tray.set_title(new_title.clone());
                *item.last_title.borrow_mut() = new_title;
            }
            for (i, (mi, text)) in item.lines.iter().zip(lines(item.metric, &snap, &u, &i18n)).enumerate() {
                item.set_text(i, mi, if text.is_empty() { " " } else { &text });
            }
            let rank = rank_for(item.metric);
            let top = snap.top_apps(rank, TOP);
            let key = format!("{:?}", item.metric).to_lowercase();
            for (i, mi) in item.apps.iter().enumerate() {
                match top.get(i) {
                    Some(a) => {
                        let text = i18n.tf(
                            "tray.app_line",
                            &[("name", &a.app.name), ("value", &metric_value(a, rank, &u, snap.machine.cpu.core_count)), ("n", &a.process_count.to_string())],
                        );
                        item.set_text(LINES + i, mi, &text);
                        mi.set_enabled(true);
                        st.targets.insert(format!("pb:{key}:app:{i}"), (tab_for(item.metric), a.key.clone()));
                    }
                    None => {
                        item.set_text(LINES + i, mi, "—");
                        mi.set_enabled(false);
                    }
                }
            }
        }
    });
}

/// From any thread: schedule `refresh()` on the main queue.
pub fn schedule_refresh() {
    #[cfg(target_os = "macos")]
    dispatch2::DispatchQueue::main().exec_async(refresh);
}
