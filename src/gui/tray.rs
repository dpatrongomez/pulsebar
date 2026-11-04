//! M4 — Menu bar. Each metric is an independent status item (shown/hidden/reordered from
//! Settings), rendered as number, sparkline or both, optionally framed, tinted red above its
//! threshold. Clicking opens a dropdown with the family's figures and its top 5 apps; clicking
//! an app opens the main window on that tab, scrolled to the app.
//!
//! AppKit status items must be touched on the main thread: the sampler thread schedules
//! `refresh()` on the main dispatch queue; state lives in a main-thread `thread_local`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::daemon::Shared;
use crate::i18n::I18n;
use crate::model::{MetricKind, RankMetric, Snapshot};
use crate::output::metric_value;
use crate::settings::{MenuBarItem, MenuDisplay, MenuMetric, Settings, Tab};
use crate::units::{Units, format_duration};

pub const LINES: usize = 3;
pub const TOP: usize = 5;

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

pub fn tab_for(m: MenuMetric) -> Tab {
    match m {
        MenuMetric::Cpu | MenuMetric::Temperature | MenuMetric::Fans => Tab::Cpu,
        MenuMetric::Memory => Tab::Memory,
        MenuMetric::Network => Tab::Network,
        MenuMetric::Disk => Tab::Disk,
        MenuMetric::Gpu => Tab::Gpu,
        MenuMetric::Neural => Tab::Neural,
        MenuMetric::Battery | MenuMetric::Power => Tab::Battery,
    }
}

pub fn rank_for(m: MenuMetric) -> RankMetric {
    match m {
        MenuMetric::Cpu | MenuMetric::Temperature | MenuMetric::Fans => RankMetric::Cpu,
        MenuMetric::Memory => RankMetric::Memory,
        MenuMetric::Network => RankMetric::Network,
        MenuMetric::Disk => RankMetric::Disk,
        MenuMetric::Gpu | MenuMetric::Neural | MenuMetric::Battery | MenuMetric::Power => RankMetric::Energy,
    }
}

pub fn spark_kind(m: MenuMetric) -> MetricKind {
    match m {
        MenuMetric::Cpu => MetricKind::Cpu,
        MenuMetric::Memory => MetricKind::Memory,
        MenuMetric::Network => MetricKind::Network,
        MenuMetric::Disk => MetricKind::Disk,
        MenuMetric::Gpu => MetricKind::Gpu,
        MenuMetric::Neural => MetricKind::Neural,
        MenuMetric::Temperature => MetricKind::Temperature,
        MenuMetric::Fans => MetricKind::Fans,
        MenuMetric::Battery => MetricKind::Battery,
        MenuMetric::Power => MetricKind::Power,
    }
}

/// (title, value compared to the threshold, gauge fraction 0..1)
pub fn title_and_value(m: MenuMetric, s: &Snapshot, u: &Units) -> (String, f64, f64) {
    let x = &s.machine;
    let short = |t: String| t.replace(' ', "");
    match m {
        MenuMetric::Cpu => (format!("{:.0}%", x.cpu.total_pct), x.cpu.total_pct, x.cpu.total_pct / 100.0),
        MenuMetric::Memory => {
            let pct = if x.memory.total > 0 { x.memory.used as f64 * 100.0 / x.memory.total as f64 } else { 0.0 };
            (format!("{pct:.0}%"), pct, pct / 100.0)
        }
        MenuMetric::Network => (
            format!("↓{} ↑{}", u.rate_short(x.network.rx_bps), u.rate_short(x.network.tx_bps)),
            x.network.rx_bps + x.network.tx_bps,
            0.0,
        ),
        MenuMetric::Disk => {
            let used = x.disk.boot_free().map(|v| 100.0 - v.free as f64 * 100.0 / v.total.max(1) as f64).unwrap_or(0.0);
            (short(u.rate(x.disk.read_bps + x.disk.write_bps)), used, used / 100.0)
        }
        MenuMetric::Gpu => (format!("{:.0}%", x.gpu.utilization_pct), x.gpu.utilization_pct, x.gpu.utilization_pct / 100.0),
        MenuMetric::Neural => (u.watts(x.neural.power_w, 1).replace(' ', ""), x.neural.utilization_pct, x.neural.utilization_pct / 100.0),
        MenuMetric::Temperature => {
            let t = x.sensors.cpu_temp_c.unwrap_or(0.0);
            (short(u.temp(t)), t, t / 110.0)
        }
        MenuMetric::Fans => {
            let rpm = x.sensors.fans.iter().map(|f| f.rpm).fold(0.0, f64::max);
            let max = x.sensors.fans.iter().map(|f| f.max_rpm).fold(0.0, f64::max).max(1.0);
            (if x.sensors.fans.is_empty() { "—".into() } else { format!("{rpm:.0}rpm") }, rpm, rpm / max)
        }
        MenuMetric::Battery => (format!("{:.0}%", x.battery.percent), 100.0 - x.battery.percent, x.battery.percent / 100.0),
        MenuMetric::Power => {
            let w = x.power.system_w.unwrap_or(0.0);
            (format!("{w:.0}W"), w, (w / 60.0).min(1.0))
        }
    }
}

