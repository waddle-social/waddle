//! Own presence on screen: input events and a timer feed the controller in
//! `crate::presence`, which says what to announce, store or read. This
//! module only performs those effects and shows the result in the account
//! row.

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;

use super::WaddleWindow;
use crate::composer::Availability;
use crate::presence::{Effect, ManualStatus, PresenceMode, Show};

/// How often idle time is looked at. Input brings the device back sooner.
const TICK_SECONDS: u32 = 30;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl WaddleWindow {
    /// Window input counts as the user being here (auto-away).
    pub(super) fn setup_presence_input(&self) {
        let seen = |window: &glib::WeakRef<WaddleWindow>| {
            let window = window.clone();
            move || {
                if let Some(window) = window.upgrade() {
                    window.note_input();
                }
            }
        };
        let weak = self.downgrade();

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let on_key = seen(&weak);
        keys.connect_key_pressed(move |_, _, _, _| {
            on_key();
            glib::Propagation::Proceed
        });
        self.add_controller(keys);

        let motion = gtk::EventControllerMotion::new();
        motion.set_propagation_phase(gtk::PropagationPhase::Capture);
        let on_motion = seen(&weak);
        motion.connect_motion(move |_, _, _| on_motion());
        self.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let on_scroll = seen(&weak);
        scroll.connect_scroll(move |_, _, _| {
            on_scroll();
            glib::Propagation::Proceed
        });
        self.add_controller(scroll);

        // A click does not claim its sequence: it only notes that the user
        // is here.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let on_click = seen(&weak);
        click.connect_pressed(move |_, _, _, _| on_click());
        self.add_controller(click);

        let window = self.downgrade();
        glib::timeout_add_seconds_local(TICK_SECONDS, move || {
            let Some(window) = window.upgrade() else {
                return glib::ControlFlow::Break;
            };
            window.refresh_presence();
            glib::ControlFlow::Continue
        });
    }

    /// The user did something in this window.
    fn note_input(&self) {
        let state = &self.imp().profile;
        let now = now_ms();
        let was_idle = {
            let mut presence = state.presence.borrow_mut();
            presence.note_input(now);
            presence.idle_by_itself()
        };
        // Coming back from Away should show at once, not at the next tick.
        if was_idle {
            self.refresh_presence();
        }
    }

    pub(super) fn refresh_presence(&self) {
        let effects = self.imp().profile.presence.borrow_mut().refresh(now_ms());
        self.perform(effects);
    }

    /// The connection came up: announce, then store or read the account's
    /// stored status.
    pub(super) fn presence_connected(&self) {
        let effects = self.imp().profile.presence.borrow_mut().connected(now_ms());
        self.perform(effects);
    }

    pub(super) fn presence_disconnected(&self) {
        self.imp().profile.presence.borrow_mut().disconnected();
    }

    /// Pick a status for the account, or Automatic.
    pub(super) fn pick_presence(&self, mode: PresenceMode) {
        let effects = self
            .imp()
            .profile
            .presence
            .borrow_mut()
            .set_mode(mode, now_ms());
        self.perform(effects);
        self.refresh_account_row();
    }

    /// The RFC 6121 status message; blank removes it.
    pub(super) fn set_status_message(&self, text: &str) {
        let effects = self
            .imp()
            .profile
            .presence
            .borrow_mut()
            .set_status_text(text, now_ms());
        self.perform(effects);
    }

    /// Another device of the account picked a status.
    pub(super) fn on_status_heard(&self, mode: PresenceMode) {
        let effects = self
            .imp()
            .profile
            .presence
            .borrow_mut()
            .heard(mode, now_ms());
        self.perform(effects);
        self.refresh_account_row();
    }

    /// `/away`, `/active` and `/dnd` pick a status by hand, as on Apple.
    pub(super) fn set_availability(&self, availability: Availability) {
        let status = match availability {
            Availability::Available => ManualStatus::Available,
            Availability::Away => ManualStatus::Away,
            Availability::DoNotDisturb => ManualStatus::DoNotDisturb,
        };
        self.pick_presence(PresenceMode::Manual(status));
        self.toast(&format!("Your status is now {}.", availability.title()));
    }

    fn perform(&self, effects: Vec<Effect>) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get();
        for effect in effects {
            match effect {
                Effect::Announce(presence) => session.announce(&presence),
                Effect::Publish(mode) => {
                    let (window, session) = (self.clone(), session.clone());
                    glib::spawn_future_local(async move {
                        let accepted = session.store_mode(mode).await;
                        if window.imp().generation.get() != generation {
                            return;
                        }
                        let effects = window.imp().profile.presence.borrow_mut().published(
                            mode,
                            accepted,
                            now_ms(),
                        );
                        window.perform(effects);
                        window.refresh_account_row();
                    });
                }
                Effect::Fetch(token) => {
                    let (window, session) = (self.clone(), session.clone());
                    glib::spawn_future_local(async move {
                        let stored = session.stored_mode().await;
                        if window.imp().generation.get() != generation {
                            return;
                        }
                        let effects = window.imp().profile.presence.borrow_mut().fetched(
                            token,
                            stored,
                            now_ms(),
                        );
                        window.perform(effects);
                        window.refresh_account_row();
                    });
                }
            }
        }
        self.refresh_account_row();
    }

    /// The account row: the picture, the name, and how the account shows
    /// when it is not simply online.
    pub(super) fn refresh_account_row(&self) {
        let imp = self.imp();
        let Some(jid) = imp
            .account
            .borrow()
            .as_ref()
            .map(|account| account.session.jid.to_string())
        else {
            return;
        };
        let shown = imp.profile.presence.borrow().shown();
        imp.account_label.set_label(&if shown == Show::Available {
            jid.clone()
        } else {
            format!("{} · {jid}", shown.title())
        });
        imp.account_avatar
            .set_custom_image(self.avatar_texture(&jid).as_ref());
    }
}
