//! The window's side of staying connected: the reconnect controller's
//! timers, the offline banner, and the focus and network triggers.
//!
//! `LinkState` is also where the delivery, typing and read-marker state of
//! the window lives (`delivery.rs`, `typing.rs` and `read_markers.rs` in
//! this directory); it is one field of the window so those features do not
//! each grow it.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};

use super::WaddleWindow;
use crate::delivery::Outbox;
use crate::read_sync::ReadMarks;
use crate::reconnect::{Action, Controller};
use crate::typing;

#[derive(Default)]
pub struct LinkState {
    pub controller: RefCell<Controller>,
    pub banner: RefCell<Option<adw::Banner>>,
    /// Unsent messages of the signed-in account (see `delivery.rs`).
    pub outbox: RefCell<Outbox>,
    pub flushing: Cell<bool>,
    /// Origin ids of messages the user discarded; their rows stay in the
    /// timeline (rows are addressed by position) but are not shown.
    pub discarded: RefCell<HashSet<String>>,
    /// Origin ids the server refused outright.
    pub rejected: RefCell<HashSet<String>>,
    pub typing_sender: RefCell<typing::Sender>,
    pub typing_book: RefCell<typing::Book>,
    pub typing_label: RefCell<Option<gtk::Label>>,
    pub typing_ticking: Cell<bool>,
    pub marks: RefCell<ReadMarks>,
}

impl WaddleWindow {
    /// Once per window: the banner above the content, and the focus and
    /// network triggers.
    pub(super) fn setup_link(&self) {
        let imp = self.imp();
        let banner = adw::Banner::builder()
            .button_label("Reconnect Now")
            .revealed(false)
            .build();
        let window = self.downgrade();
        banner.connect_button_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.link_resume();
            }
        });
        // The banner sits above everything the toast overlay holds.
        if let Some(content) = imp.toast_overlay.child() {
            imp.toast_overlay.set_child(None::<&gtk::Widget>);
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            column.append(&banner);
            content.set_vexpand(true);
            column.append(&content);
            imp.toast_overlay.set_child(Some(&column));
        }
        imp.link.banner.replace(Some(banner));

        // Back in front of the user: do not leave them waiting out a backoff.
        self.connect_is_active_notify(|window| {
            if window.is_active() {
                window.link_resume();
            }
        });
        let monitor = gio::NetworkMonitor::default();
        let window = self.downgrade();
        monitor.connect_network_available_notify(move |monitor| {
            if let Some(window) = window.upgrade() {
                window.link_network(monitor.is_network_available());
            }
        });
        self.setup_typing();
    }

    /// A session was just started (and has already begun connecting).
    pub(super) fn link_begin(&self, generation: u64) {
        let imp = self.imp();
        let monitor = gio::NetworkMonitor::default();
        {
            let mut controller = imp.link.controller.borrow_mut();
            *controller = Controller::default();
            if !monitor.is_network_available() {
                controller.network_changed(false);
            }
        }
        let actions = imp.link.controller.borrow_mut().start();
        // `Session::start` already made the first attempt.
        self.link_perform(
            actions
                .into_iter()
                .filter(|action| *action != Action::Connect)
                .collect(),
            generation,
        );
        self.link_load_outbox();
        self.refresh_banner();
    }

    pub(super) fn link_stop(&self) {
        let imp = self.imp();
        imp.link.controller.borrow_mut().stop();
        imp.link.typing_sender.borrow_mut().reset();
        imp.link.typing_book.borrow_mut().clear();
        imp.link.marks.borrow_mut().clear();
        imp.link.discarded.borrow_mut().clear();
        imp.link.rejected.borrow_mut().clear();
        imp.link.outbox.replace(Default::default());
        imp.link.flushing.set(false);
        self.refresh_typing_line();
        self.refresh_banner();
    }

    pub(super) fn link_connected(&self) {
        self.imp().link.controller.borrow_mut().connected();
        self.refresh_banner();
    }

    pub(super) fn link_disconnected(&self, generation: u64) {
        let imp = self.imp();
        imp.link.outbox.borrow_mut().connection_lost();
        imp.link.typing_sender.borrow_mut().reset();
        let actions = imp
            .link
            .controller
            .borrow_mut()
            .disconnected(glib::random_double());
        imp.account_status.set_title("Reconnecting…");
        imp.composer.set_sensitive(false);
        self.link_perform(actions, generation);
        self.refresh_banner();
        self.refresh_delivery_rows();
    }

    fn link_resume(&self) {
        let imp = self.imp();
        let generation = imp.generation.get();
        let actions = imp.link.controller.borrow_mut().resume();
        self.link_perform(actions, generation);
        self.refresh_banner();
    }

    fn link_network(&self, available: bool) {
        let imp = self.imp();
        let generation = imp.generation.get();
        let actions = imp.link.controller.borrow_mut().network_changed(available);
        self.link_perform(actions, generation);
        self.refresh_banner();
    }

    fn link_timer(&self, generation: u64, token: u64) {
        let imp = self.imp();
        if imp.generation.get() != generation {
            return;
        }
        let actions = imp
            .link
            .controller
            .borrow_mut()
            .timer(token, glib::random_double());
        if !actions.is_empty() {
            imp.account_status.set_title("Reconnecting…");
            imp.composer.set_sensitive(false);
        }
        self.link_perform(actions, generation);
        self.refresh_banner();
    }

    /// Carry out what the controller asked for.
    fn link_perform(&self, actions: Vec<Action>, generation: u64) {
        for action in actions {
            match action {
                Action::Connect => {
                    if let Some(session) = self.imp().session.borrow().as_ref() {
                        session.connect();
                    }
                }
                Action::Arm { token, after } => {
                    let window = self.clone();
                    glib::timeout_add_local_once(after.max(Duration::from_millis(1)), move || {
                        window.link_timer(generation, token);
                    });
                }
            }
        }
    }

    pub(super) fn refresh_banner(&self) {
        let imp = self.imp();
        let text = imp.link.controller.borrow().banner();
        if let Some(banner) = imp.link.banner.borrow().as_ref() {
            if let Some(text) = text {
                banner.set_title(text);
            }
            banner.set_revealed(text.is_some());
        }
    }
}