/// The dropdown's figure lines (always exactly LINES).
pub fn lines(m: MenuMetric, s: &Snapshot, u: &Units, i18n: &I18n) -> Vec<String> {
    let x = &s.machine;
    let cores = x.cpu.core_count;
    let tf = |k: &str, a: &[(&str, &str)]| i18n.tf(k, a);
    let w1 = |w: f64| u.watts(w, 1);
    let w2 = |w: f64| u.watts(w, 2);
    let dash = || "—".to_string();
    let mut v = match m {
        MenuMetric::Cpu => vec![
            format!("CPU {}", u.cpu(x.cpu.total_pct, cores)),
            tf("tray.user_system", &[("user", &format!("{:.0}", x.cpu.user_pct)), ("system", &format!("{:.0}", x.cpu.system_pct))]),
            tf(
                "tray.load",
                &[("a", &i18n.num(format!("{:.2}", x.cpu.load_avg[0]))), ("b", &i18n.num(format!("{:.2}", x.cpu.load_avg[1]))), ("c", &i18n.num(format!("{:.2}", x.cpu.load_avg[2])))],
            ),
        ],
        MenuMetric::Memory => vec![
            tf("tray.memory_of", &[("used", &u.bytes(x.memory.used)), ("total", &u.bytes(x.memory.total))]),
            tf("tray.memory_types", &[("app", &u.bytes(x.memory.app)), ("wired", &u.bytes(x.memory.wired)), ("compressed", &u.bytes(x.memory.compressed))]),
            tf("tray.memory_cached", &[("cached", &u.bytes(x.memory.cached)), ("swap", &u.bytes(x.memory.swap_used))]),
        ],
        MenuMetric::Network => vec![
            tf("tray.down", &[("v", &u.rate(x.network.rx_bps))]),
            tf("tray.up", &[("v", &u.rate(x.network.tx_bps))]),
            i18n.tn("tray.interfaces", x.network.interfaces.iter().filter(|i| i.rx_bps + i.tx_bps > 0.0).count(), &[]),
        ],
        MenuMetric::Disk => {
            let b = x.disk.boot_free();
            vec![
                tf("tray.read_write", &[("r", &u.rate(x.disk.read_bps)), ("w", &u.rate(x.disk.write_bps))]),
                b.map(|v| tf("tray.free_of", &[("free", &u.bytes(v.free)), ("total", &u.bytes(v.total))])).unwrap_or_default(),
                i18n.tn("tray.volumes", x.disk.volumes.len(), &[]),
            ]
        }
        MenuMetric::Gpu => vec![
            format!("GPU {}", i18n.num(format!("{:.0} %", x.gpu.utilization_pct))),
            tf("tray.memory_in_use", &[("v", &u.bytes(x.gpu.vram_used))]),
            x.gpu.power_w.map(|w| tf("tray.power", &[("v", &w1(w))])).unwrap_or_else(|| x.gpu.name.clone()),
        ],
        MenuMetric::Neural => vec![
            format!("Neural Engine {}", w2(x.neural.power_w)),
            tf("tray.estimated_load", &[("v", &format!("{:.0}", x.neural.utilization_pct))]),
            if x.neural.available { i18n.t("tray.via_ioreport") } else { i18n.t("tray.not_available") },
        ],
        MenuMetric::Temperature => vec![
            format!("CPU {}", x.sensors.cpu_temp_c.map(|t| u.temp(t)).unwrap_or_else(dash)),
            format!("GPU {}", x.sensors.gpu_temp_c.map(|t| u.temp(t)).unwrap_or_else(dash)),
            i18n.tn("tray.sensors", x.sensors.all.len(), &[]),
        ],
        MenuMetric::Fans => {
            let mut l: Vec<String> = x
                .sensors
                .fans
                .iter()
                .map(|f| tf("tray.fan", &[("i", &(f.index + 1).to_string()), ("rpm", &format!("{:.0}", f.rpm)), ("max", &format!("{:.0}", f.max_rpm))]))
                .collect();
            if l.is_empty() {
                l.push(i18n.t("tray.no_fans"));
            }
            l
        }
        MenuMetric::Battery => vec![
            tf("tray.battery", &[("v", &format!("{:.0}", x.battery.percent)), ("state", &if x.battery.charging { i18n.t("tray.charging") } else { String::new() })]),
            x.battery.time_remaining_min.map(|t| tf("tray.remaining", &[("v", &format_duration(t * 60))])).unwrap_or_else(|| i18n.t("tray.calculating")),
            tf(
                "tray.health_cycles",
                &[
                    ("health", &x.battery.health_pct.map(|h| format!("{h:.0} %")).unwrap_or_else(dash)),
                    ("cycles", &x.battery.cycle_count.map(|c| i18n.tn("n.cycles", c.max(0) as usize, &[])).unwrap_or_else(dash)),
                ],
            ),
        ],
        MenuMetric::Power => vec![
            tf("tray.system", &[("v", &x.power.system_w.map(w1).unwrap_or_else(dash))]),
            format!("CPU {} · GPU {}", x.power.cpu_w.map(w1).unwrap_or_else(dash), x.power.gpu_w.map(w1).unwrap_or_else(dash)),
            format!("Neural Engine {}", x.power.ane_w.map(w2).unwrap_or_else(dash)),
        ],
    };
    v.resize(LINES, String::new());
    v
}

