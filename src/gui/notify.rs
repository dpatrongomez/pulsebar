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

/// What a tap on the banner or one of its buttons should do, decided without side effects.
#[derive(Debug, PartialEq)]
enum Intent {
    Snooze,
    /// Open the window on `tab`; `quit` only opens the confirmation dialog, it never quits.
    Open { tab: Tab, app: Option<String>, quit: bool },
    Nothing,
}

/// `action` is the response's action identifier (a plain click arrives as `ACT_SHOW`).
fn intent(action: &str, a: &Alert) -> Intent {
    let open = |quit| Intent::Open { tab: tab_for(a.kind), app: (!a.app_key.is_empty()).then(|| a.app_key.clone()), quit };
    match action {
        ACT_SNOOZE => Intent::Snooze,
        ACT_QUIT => open(true),
        ACT_SHOW => open(false),
        _ => Intent::Nothing,
    }
}

/// Carry out what [`intent`] decided.
fn perform(action: &str, a: &Alert) {
    match intent(action, a) {
        Intent::Snooze => crate::daemon::snooze_alert(a, crate::sys::now_s() + SNOOZE_S),
        Intent::Open { tab, app, quit } => super::open_window_with(tab, app, quit),
        Intent::Nothing => {}
    }
}

#[cfg(target_os = "macos")]
pub use mac::{deliver, install};

/// Non-macOS builds have no native path.
#[cfg(not(target_os = "macos"))]
pub fn install() {}
#[cfg(not(target_os = "macos"))]
pub fn deliver(_: &Alert, _: &crate::i18n::I18n) -> bool {
    false
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};

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

    /// A single authorisation request in flight: while the user hasn't answered the prompt,
    /// further alerts go through osascript instead of queueing their own request.
    static AUTH_PENDING: AtomicBool = AtomicBool::new(false);

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
        register_categories(&I18n::new(Settings::load().language.as_deref()));
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

    /// Action titles are fixed when a category is registered, so this runs before each post as
    /// well (with the language the daemon currently uses) and a language change applies to the
    /// next notification.
    fn register_categories(i18n: &I18n) {
        let act = |id: &str, key: &str, opts: UNNotificationActionOptions| {
            UNNotificationAction::actionWithIdentifier_title_options(&ns(id), &ns(&i18n.t(key)), opts)
        };
        let show = act(ACT_SHOW, "notif.show", UNNotificationActionOptions::Foreground);
        let quit = act(ACT_QUIT, "notif.quit", UNNotificationActionOptions::Foreground);
        let snooze = act(ACT_SNOOZE, "notif.snooze", UNNotificationActionOptions::empty());
        let cats = [category(CAT_APP, &[show.clone(), quit, snooze.clone()]), category(CAT_MACHINE, &[show, snooze])];
        UNUserNotificationCenter::currentNotificationCenter().setNotificationCategories(&NSSet::from_retained_slice(&cats));
    }

    fn post(a: &Alert, i18n: &I18n) {
        register_categories(i18n);
        let content = UNMutableNotificationContent::new();
        let (title, body) = (a.title(i18n), a.text(i18n));
        content.setTitle(&ns(&title));
        content.setBody(&ns(&body));
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
    pub fn deliver(a: &Alert, i18n: &I18n) -> bool {
        if !bundled() {
            return false;
        }
        let alert = a.clone();
        let i18n = i18n.clone();
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let got_settings = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: the centre passes a live settings object for the duration of the call.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            if status == UNAuthorizationStatus::NotDetermined {
                if AUTH_PENDING.swap(true, Ordering::SeqCst) {
                    fallback(&alert, &i18n);
                    return;
                }
                let alert = alert.clone();
                let i18n = i18n.clone();
                let granted = RcBlock::new(move |ok: Bool, _err: *mut NSError| {
                    // Every completion (granted, denied or error) lands here first.
                    AUTH_PENDING.store(false, Ordering::SeqCst);
                    if ok.as_bool() {
                        post(&alert, &i18n);
                    } else {
                        fallback(&alert, &i18n);
                    }
                });
                UNUserNotificationCenter::currentNotificationCenter()
                    .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &granted);
            } else if status == UNAuthorizationStatus::Denied {
                fallback(&alert, &i18n);
            } else {
                post(&alert, &i18n);
            }
        });
        center.getNotificationSettingsWithCompletionHandler(&got_settings);
        true
    }

    fn fallback(a: &Alert, i18n: &I18n) {
        crate::alerts::notify(&a.title(i18n), &a.text(i18n));
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

    fn alert(app_key: &str) -> Alert {
        Alert { kind: AlertKind::AppMemoryGrowth, app_key: app_key.into(), app_name: String::new(), message: "m".into(), at: 1, key: String::new(), args: vec![] }
    }

    #[test]
    fn quit_action_only_opens_the_dialog() {
        let open = Intent::Open { tab: Tab::Memory, app: Some("bundle:x".into()), quit: true };
        assert_eq!(intent(ACT_QUIT, &alert("bundle:x")), open);
    }

    #[test]
    fn normal_click_equals_show() {
        let a = alert("bundle:x");
        assert_eq!(intent(ACT_SHOW, &a), Intent::Open { tab: Tab::Memory, app: Some("bundle:x".into()), quit: false });
        // The delegate maps the default action to ACT_SHOW before calling `perform`.
        assert_ne!(intent(ACT_SHOW, &a), intent(ACT_QUIT, &a));
    }

    #[test]
    fn machine_alerts_have_no_app() {
        assert_eq!(intent(ACT_SHOW, &alert("")), Intent::Open { tab: Tab::Memory, app: None, quit: false });
    }

    #[test]
    fn snooze_and_unknown_actions() {
        assert_eq!(intent(ACT_SNOOZE, &alert("bundle:x")), Intent::Snooze);
        assert_eq!(intent("com.apple.UNNotificationDismissActionIdentifier", &alert("bundle:x")), Intent::Nothing);
        assert_eq!(intent("", &alert("bundle:x")), Intent::Nothing);
    }
}
