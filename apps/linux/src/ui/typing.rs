//! Typing indicators on the window: the composer sends chat states, and a
//! line under the timeline says who is typing. The rules are in
//! `crate::typing`.

use std::time::Instant;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;
use waddle_xmpp_client_ffi::WaddleMessage;

use super::WaddleWindow;
use crate::typing::{self, Send};

impl WaddleWindow {
    /// Once per window: the line above the composer, and the composer's
    /// edits as chat states.
    pub(super) fn setup_typing(&self) {
        let imp = self.imp();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .margin_start(8)
            .margin_top(4)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .visible(false)
            .build();
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        // Between the reply or edit note and the composer.
        if let Some(column) = imp.composer.parent().and_downcast::<gtk::Box>() {
            column.insert_child_after(&label, Some(&*imp.composer_context));
        }
        imp.link.typing_label.replace(Some(label));

        let window = self.downgrade();
        imp.composer_entry.buffer().connect_changed(move |buffer| {
            if let Some(window) = window.upgrade() {
                let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
                window.on_composer_edited(text.as_str());
            }
        });
    }

    fn on_composer_edited(&self, text: &str) {
        let imp = self.imp();
        let Some(conversation) = self.current_conversation() else {
            return;
        };
        // Editing a sent message, running a command or an empty field is
        // not typing; the connection must be there to say anything.
        let editing = matches!(*imp.compose.borrow(), super::actions::Compose::Edit { .. });
        let quiet = editing || text.starts_with('/') || !imp.connected.get();
        let sends = imp.link.typing_sender.borrow_mut().edited(
            &conversation.key(),
            quiet || text.trim().is_empty(),
            Instant::now(),
        );
        self.send_chat_states(sends);
        self.ensure_typing_tick();
    }

    /// The user went to another conversation: what they were typing in the
    /// first is no longer going on.
    pub(super) fn typing_switched(&self) {
        let sends = self.imp().link.typing_sender.borrow_mut().end();
        self.send_chat_states(sends);
        self.refresh_typing_line();
    }

    fn send_chat_states(&self, sends: Vec<Send>) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        for send in sends {
            let conversation = imp
                .conversations
                .borrow()
                .iter()
                .find(|known| known.key() == send.key)
                .cloned();
            if let Some(conversation) = conversation {
                session.send_chat_state(&conversation.jid, conversation.kind, send.state);
            }
        }
    }

    /// Say `paused` when the user stops, and drop what has expired, once a
    /// second for as long as either is pending.
    pub(super) fn ensure_typing_tick(&self) {
        let imp = self.imp();
        if imp.link.typing_ticking.replace(true) {
            return;
        }
        let window = self.downgrade();
        glib::timeout_add_seconds_local(1, move || {
            let Some(window) = window.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let imp = window.imp();
            let now = Instant::now();
            let sends = imp.link.typing_sender.borrow_mut().tick(now);
            window.send_chat_states(sends);
            let changed = imp.link.typing_book.borrow_mut().sweep(now);
            if !changed.is_empty() {
                window.refresh_typing_line();
            }
            let busy = imp.link.typing_sender.borrow().is_composing()
                || !imp.link.typing_book.borrow().is_empty();
            if busy {
                glib::ControlFlow::Continue
            } else {
                imp.link.typing_ticking.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    /// A live message: someone's chat state, or the end of it.
    pub(super) fn on_typing_message(&self, message: &WaddleMessage) {
        let imp = self.imp();
        let Some(me) = imp
            .session
            .borrow()
            .as_ref()
            .map(|session| session.me.clone())
        else {
            return;
        };
        let Some(heard) = typing::heard(message, me.bare.as_str(), &me.nick) else {
            return;
        };
        if imp
            .link
            .typing_book
            .borrow_mut()
            .apply(&heard, Instant::now())
        {
            self.refresh_typing_line();
        }
        self.ensure_typing_tick();
    }

    pub(super) fn refresh_typing_line(&self) {
        let imp = self.imp();
        let Some(label) = imp.link.typing_label.borrow().clone() else {
            return;
        };
        let Some(conversation) = self.current_conversation() else {
            label.set_visible(false);
            return;
        };
        let names = imp
            .link
            .typing_book
            .borrow()
            .names(&conversation.jid, |who| {
                // A DM peer is keyed by address; show the name the sidebar does.
                imp.conversations
                    .borrow()
                    .iter()
                    .find(|known| known.jid == who && known.kind == crate::session::Kind::Direct)
                    .map_or_else(|| who.to_string(), |known| known.name.clone())
            });
        let line = typing::line(&names);
        label.set_label(&line);
        label.set_visible(!line.is_empty());
    }
}
