//! The complete list of network endpoints PulseBar may ever contact. Nothing else in the
//! codebase opens a TCP connection (Docker is a local unix socket). Both calls are off in
//! evaluation mode and can be disabled entirely; no telemetry, no analytics, no account.

pub const UPDATE_FEED: &str = "https://pulsebar.app/appcast.xml";
pub const LICENSE_ENDPOINT: &str = "https://pulsebar.app/api/license/validate";

pub const ALLOWED_HOSTS: [&str; 1] = ["pulsebar.app"];

/// Check for updates. Implemented by shelling out to `curl` (no HTTP stack linked into the
/// binary), only when the user enabled "Check for updates".
pub fn check_for_updates(enabled: bool, current_version: &str) -> Option<String> {
    if !enabled {
        return None;
    }
    let out = crate::sys::run("/usr/bin/curl", &["-fsS", "--max-time", "10", UPDATE_FEED], std::time::Duration::from_secs(12))?;
    // Appcast: first <sparkle:shortVersionString>X</…>
    let v = out.split("shortVersionString>").nth(1)?.split('<').next()?.trim().to_string();
    (v != current_version).then_some(v)
}

/// Validate a licence key. Never called in evaluation mode (no key).
pub fn validate_license(key: Option<&str>) -> Option<bool> {
    let key = key?;
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Some(false);
    }
    let body = format!("{{\"key\":\"{key}\"}}");
    let out = crate::sys::run(
        "/usr/bin/curl",
        &["-fsS", "--max-time", "10", "-H", "Content-Type: application/json", "-d", &body, LICENSE_ENDPOINT],
        std::time::Duration::from_secs(12),
    )?;
    Some(out.contains("\"valid\":true"))
}
