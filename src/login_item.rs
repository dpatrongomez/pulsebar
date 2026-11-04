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
    /// Registered, but the user has not approved it (or revoked consent) in System Settings →
    /// Login Items: it will NOT open at login until they do.
    RequiresApproval,
    /// Not running from an app bundle: can't be registered.
    Unbundled,
    /// This platform/OS version has no login item support.
    Unsupported,
}

impl LoginStatus {
    /// Whether the app will open at login: the switch position. `RequiresApproval` is off,
    /// because the system will not launch it until the user approves it again.
    pub fn is_on(&self) -> bool {
        matches!(self, LoginStatus::Enabled)
    }
    /// Stable machine-readable token for `--json` (never reworded).
    pub fn token(&self) -> &'static str {
        match self {
            LoginStatus::Enabled => "enabled",
            LoginStatus::Disabled => "disabled",
            LoginStatus::RequiresApproval => "requires_approval",
            LoginStatus::Unbundled => "unbundled",
            LoginStatus::Unsupported => "unsupported",
        }
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

/// Where to re-run when started through a symlink into the bundle (the Homebrew `pulsebar`
/// link). `Bundle.main` only resolves to the app when the real binary is the running process,
/// so `SMAppService.mainApp` needs a re-exec. `None` when already running from the bundle, when
/// the real binary is not in a bundle either, or when this already is the re-executed process.
pub fn reexec_target(exe: &Path, real: &Path, already_reexeced: bool) -> Option<std::path::PathBuf> {
    (!already_reexeced && real != exe && !exe_in_bundle(exe) && exe_in_bundle(real)).then(|| real.to_path_buf())
}

/// Re-run the real binary with the same arguments when invoked through a symlink into the
/// bundle. Replaces the process on success (never returns); returns `Ok(())` when not needed.
/// A marker env var guarantees a single hop.
#[cfg(unix)]
pub fn reexec_from_symlink() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    const MARK: &str = "PULSEBAR_LOGIN_REEXEC";
    let exe = std::env::current_exe()?;
    let real = std::fs::canonicalize(&exe)?;
    match reexec_target(&exe, &real, std::env::var_os(MARK).is_some()) {
        Some(real) => Err(std::process::Command::new(real).args(std::env::args_os().skip(1)).env(MARK, "1").exec()),
        None => Ok(()),
    }
}

#[cfg(not(unix))]
pub fn reexec_from_symlink() -> std::io::Result<()> {
    Ok(())
}

fn bundled() -> bool {
    std::env::current_exe().is_ok_and(|p| exe_in_bundle(&p))
}

/// Why registering/unregistering failed. The GUI shows `localized` (system language); the CLI
/// shows [`SetError::cli_message`] (always English).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetError {
    /// NSError domain; empty for errors that did not come from the system.
    pub domain: String,
    pub code: isize,
    pub localized: String,
}

/// `kSMErrorLaunchDeniedByUser` (ServiceManagement/SMErrors.h).
const LAUNCH_DENIED_BY_USER: isize = 10;

impl SetError {
    /// An error that is not from the system: the same (English) text everywhere.
    pub fn plain(msg: impl Into<String>) -> Self {
        Self { domain: String::new(), code: 0, localized: msg.into() }
    }
    /// English hint for the cases where the user can fix it.
    pub fn hint(&self) -> Option<&'static str> {
        (self.domain == "SMAppServiceErrorDomain" && self.code == LAUNCH_DENIED_BY_USER)
            .then_some("approve PulseBar in System Settings → General → Login Items")
    }
    /// English text with domain and code (the localized description is never used here).
    pub fn cli_message(&self) -> String {
        if self.domain.is_empty() {
            return self.localized.clone();
        }
        let base = format!("{} error {}", self.domain, self.code);
        match self.hint() {
            Some(h) => format!("{base} ({h})"),
            None => base,
        }
    }
}

#[cfg(target_os = "macos")]
mod sys_impl {
    use super::{LoginStatus, SetError};
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    // SMAppService needs macOS 13+; the app requires 15 (Info.plist, cask), and unbundled
    // processes never get here, so no runtime availability check is needed.
    pub fn status() -> LoginStatus {
        let s = unsafe { SMAppService::mainAppService().status() };
        match s {
            SMAppServiceStatus::Enabled => LoginStatus::Enabled,
            SMAppServiceStatus::RequiresApproval => LoginStatus::RequiresApproval,
            // NotRegistered, and NotFound: a bundle that was never registered reports NotFound
            // until the first register() (unbundled processes are handled before we get here).
            _ => LoginStatus::Disabled,
        }
    }

