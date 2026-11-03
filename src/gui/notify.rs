//! Native notifications (`UNUserNotificationCenter`) for alerts, with actions:
//! **Show** (opens the window on the app, via `nav.json`), **Quit app…** (opens the window's
//! confirmation dialog, never quits directly) and **Snooze 1 h** (writes `snoozes.json`).
//!
//! * Authorisation is requested the first time an alert fires, not at launch.
//! * Notifications are grouped per app (`threadIdentifier`).
//! * Falls back to the osascript path (`alerts::notify`) when running unbundled (no bundle
//!   identifier: `UNUserNotificationCenter` throws without one) or when the user denied
//!   notifications.
//!
//! Only the menu bar agent posts notifications and receives the responses; the window is a
//! separate process of the same bundle and is driven through `gui::open_window_with`.

use crate::alerts::Alert;
use crate::model::AlertKind;
use crate::settings::Tab;

const CAT_APP: &str = "pulsebar.alert.app";
const CAT_MACHINE: &str = "pulsebar.alert.machine";
const ACT_SHOW: &str = "pulsebar.show";
const ACT_QUIT: &str = "pulsebar.quit";
const ACT_SNOOZE: &str = "pulsebar.snooze";
const SNOOZE_S: i64 = 3600;
const INFO_KEY: &str = "alert";

/// The tab that explains an alert.
fn tab_for(kind: AlertKind) -> Tab {
    match kind {
        AlertKind::CpuSustained => Tab::Cpu,
        AlertKind::AppMemoryGrowth => Tab::Memory,
        AlertKind::DiskWriteSustained => Tab::Disk,
        AlertKind::NetworkSustained => Tab::Network,
    }
}

/// What a tap on the banner or one of its action buttons does. `action` is the response's
/// action identifier (a plain click arrives as `ACT_SHOW`).
fn perform(action: &str, a: &Alert) {
    let app = (!a.app_key.is_empty()).then(|| a.app_key.clone());
    match action {
        ACT_SNOOZE => crate::daemon::snooze_alert(a, crate::sys::now_s() + SNOOZE_S),
        ACT_QUIT => super::open_window_with(tab_for(a.kind), app, true),
        ACT_SHOW => super::open_window_with(tab_for(a.kind), app, false),
        _ => {}
    }
}

#[cfg(target_os = "macos")]
pub use mac::{deliver, install};

