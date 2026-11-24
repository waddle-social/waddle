//! Read markers on the window: telling the other side and our other
//! devices what we have read, and taking their word for what they have.
//! The rules are in `crate::read_sync`.

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;
use waddle_xmpp_client_ffi::WaddleMessage;

use super::conversation::notification_id;
use super::WaddleWindow;
use crate::read_sync::{self, Cursor, Signal};
use crate::session::Conversation;

impl WaddleWindow {
    /// `conversation` was just read here: send the XEP-0333 marker and
    /// publish the XEP-0490 cursor for its newest message from someone else.
    pub(super) fn publish_read(&self, conversation: &Conversation) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        if !imp.connected.get() || conversation.thread.is_some() {
            return;
        }
        let jid = conversation.jid.clone();
        let kind = conversation.kind;
        let plan = {
            let timelines = imp.timelines.borrow();
            let Some(timeline) = timelines.get(&jid) else {
                return;
            };
            imp.link
                .marks
                .borrow()
                .plan(&jid, kind, timeline.items(), &session.me.bare)
        };
        let Some(dispatch) = plan else {
            return;
        };
        let generation = imp.generation.get();
        let epoch = imp.connect_epoch.get();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let mut sent = true;
            if let Some(marker) = dispatch.marker.clone() {
                sent &= session.send_displayed(&jid, kind, marker).await;
            }
            if let Some(cursor) = dispatch.cursor.clone() {
                let known = window.imp().link.marks.borrow().publish_supported;
                let supported = match known {
                    Some(supported) => supported,
                    None => {
                        let probed = session.supports_cursor_publish().await;
                        window.imp().link.marks.borrow_mut().publish_supported = Some(probed);
                        probed
                    }
                };
                if supported {
                    sent &= session.publish_cursor(cursor).await;
                }
            }
            let imp = window.imp();
            if sent && imp.generation.get() == generation && imp.connect_epoch.get() == epoch {
                imp.link.marks.borrow_mut().published(&jid, &dispatch, kind);
            }
        });
    }

    /// Learn what a live message says about reading.
    pub(super) fn on_read_message(&self, message: &WaddleMessage) {
        let imp = self.imp();
        let Some(me) = imp
            .session
            .borrow()
            .as_ref()
            .map(|session| session.me.clone())
        else {
            return;
        };
        for signal in read_sync::signals(message, &me.bare) {
            match signal {
                Signal::Markable {
                    reference,
                    message_id,
                    ..
                } => imp
                    .link
                    .marks
                    .borrow_mut()
                    .note_markable(reference, message_id),
                Signal::PeerDisplayed { conversation, id } => {
                    let moved = {
                        let timelines = imp.timelines.borrow();
                        timelines.get(&conversation).is_some_and(|timeline| {
                            imp.link.marks.borrow_mut().peer_displayed(
                                &conversation,
                                &id,
                                timeline.items(),
                            )
                        })
                    };
                    if moved {
                        self.refresh_delivery_rows();
                    }
                }
                Signal::Cursors(cursors) => {
                    for cursor in cursors {
                        self.apply_cursor(&cursor);
                    }
                }
            }
        }
    }

    /// Another device of ours read a conversation: its unread count here
    /// drops to what came after the message the cursor names.
    pub(super) fn apply_cursor(&self, cursor: &Cursor) {
        let imp = self.imp();
        let Some(me) = imp
            .session
            .borrow()
            .as_ref()
            .map(|session| session.me.clone())
        else {
            return;
        };
        let Some(conversation) = imp
            .conversations
            .borrow()
            .iter()
            .find(|known| known.jid == cursor.conversation && known.thread.is_none())
            .cloned()
        else {
            return;
        };
        let remaining = {
            let timelines = imp.timelines.borrow();
            let Some(timeline) = timelines.get(&conversation.jid) else {
                return;
            };
            read_sync::remaining_after(cursor, conversation.kind, timeline.items(), &me.bare)
        };
        let Some(remaining) = remaining else {
            return;
        };
        imp.link.marks.borrow_mut().adopt(cursor);
        let held = imp
            .unread
            .borrow()
            .get(&conversation.key())
            .copied()
            .unwrap_or(0);
        if remaining >= held {
            return;
        }
        {
            let mut unread = imp.unread.borrow_mut();
            if remaining == 0 {
                unread.remove(&conversation.key());
            } else {
                unread.insert(conversation.key(), remaining);
            }
        }
        if remaining == 0 {
            if let Some(app) = self.application() {
                app.withdraw_notification(&notification_id(&conversation.key()));
            }
        }
        self.render_conversations();
    }

    /// After connecting: take our other devices' cursors, and be told when
    /// they move.
    pub(super) fn bootstrap_cursors(&self, generation: u64, epoch: u64) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let window = self.clone();
        glib::spawn_future_local(async move {
            session.subscribe_cursors();
            let cursors = read_sync::latest_per_conversation(session.cursors().await);
            let imp = window.imp();
            if imp.generation.get() != generation || imp.connect_epoch.get() != epoch {
                return;
            }
            for cursor in cursors {
                window.apply_cursor(&cursor);
            }
        });
    }
}
