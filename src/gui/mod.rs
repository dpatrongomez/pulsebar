//! Native UI, split into two processes (see `daemon` for why):
//! * `run_agent`  — menu bar status items on a bare NSApplication run loop (no GPU window);
//! * `run_window` — the egui window, launched on demand and exiting when closed.

mod app;
mod dialog;
mod icons;
mod shortcuts;
pub mod tray;
mod widgets;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::daemon::{self, Role};
use crate::settings::{Settings, Tab};
use crate::{actions, paths, sys};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Nav {
    pub tab: Tab,
    pub app: Option<String>,
    pub at_ms: i64,
}

/// Bring this process's windows to the front. Main thread only.
pub fn show_app() {
    #[cfg(target_os = "macos")]
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        app.unhide(None);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

/// Make the window process a regular app: Dock icon, Cmd-Tab entry, and a window that stays put
/// (behind other windows) when another app is activated. The bundle is `LSUIElement` (so the
/// menu bar agent has no Dock icon) and winit leaves the policy alone in bundled apps, so without
/// this the window process would be an Accessory app too: no way back to its window once covered.
/// Main thread only. The process exits when the window closes, which removes the Dock icon again.
fn become_regular_app() {
    #[cfg(target_os = "macos")]
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Regular);
    }
}

fn window_running() -> bool {
    std::fs::read_to_string(paths::window_pid_file())
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .is_some_and(|pid| pid > 1 && sys::pid_alive(pid))
}

/// Open (or focus) the window on a tab, optionally scrolled to an app.
pub fn open_window(tab: Tab, app: Option<String>) {
    let nav = Nav { tab, app: app.clone(), at_ms: sys::now_ms() };
    let _ = paths::ensure_dirs();
    let _ = std::fs::write(paths::nav_file(), serde_json::to_vec(&nav).unwrap_or_default());
    if window_running() {
        return; // the window polls nav.json and activates itself
    }
    if let Ok(exe) = std::env::current_exe() {
        let tab_s = serde_json::to_value(tab).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "overview".into());
        let mut cmd = std::process::Command::new(exe);
        cmd.args(["window", "--tab", &tab_s]);
        if let Some(a) = app {
            cmd.args(["--app", &a]);
        }
        let _ = cmd.spawn();
    }
}

/// Menu bar agent: tray items + sampler (history, alerts, live.json). No dock icon.
pub fn run_agent() -> anyhow::Result<()> {
    let _ = paths::ensure_dirs();
    let settings = Settings::load();
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        let mtm = objc2::MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("must run on the main thread"))?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let shared: Arc<daemon::Shared> = daemon::spawn(settings, Role::Agent);
        let last_prompted = std::sync::Mutex::new(String::new());
        *shared.on_tick.lock().unwrap() = Some(Box::new(move || {
            tray::schedule_refresh();
            // An MCP quit request needs a human: make sure the window is up.
            if let Some(first) = actions::list_pending().first() {
                let mut lp = last_prompted.lock().unwrap();
                if *lp != first.id {
                    *lp = first.id.clone();
                    open_window(Tab::Overview, None);
                }
            }
        }));
        tray::install(shared.clone());
        tray_icon::menu::MenuEvent::set_event_handler(Some(|e: tray_icon::menu::MenuEvent| {
            let id = e.id.0.clone();
            if id == "pb:quit" {
                if let Ok(pid) = std::fs::read_to_string(paths::window_pid_file())
                    && let Ok(pid) = pid.trim().parse::<i32>()
                        && sys::pid_alive(pid) {
                            unsafe { libc::kill(pid, libc::SIGTERM) };
                        }
                std::process::exit(0);
            }
            if let Some((tab, app)) = tray::target_for(&id) {
                open_window(tab, app);
            }
        }));
        if std::env::var_os("PULSEBAR_NO_WINDOW").is_none() && !settings_has_launched_before() {
            open_window(Tab::Overview, None);
        }
        app.run();
    }
    Ok(())
}

fn settings_has_launched_before() -> bool {
    let marker = paths::app_support().join(".launched");
    let seen = marker.exists();
    let _ = std::fs::write(marker, b"1");
    seen
}

/// Dock / Cmd-Tab icon for the window process. Without one, eframe sets its own default egui
/// logo at runtime, overriding the bundle icon. Inside PulseBar.app an empty `IconData` tells
/// eframe to leave the icon alone, so macOS uses AppIcon.icns (every resolution). Outside a
/// bundle (`cargo run`) there's no .icns, so use the embedded 256 px PNG.
fn app_icon() -> eframe::egui::IconData {
    let in_bundle = std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"));
    if in_bundle {
        return eframe::egui::IconData::default();
    }
    image::load_from_memory(include_bytes!("../../docs/icon.png"))
        .map(|img| {
            let img = img.to_rgba8();
            let (width, height) = img.dimensions();
            eframe::egui::IconData { rgba: img.into_raw(), width, height }
        })
        .unwrap_or_default()
}

/// The window process.
pub fn run_window(tab: Tab, app: Option<String>) -> anyhow::Result<()> {
    let _ = paths::ensure_dirs();
    let _ = std::fs::write(paths::window_pid_file(), std::process::id().to_string());
    let settings = Settings::load();
    let shared = daemon::spawn(settings, Role::Window);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("PulseBar")
            // Screenshots (PULSEBAR_SCREENSHOT) use a taller window so whole tabs fit.
            .with_inner_size(if std::env::var_os("PULSEBAR_SCREENSHOT").is_some() { [1180.0, 1400.0] } else { [1180.0, 760.0] })
            .with_min_inner_size([820.0, 520.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    let r = eframe::run_native(
        "PulseBar",
        options,
        Box::new(move |cc| {
            // Screenshot runs stay Dock-less (existing behaviour).
            if std::env::var_os("PULSEBAR_SCREENSHOT").is_none() {
                become_regular_app();
            }
            show_app();
            Ok(Box::new(app::PulseApp::new(cc, shared, tab, app)))
        }),
    );
    let _ = std::fs::remove_file(paths::window_pid_file());
    r.map_err(|e| anyhow::anyhow!("{e}"))
}
