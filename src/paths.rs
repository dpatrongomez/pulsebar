//! Filesystem locations. Everything lives under `~/Library/Application Support/PulseBar`
//! (override with `PULSEBAR_HOME` for tests).

use std::path::PathBuf;

pub const APP_NAME: &str = "PulseBar";

pub fn app_support() -> PathBuf {
    if let Ok(p) = std::env::var("PULSEBAR_HOME") {
        return PathBuf::from(p);
    }
    dirs::data_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join(APP_NAME)
}

pub fn ensure_dirs() -> std::io::Result<()> {
    std::fs::create_dir_all(app_support())?;
    std::fs::create_dir_all(pending_dir())?;
    Ok(())
}

pub fn settings_file() -> PathBuf {
    app_support().join("settings.json")
}

/// The single SQLite file for history + alert rules.
pub fn database_file() -> PathBuf {
    app_support().join("history.sqlite")
}

/// Latest snapshot written by the running app (read by CLI/MCP).
pub fn live_file() -> PathBuf {
    app_support().join("live.json")
}

/// Quit requests coming from MCP that wait for a human "yes" in the UI.
pub fn pending_dir() -> PathBuf {
    app_support().join("pending")
}

pub fn exports_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(app_support)
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Alerts raised by the menu bar agent, shown (and dismissed) by the window.
pub fn alerts_file() -> PathBuf {
    app_support().join("alerts.json")
}

/// "Silence for 24 h" requests from the window → agent.
pub fn snoozes_file() -> PathBuf {
    app_support().join("snoozes.json")
}

/// PID of the running window process, for the agent to close it.
pub fn window_pid_file() -> PathBuf {
    app_support().join("window.pid")
}

/// Held by the running window (single instance).
pub fn window_lock_file() -> PathBuf {
    app_support().join(".window.lock")
}

/// Held by the running menu bar agent (single instance).
pub fn agent_lock_file() -> PathBuf {
    app_support().join(".agent.lock")
}

/// Navigation request from the menu bar ("open the Memory tab at Chrome").
pub fn nav_file() -> PathBuf {
    app_support().join("nav.json")
}
