//! Native UI, split into two processes (see `daemon` for why):
//! * `run_agent`  — menu bar status items on a bare NSApplication run loop, or D-Bus status icons
//!   on Linux (no GPU window);
//! * `run_window` — the egui window, launched on demand and exiting when closed.

mod app;
mod dialog;
mod icons;
pub mod notify;
mod shortcuts;
mod loc;
pub mod tray;
#[cfg(target_os = "linux")]
mod tray_linux;
#[cfg(target_os = "macos")]
mod tray_mac;
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
    /// Also open the quit confirmation for `app` (never quits without that confirmation).
    #[serde(default)]
    pub quit: bool,
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
    sys::try_lock(&paths::window_lock_file()).is_none()
}

/// Ask the window, running or about to start, to show a tab.
fn write_nav(tab: Tab, app: Option<String>, quit: bool) {
    let nav = Nav { tab, app, quit, at_ms: sys::now_ms() };
    let _ = paths::ensure_dirs();
    let _ = std::fs::write(paths::nav_file(), serde_json::to_vec(&nav).unwrap_or_default());
}

/// Open (or focus) the window on a tab, optionally scrolled to an app.
pub fn open_window(tab: Tab, app: Option<String>) {
    open_window_with(tab, app, false);
}

/// Like `open_window`; with `quit` the window also shows the quit confirmation for `app`.
pub fn open_window_with(tab: Tab, app: Option<String>, quit: bool) {
    let quit = quit && app.is_some();
    write_nav(tab, app.clone(), quit);
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
        if quit {
            cmd.arg("--quit");
        }
        let _ = cmd.spawn();
    }
}

/// An MCP quit request needs a human: make sure the window is up.
fn surface_pending_request(last_prompted: &std::sync::Mutex<String>) {
    if let Some(first) = actions::list_pending().first() {
        let mut lp = last_prompted.lock().unwrap();
        if *lp != first.id {
            *lp = first.id.clone();
            open_window(Tab::Overview, None);
        }
    }
}

/// Close the window too, then end the agent.
fn quit_agent() -> ! {
    if let Ok(pid) = std::fs::read_to_string(paths::window_pid_file())
        && let Ok(pid) = pid.trim().parse::<i32>()
        && sys::pid_alive(pid)
    {
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    std::process::exit(0);
}

/// Menu bar agent: tray items + sampler (history, alerts, live.json). No dock icon.
pub fn run_agent() -> anyhow::Result<()> {
    let _ = paths::ensure_dirs();
    // Already running: starting it again is a way of asking for the window.
    let Some(_instance) = sys::try_lock(&paths::agent_lock_file()) else {
        if std::env::var_os("PULSEBAR_NO_WINDOW").is_none() {
            open_window(Tab::Overview, None);
        }
        return Ok(());
    };
    let settings = Settings::load();
    #[cfg(target_os = "linux")]
    {
        let shared: Arc<daemon::Shared> = daemon::spawn(settings, Role::Agent);
        let agent = tray_linux::Agent::new(shared.clone());
        // Without a status notifier host nothing shows the icons, so the window is the only way in.
        let no_panel = !agent.has_host();
        let agent = std::sync::Mutex::new(agent);
        let last_prompted = std::sync::Mutex::new(String::new());
        *shared.on_tick.lock().unwrap() = Some(Box::new(move || {
            agent.lock().unwrap().refresh();
            surface_pending_request(&last_prompted);
        }));
        if std::env::var_os("PULSEBAR_NO_WINDOW").is_none() && (no_panel || !settings_has_launched_before()) {
            open_window(Tab::Overview, None);
        }
        loop {
            std::thread::park();
        }
    }
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        let mtm = objc2::MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("must run on the main thread"))?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let shared: Arc<daemon::Shared> = daemon::spawn(settings, Role::Agent);
        let last_prompted = std::sync::Mutex::new(String::new());
        *shared.on_tick.lock().unwrap() = Some(Box::new(move || {
            tray_mac::schedule_refresh();
            surface_pending_request(&last_prompted);
        }));
        notify::install();
        tray_mac::install(shared.clone());
        tray_icon::menu::MenuEvent::set_event_handler(Some(|e: tray_icon::menu::MenuEvent| {
            let id = e.id.0.clone();
            if id == "pb:quit" {
                quit_agent();
            }
            if let Some((tab, app)) = tray_mac::target_for(&id) {
                open_window(tab, app);
            }
        }));
        if std::env::var_os("PULSEBAR_NO_WINDOW").is_none() && !settings_has_launched_before() {
            open_window(Tab::Overview, None);
        }
        app.run();
    }
    #[cfg(not(target_os = "linux"))]
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
pub fn run_window(tab: Tab, app: Option<String>, quit: bool) -> anyhow::Result<()> {
    let _ = paths::ensure_dirs();
    // Single instance: a launcher can start this directly, any number of times. A second start
    // hands over to the running window. Screenshot runs are independent of it.
    let instance = sys::try_lock(&paths::window_lock_file());
    let screenshot = std::env::var_os("PULSEBAR_SCREENSHOT").is_some();
    if instance.is_none() && !screenshot {
        write_nav(tab, app.clone(), quit && app.is_some());
        return Ok(());
    }
    if !screenshot {
        let _ = std::fs::write(paths::window_pid_file(), std::process::id().to_string());
    }
    let settings = Settings::load();
    let shared = daemon::spawn(settings, Role::Window);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("PulseBar")
            // Wayland and X11 match this against pulsebar.desktop for the window's icon.
            .with_app_id("pulsebar")
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
            Ok(Box::new(app::PulseApp::new(cc, shared, tab, app, quit)))
        }),
    );
    if !screenshot {
        let _ = std::fs::remove_file(paths::window_pid_file());
    }
    drop(instance);
    r.map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_without_quit_field_still_parses() {
        let n: Nav = serde_json::from_str(r#"{"tab":"memory","app":"bundle:com.x","at_ms":5}"#).unwrap();
        assert!(!n.quit);
        let n: Nav = serde_json::from_str(r#"{"tab":"memory","app":"bundle:com.x","quit":true,"at_ms":5}"#).unwrap();
        assert!(n.quit && n.tab == Tab::Memory);
    }
}
