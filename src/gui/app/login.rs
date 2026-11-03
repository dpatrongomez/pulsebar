//! "Open at login": the Settings switch and the one-time first-launch banner. State always comes
//! from `login_item::status()` (re-read every couple of seconds while visible, so a change made in
//! System Settings shows up), never from a stored flag.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Vec2};

use super::PulseApp;
use crate::login_item::{self, LoginStatus};

pub(super) struct LoginUi {
    status: LoginStatus,
    checked: Instant,
    /// Last registration error, shown under the switch.
    error: Option<String>,
    /// First-launch offer still pending (shown once, then never again).
    banner: bool,
}

impl LoginUi {
    pub(super) fn new() -> Self {
        let status = login_item::status();
        let banner = login_item::should_offer(&status, login_item::was_offered());
        Self { status, checked: Instant::now(), error: None, banner }
    }
}

impl PulseApp {
    fn login_refresh(&mut self) {
        if self.login.checked.elapsed() > Duration::from_secs(2) {
            self.login.status = login_item::status();
            self.login.checked = Instant::now();
        }
    }

    fn login_set(&mut self, on: bool) {
        match login_item::set_enabled(on) {
            Ok(st) => {
                self.login.status = st;
                self.login.error = None;
            }
            Err(e) => {
                self.login.status = login_item::status();
                self.login.error = Some(format!("{}: {e}", self.i18n.t("login.error")));
            }
        }
        self.login.checked = Instant::now();
    }

    /// One-time offer at the top of the window (not a modal). Either answer dismisses it for good.
    pub(super) fn login_banner(&mut self, ui: &mut egui::Ui) {
        if !self.login.banner {
            return;
        }
        self.login_refresh();
        if self.login.status != LoginStatus::Disabled {
            // Already enabled (e.g. from System Settings) or not possible: nothing to offer.
            self.login.banner = false;
            return;
        }
        let mut answer: Option<bool> = None;
        super::widgets::card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(self.i18n.t("login.offer")).strong());
                if ui.add(egui::Button::new(self.i18n.t("login.toggle")).corner_radius(8.0).min_size(Vec2::new(0.0, 26.0))).clicked() {
                    answer = Some(true);
                }
                if ui.add(egui::Button::new(self.i18n.t("login.not_now")).corner_radius(8.0).min_size(Vec2::new(0.0, 26.0))).clicked() {
                    answer = Some(false);
                }
            });
        });
        ui.add_space(10.0);
        if let Some(yes) = answer {
            login_item::mark_offered();
            self.login.banner = false;
            if yes {
                self.login_set(true);
            }
        }
    }

    /// Settings card with the "Open at login" switch.
    pub(super) fn login_section(&mut self, ui: &mut egui::Ui) {
        self.login_refresh();
        ui.ctx().request_repaint_after(Duration::from_secs(2));
        let status = self.login.status.clone();
        let mut on = status.is_on();
        let mut toggled = false;
        let mut open_settings = false;
        Self::section(ui, &self.i18n.t("login.section"), "", |ui| {
            ui.horizontal_wrapped(|ui| {
                toggled = ui.add_enabled(status.can_change(), egui::Checkbox::new(&mut on, self.i18n.t("login.toggle"))).changed();
                match status {
                    LoginStatus::RequiresApproval => {
                        ui.label(RichText::new(self.i18n.t("login.requires_approval")).color(ui.visuals().warn_fg_color));
                        open_settings = ui.small_button(self.i18n.t("login.open_settings")).clicked();
                    }
                    LoginStatus::Unbundled => {
                        ui.label(RichText::new(self.i18n.t("login.unbundled")).small().weak());
                    }
                    LoginStatus::Unsupported => {
                        ui.label(RichText::new(self.i18n.t("login.unsupported")).small().weak());
                    }
                    _ => {}
                }
            });
            if let Some(e) = &self.login.error {
                ui.label(RichText::new(e).small().color(ui.visuals().error_fg_color));
            }
        });
        if toggled {
            self.login_set(on);
        }
        if open_settings {
            login_item::open_system_settings();
        }
    }
}
