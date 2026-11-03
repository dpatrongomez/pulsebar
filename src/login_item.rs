//! "Open at login": `SMAppService.mainApp` (macOS 13+). The app then shows up in System Settings →
//! General → Login Items as PulseBar. The status is always read back from the system (the user
//! can switch it off there), never from a stored flag. Only a bundled `PulseBar.app` can register;
//! a bare binary (`cargo run`) reports [`LoginStatus::Unbundled`] instead.

use std::path::Path;

use crate::paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginStatus {
    /// Registered and allowed to launch at login.
    Enabled,
    /// Not registered (the default).
    Disabled,
    /// Registered, but the user has to approve it in System Settings → Login Items.
    RequiresApproval,
    /// Not running from an app bundle: can't be registered.
    Unbundled,
    /// This platform/OS version has no login item support.
    Unsupported,
}

impl LoginStatus {
    /// Whether the app will (try to) open at login: the switch position.
    pub fn is_on(&self) -> bool {
        matches!(self, LoginStatus::Enabled | LoginStatus::RequiresApproval)
    }
    /// Whether the user can change the setting from here.
    pub fn can_change(&self) -> bool {
        !matches!(self, LoginStatus::Unbundled | LoginStatus::Unsupported)
    }
    /// English description for the CLI.
    pub fn describe(&self) -> &'static str {
        match self {
            LoginStatus::Enabled => "on",
            LoginStatus::Disabled => "off",
            LoginStatus::RequiresApproval => "requires approval (System Settings → General → Login Items)",
            LoginStatus::Unbundled => "unavailable (not running from PulseBar.app)",
            LoginStatus::Unsupported => "unavailable (requires macOS 13 or later)",
        }
    }
}

/// True when `exe` lives inside an `.app` bundle (`Foo.app/Contents/MacOS/foo`).
pub fn exe_in_bundle(exe: &Path) -> bool {
    exe.to_string_lossy().contains(".app/Contents/MacOS/")
}

fn bundled() -> bool {
    std::env::current_exe().is_ok_and(|p| exe_in_bundle(&p))
}

#[cfg(target_os = "macos")]
mod sys_impl {
    use super::LoginStatus;
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    // SMAppService is macOS 13+. The class is looked up at runtime so older systems degrade.
    fn available() -> bool {
        objc2::runtime::AnyClass::get(c"SMAppService").is_some()
    }

    pub fn status() -> LoginStatus {
        if !available() {
            return LoginStatus::Unsupported;
        }
        let s = unsafe { SMAppService::mainAppService().status() };
        match s {
            SMAppServiceStatus::Enabled => LoginStatus::Enabled,
            SMAppServiceStatus::RequiresApproval => LoginStatus::RequiresApproval,
            // NotRegistered, and NotFound: a bundle that was never registered reports NotFound
            // until the first register() (unbundled processes are handled before we get here).
            _ => LoginStatus::Disabled,
        }
    }

    pub fn set(on: bool) -> Result<(), String> {
        let svc = unsafe { SMAppService::mainAppService() };
        let r = unsafe { if on { svc.registerAndReturnError() } else { svc.unregisterAndReturnError() } };
        r.map_err(|e| e.localizedDescription().to_string())
    }

    pub fn open_settings() {
        if available() {
            unsafe { SMAppService::openSystemSettingsLoginItems() };
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod sys_impl {
    use super::LoginStatus;
    pub fn status() -> LoginStatus {
        LoginStatus::Unsupported
    }
    pub fn set(_on: bool) -> Result<(), String> {
        Err("launch at login is only available on macOS".into())
    }
    pub fn open_settings() {}
}

/// Current status, read from the system.
pub fn status() -> LoginStatus {
    if !bundled() {
        return if cfg!(target_os = "macos") { LoginStatus::Unbundled } else { LoginStatus::Unsupported };
    }
    sys_impl::status()
}

/// Register or unregister the app, then return the status read back from the system.
/// The one-time offer is considered answered either way.
pub fn set_enabled(on: bool) -> Result<LoginStatus, String> {
    let st = status();
    if !st.can_change() {
        return Err(st.describe().to_string());
    }
    mark_offered();
    sys_impl::set(on)?;
    Ok(status())
}

/// Open System Settings → General → Login Items.
pub fn open_system_settings() {
    sys_impl::open_settings();
}

fn offered_marker() -> std::path::PathBuf {
    paths::app_support().join(".login_offered")
}

/// Whether the first-launch banner has already been shown and answered.
pub fn was_offered() -> bool {
    offered_marker().exists()
}

/// Remember that the first-launch offer has been answered, so it is never shown again.
pub fn mark_offered() {
    let _ = paths::ensure_dirs();
    let _ = std::fs::write(offered_marker(), b"1");
}

/// The first-launch banner is shown once, only while it can actually do something.
pub fn should_offer(status: &LoginStatus, offered: bool) -> bool {
    !offered && *status == LoginStatus::Disabled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_detection() {
        assert!(exe_in_bundle(Path::new("/Applications/PulseBar.app/Contents/MacOS/PulseBar")));
        assert!(!exe_in_bundle(Path::new("/Users/me/pulsebar/target/debug/pulsebar")));
    }

    #[test]
    fn switch_semantics() {
        assert!(LoginStatus::Enabled.is_on());
        assert!(LoginStatus::RequiresApproval.is_on());
        assert!(!LoginStatus::Disabled.is_on());
        assert!(!LoginStatus::Unbundled.can_change());
        assert!(LoginStatus::RequiresApproval.can_change());
    }

    #[test]
    fn offer_once() {
        assert!(should_offer(&LoginStatus::Disabled, false));
        assert!(!should_offer(&LoginStatus::Disabled, true));
        assert!(!should_offer(&LoginStatus::Unbundled, false));
        assert!(!should_offer(&LoginStatus::Enabled, false));
    }

    #[test]
    fn unbundled_test_binary_degrades() {
        // `cargo test` runs outside a bundle: no panic, an explanatory error.
        assert!(matches!(status(), LoginStatus::Unbundled | LoginStatus::Unsupported));
        assert!(set_enabled(true).is_err());
    }
}