    pub fn set(on: bool) -> Result<(), SetError> {
        let svc = unsafe { SMAppService::mainAppService() };
        let r = unsafe { if on { svc.registerAndReturnError() } else { svc.unregisterAndReturnError() } };
        r.map_err(|e| SetError { domain: e.domain().to_string(), code: e.code(), localized: e.localizedDescription().to_string() })
    }

    pub fn open_settings() {
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    }
}

#[cfg(not(target_os = "macos"))]
mod sys_impl {
    use super::{LoginStatus, SetError};
    pub fn status() -> LoginStatus {
        LoginStatus::Unsupported
    }
    pub fn set(_on: bool) -> Result<(), SetError> {
        Err(SetError::plain("launch at login is only available on macOS"))
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

/// Whether register/unregister must be called to reach `on`. The system fails when there is
/// nothing to change (already registered / not registered), so those cases are no-ops.
pub fn needs_change(on: bool, current: &LoginStatus) -> bool {
    !matches!((on, current), (true, LoginStatus::Enabled) | (false, LoginStatus::Disabled))
}

/// Register or unregister the app, then return the status read back from the system.
/// Asking for the state it already has succeeds without touching the system. The one-time offer
/// is marked as answered only after success.
pub fn set_enabled(on: bool) -> Result<LoginStatus, SetError> {
    let st = status();
    if !st.can_change() {
        return Err(SetError::plain(st.describe()));
    }
    if needs_change(on, &st) {
        sys_impl::set(on)?;
    }
    mark_offered();
    Ok(status())
}

/// Open System Settings → General → Login Items.
pub fn open_system_settings() {
    sys_impl::open_settings();
}

fn offered_marker() -> std::path::PathBuf {
    paths::app_support().join(".login_offered")
}

/// Whether the first-launch banner has already been shown.
pub fn was_offered() -> bool {
    offered_marker().exists()
}

/// Remember that the first-launch offer has been made, so it is never shown again.
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
        assert!(!LoginStatus::RequiresApproval.is_on(), "revoked in System Settings: switch is off");
        assert!(!LoginStatus::Disabled.is_on());
        assert!(!LoginStatus::Unbundled.can_change());
        assert!(LoginStatus::RequiresApproval.can_change());
    }

    #[test]
    fn idempotent_decision() {
        use LoginStatus::*;
        assert!(!needs_change(true, &Enabled));
        assert!(!needs_change(false, &Disabled));
        assert!(needs_change(true, &Disabled));
        assert!(needs_change(false, &Enabled));
        // Not approved: turning on must reach the system (it reports the denial), off unregisters.
        assert!(needs_change(true, &RequiresApproval));
        assert!(needs_change(false, &RequiresApproval));
    }

    #[test]
    fn reexec_only_through_a_symlink_and_once() {
        let link = Path::new("/opt/homebrew/bin/pulsebar");
        let real = Path::new("/Applications/PulseBar.app/Contents/MacOS/PulseBar");
        assert_eq!(reexec_target(link, real, false), Some(real.to_path_buf()));
        assert_eq!(reexec_target(link, real, true), None, "marker set: no second hop");
        assert_eq!(reexec_target(real, real, false), None, "already the real binary");
        assert_eq!(reexec_target(real, Path::new("/x/PulseBar.app/Contents/MacOS/PulseBar"), false), None, "already in a bundle");
        let bare = Path::new("/Users/me/pulsebar/target/debug/pulsebar");
        assert_eq!(reexec_target(link, bare, false), None, "target is not a bundle");
    }

    #[test]
    fn set_error_messages_are_english() {
        let denied = SetError { domain: "SMAppServiceErrorDomain".into(), code: 10, localized: "No se pudo".into() };
        assert_eq!(denied.cli_message(), "SMAppServiceErrorDomain error 10 (approve PulseBar in System Settings → General → Login Items)");
        let other = SetError { domain: "SMAppServiceErrorDomain".into(), code: 1, localized: "No se pudo".into() };
        assert_eq!(other.cli_message(), "SMAppServiceErrorDomain error 1");
        assert_eq!(SetError::plain("nope").cli_message(), "nope");
    }

    #[test]
    fn status_tokens_are_stable() {
        assert_eq!(LoginStatus::RequiresApproval.token(), "requires_approval");
        assert_eq!(LoginStatus::Unbundled.token(), "unbundled");
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