// --- icon rendering --------------------------------------------------------------------------

const W: usize = 44;
const H: usize = 32;

fn put(buf: &mut [u8], w: usize, x: i32, y: i32, c: [u8; 4]) {
    if x < 0 || y < 0 || x as usize >= w || y as usize >= buf.len() / 4 / w {
        return;
    }
    let i = (y as usize * w + x as usize) * 4;
    buf[i..i + 4].copy_from_slice(&c);
}

fn line(buf: &mut [u8], w: usize, (x0, y0): (i32, i32), (x1, y1): (i32, i32), c: [u8; 4]) {
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let (mut x, mut y, mut err) = (x0, y0, dx + dy);
    loop {
        put(buf, w, x, y, c);
        put(buf, w, x, y + 1, c);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// RGBA icon: sparkline (or gauge when no series), optional frame, red when over threshold.
pub fn render_icon(series: &[f64], fixed_max: Option<f64>, gauge: Option<f64>, framed: bool, red: bool) -> (Vec<u8>, u32, u32) {
    let mut buf = vec![0u8; W * H * 4];
    let c = if red { [230, 60, 60, 255] } else { [0, 0, 0, 255] };
    let (x0, y0, x1, y1) = (3i32, 5i32, W as i32 - 4, H as i32 - 6);
    if framed {
        for x in x0 - 2..=x1 + 2 {
            put(&mut buf, W, x, y0 - 3, c);
            put(&mut buf, W, x, y1 + 3, c);
        }
        for y in y0 - 3..=y1 + 3 {
            put(&mut buf, W, x0 - 2, y, c);
            put(&mut buf, W, x1 + 2, y, c);
        }
    }
    if let Some(g) = gauge {
        let fill = x0 + ((x1 - x0) as f64 * g.clamp(0.0, 1.0)) as i32;
        for y in y0 + 4..=y1 - 4 {
            for x in x0..=fill {
                put(&mut buf, W, x, y, c);
            }
        }
        for x in x0..=x1 {
            put(&mut buf, W, x, y1 - 3, [c[0], c[1], c[2], 90]);
        }
    } else if series.len() >= 2 {
        let hi = fixed_max.unwrap_or_else(|| series.iter().cloned().fold(0.0, f64::max)).max(1e-9);
        let n = series.len().min(40);
        let tail = &series[series.len() - n..];
        let pts: Vec<(i32, i32)> = tail
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = x0 + ((x1 - x0) as f64 * i as f64 / (n - 1).max(1) as f64) as i32;
                let y = y1 - ((y1 - y0) as f64 * (v / hi).clamp(0.0, 1.0)) as i32;
                (x, y)
            })
            .collect();
        for w2 in pts.windows(2) {
            line(&mut buf, W, w2[0], w2[1], c);
        }
    }
    (buf, W as u32, H as u32)
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