/// Non-macOS builds have no native path.
#[cfg(not(target_os = "macos"))]
pub fn install() {}
#[cfg(not(target_os = "macos"))]
pub fn deliver(_: &Alert) -> bool {
    false
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, ProtocolObject};
    use objc2::{AnyThread, define_class, msg_send};
    use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSSet, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification, UNNotificationAction,
        UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions, UNNotificationDefaultActionIdentifier,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse, UNNotificationSettings, UNNotificationSound,
        UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };

    use super::*;
    use crate::i18n::I18n;
    use crate::settings::Settings;

    /// `UNUserNotificationCenter` raises an exception without a bundle identifier (plain
    /// `cargo run` binaries), so every use is gated on this.
    fn bundled() -> bool {
        NSBundle::mainBundle().bundleIdentifier().is_some()
    }

    fn ns(s: &str) -> Retained<NSString> {
        NSString::from_str(s)
    }

    define_class!(
        /// Receives notification responses (button taps) and lets banners show while the app is
        /// frontmost. Callbacks arrive on a background queue.
        #[unsafe(super(NSObject))]
        #[name = "PulseBarNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                completion.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(&self, _center: &UNUserNotificationCenter, response: &UNNotificationResponse, completion: &block2::DynBlock<dyn Fn()>) {
                let id = response.actionIdentifier();
                // SAFETY: immutable framework constant.
                let action = if *id == *unsafe { UNNotificationDefaultActionIdentifier } { ACT_SHOW.to_string() } else { id.to_string() };
                if let Some(alert) = alert_of(&response.notification()) {
                    super::perform(&action, &alert);
                }
                completion.call(());
            }
        }
    );

    impl Delegate {
        fn new() -> Retained<Self> {
            unsafe { msg_send![Self::alloc(), init] }
        }
    }

    /// The alert a delivered notification was created from (carried in `userInfo`).
    fn alert_of(n: &UNNotification) -> Option<Alert> {
        let info = n.request().content().userInfo();
        let key = ns(INFO_KEY);
        let v = info.objectForKey(&key)?;
        let s = v.downcast_ref::<NSString>()?;
        serde_json::from_str(&s.to_string()).ok()
    }

    /// Menu bar agent start-up (main thread): attach the delegate so taps on notifications that
    /// are already in Notification Center still work after a restart. Does not ask for
    /// permission.
    pub fn install() {
        if !bundled() {
            return;
        }
        let delegate = Delegate::new();
        UNUserNotificationCenter::currentNotificationCenter().setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The centre keeps only a weak reference.
        std::mem::forget(delegate);
        register_categories();
    }

    fn category(id: &str, actions: &[Retained<UNNotificationAction>]) -> Retained<UNNotificationCategory> {
        let acts = NSArray::from_retained_slice(actions);
        UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &ns(id),
            &acts,
            &NSArray::new(),
            UNNotificationCategoryOptions::empty(),
        )
    }

    /// Titles follow the language in Settings, so this runs before each post as well.
    fn register_categories() {
        let i18n = I18n::new(Settings::load().language.as_deref());
        let act = |id: &str, key: &str, opts: UNNotificationActionOptions| {
            UNNotificationAction::actionWithIdentifier_title_options(&ns(id), &ns(&i18n.t(key)), opts)
        };
        let show = act(ACT_SHOW, "notif.show", UNNotificationActionOptions::Foreground);
        let quit = act(ACT_QUIT, "notif.quit", UNNotificationActionOptions::Foreground);
        let snooze = act(ACT_SNOOZE, "notif.snooze", UNNotificationActionOptions::empty());
        let cats = [category(CAT_APP, &[show.clone(), quit, snooze.clone()]), category(CAT_MACHINE, &[show, snooze])];
        UNUserNotificationCenter::currentNotificationCenter().setNotificationCategories(&NSSet::from_retained_slice(&cats));
    }

    fn post(a: &Alert) {
        register_categories();
        let content = UNMutableNotificationContent::new();
        let title = if a.app_name.is_empty() { "Alert" } else { &a.app_name };
        content.setTitle(&ns(title));
        content.setBody(&ns(&a.message));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let machine = a.app_key.is_empty();
        content.setCategoryIdentifier(&ns(if machine { CAT_MACHINE } else { CAT_APP }));
        content.setThreadIdentifier(&ns(&if machine { "pulsebar.machine".to_string() } else { format!("pulsebar.app.{}", a.app_key) }));
        let json = serde_json::to_string(a).unwrap_or_default();
        let info: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_retained_objects(&[&*ns(INFO_KEY)], &[ns(&json).into()]);
        // SAFETY: keys are NSStrings and values NSStrings, which `alert_of` expects.
        unsafe { content.setUserInfo(&*(Retained::as_ptr(&info) as *const NSDictionary)) };
        let id = format!("pulsebar.{:?}.{}.{}", a.kind, a.app_key, a.at);
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&ns(&id), &content, None);
        let (title, body) = (title.to_string(), a.message.clone());
        let done = RcBlock::new(move |err: *mut NSError| {
            if !err.is_null() {
                // Could not be queued: show it the old way rather than drop it.
                crate::alerts::notify(&title, &body);
            }
        });
        UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, Some(&done));
    }

    /// Post a native notification. Returns `false` (nothing posted, caller falls back to
    /// osascript) when unbundled. When authorisation is still undetermined it is requested now,
    /// and a denial falls back to osascript from the completion handler.
    pub fn deliver(a: &Alert) -> bool {
        if !bundled() {
            return false;
        }
        let alert = a.clone();
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let got_settings = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: the centre passes a live settings object for the duration of the call.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            if status == UNAuthorizationStatus::NotDetermined {
                let alert = alert.clone();
                let granted = RcBlock::new(move |ok: Bool, _err: *mut NSError| {
                    if ok.as_bool() {
                        post(&alert);
                    } else {
                        fallback(&alert);
                    }
                });
                UNUserNotificationCenter::currentNotificationCenter()
                    .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &granted);
            } else if status == UNAuthorizationStatus::Denied {
                fallback(&alert);
            } else {
                post(&alert);
            }
        });
        center.getNotificationSettingsWithCompletionHandler(&got_settings);
        true
    }

    fn fallback(a: &Alert) {
        crate::alerts::notify(if a.app_name.is_empty() { "Alert" } else { &a.app_name }, &a.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alerts_open_the_matching_tab() {
        assert_eq!(tab_for(AlertKind::AppMemoryGrowth), Tab::Memory);
        assert_eq!(tab_for(AlertKind::CpuSustained), Tab::Cpu);
    }
}
