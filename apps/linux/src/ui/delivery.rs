//! Delivery states on the window: the unsent queue, Retry and Discard, and
//! sending again after a reconnect or a restart.
//!
//! The rules (what a state is, what is kept, what is due) are in
//! `crate::delivery`; this connects them to the timeline and the session.

use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;
use waddle_client_session::{Delivery, LocalEcho, ReplyTo, TimelineItem};

use super::WaddleWindow;
use crate::delivery::{self, EntryState, Outbox, Pending, Shown};
use crate::session::{Conversation, Draft, Kind, Session};

impl WaddleWindow {
    /// Open the account's queue and show what it holds. Called once the
    /// session exists.
    pub(super) fn link_load_outbox(&self) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let outbox = match delivery::state_dir() {
            Some(dir) => Outbox::load(delivery::outbox_path(&dir, session.me.bare.as_str())),
            None => Outbox::in_memory(),
        };
        imp.link.outbox.replace(outbox);
        let held = imp.link.outbox.borrow().entries().to_vec();
        for pending in held {
            let echo = echo_of(&pending, &session);
            self.edit_conversation(&pending.key.clone(), |timeline| {
                timeline.add_local_echo(echo)
            });
            if pending.state == EntryState::Failed {
                self.edit_conversation(&pending.key, |timeline| {
                    timeline.set_delivery(&pending.origin_id, Delivery::Failed)
                });
            }
        }
    }

    /// Send a message the user composed. It is queued on disk first, then
    /// written; whatever happens is reflected on its row.
    pub(super) async fn send_tracked(
        &self,
        session: &Session,
        conversation: &Conversation,
        draft: Draft,
        created_ms: i64,
    ) {
        let imp = self.imp();
        let generation = imp.generation.get();
        let origin = draft.origin_id.clone();
        let pending = Pending::new(
            origin.clone(),
            conversation.key(),
            conversation.jid.clone(),
            conversation.kind,
            &draft,
            created_ms,
        );
        imp.link.outbox.borrow_mut().enqueue(pending);
        imp.link
            .typing_sender
            .borrow_mut()
            .message_sent(&conversation.key());
        let written = if self.can_send(conversation) {
            session
                .send(&conversation.jid, conversation.kind, draft)
                .await
        } else {
            false
        };
        if imp.generation.get() != generation {
            return;
        }
        self.after_write(&origin, &conversation.key(), written);
    }

    /// What a write did to the message's row and queue entry.
    fn after_write(&self, origin: &str, key: &str, written: bool) {
        let imp = self.imp();
        let connected = imp.connected.get();
        if written {
            imp.link.outbox.borrow_mut().written(origin);
        } else {
            imp.link.outbox.borrow_mut().not_written(origin, connected);
            if connected {
                imp.outbound.borrow_mut().remove(origin);
                self.edit_conversation(key, |timeline| {
                    timeline.set_delivery(origin, Delivery::Failed)
                });
                self.toast("The message could not be sent.");
            } else {
                self.toast("You are offline. The message will be sent when you reconnect.");
            }
        }
        self.refresh_delivery_rows();
    }

    /// The server acknowledged a send (XEP-0198).
    pub(super) fn on_delivery_ack(&self, id: &str) {
        self.imp().link.outbox.borrow_mut().acknowledged(id);
        self.set_delivery(id, Delivery::Sent);
        self.refresh_delivery_rows();
    }

    /// The transport could not carry a send.
    pub(super) fn on_delivery_failed(&self, id: &str) {
        self.imp().link.outbox.borrow_mut().failed(id);
        self.set_delivery(id, Delivery::Failed);
        self.refresh_delivery_rows();
    }

    /// A message of ours came back (the room's reflection, or a copy from
    /// another of our devices): it is on the server.
    pub(super) fn on_own_message_seen(&self, origin_id: &str) {
        let held = self.imp().link.outbox.borrow().get(origin_id).is_some();
        if held {
            self.imp().link.outbox.borrow_mut().acknowledged(origin_id);
            self.refresh_delivery_rows();
        }
    }

    /// The server refused it outright (a stanza error). Sending the same
    /// message again would be refused the same way, so it is not offered.
    pub(super) fn outbox_rejected(&self, id: &str) {
        self.imp().link.rejected.borrow_mut().insert(id.to_string());
        self.imp().link.outbox.borrow_mut().failed(id);
        self.refresh_delivery_rows();
    }

    /// Whether a failed message can be sent again (it is in the queue).
    pub(super) fn can_retry(&self, item: &TimelineItem) -> bool {
        let link = &self.imp().link;
        item.correction_id.as_deref().is_some_and(|origin| {
            link.outbox.borrow().get(origin).is_some() && !link.rejected.borrow().contains(origin)
        })
    }

    pub(super) fn retry_send(&self, item: &TimelineItem) {
        let Some(origin) = item.correction_id.clone() else {
            return;
        };
        let imp = self.imp();
        let Some(pending) = imp.link.outbox.borrow().get(&origin).cloned() else {
            return self.toast("That message cannot be sent again. Discard it and write it again.");
        };
        imp.link.outbox.borrow_mut().retry(&origin);
        // Back to "sending" on its row; a refusal that sticks (a server
        // rejection) keeps the row failed, and the toast says so.
        self.edit_conversation(&pending.key, |timeline| {
            timeline.set_delivery(&origin, Delivery::Sending)
        });
        self.refresh_delivery_rows();
        if imp.connected.get() {
            self.flush_outbox();
        } else {
            self.toast("You are offline. The message will be sent when you reconnect.");
        }
    }

    pub(super) fn discard_send(&self, item: &TimelineItem) {
        let Some(origin) = item.correction_id.clone() else {
            return;
        };
        let imp = self.imp();
        imp.link.outbox.borrow_mut().discard(&origin);
        imp.outbound.borrow_mut().remove(&origin);
        imp.link.discarded.borrow_mut().insert(origin);
        self.refresh_delivery_rows();
    }

    pub(super) fn is_discarded(&self, item: &TimelineItem) -> bool {
        item.correction_id
            .as_deref()
            .is_some_and(|origin| self.imp().link.discarded.borrow().contains(origin))
    }

    /// What an own message of the open conversation shows, and whether it
    /// is the newest of the user's messages there.
    pub(super) fn delivery_shown(&self, item: &TimelineItem) -> Option<(Shown, bool)> {
        if !item.own || item.retracted {
            return None;
        }
        let imp = self.imp();
        let key = imp.current.borrow().clone()?;
        let entry = item
            .correction_id
            .as_deref()
            .and_then(|origin| imp.link.outbox.borrow().state(origin));
        let jid = self.current_conversation()?.jid;
        let read = imp.link.marks.borrow().is_read(&jid, item);
        let latest = imp.timelines.borrow().get(&key).is_some_and(|timeline| {
            timeline
                .items()
                .iter()
                .rev()
                .find(|other| other.own && !other.retracted && !self.is_discarded(other))
                .is_some_and(|other| other.key == item.key)
        });
        Some((delivery::shown(item.delivery, entry, read), latest))
    }

    /// Rebind the rows: their notes come from state the timeline does not
    /// carry (the queue, read marks, discards).
    pub(super) fn refresh_delivery_rows(&self) {
        if let Some(rows) = self.imp().rows.borrow().as_ref() {
            let count = rows.n_items();
            if count > 0 {
                rows.items_changed(0, count, count);
            }
        }
    }

    /// Write what is queued, oldest first: after connecting, after a room
    /// is joined, after Retry. A message already in the archive (it got
    /// through before the connection died) is not sent again.
    pub(super) fn flush_outbox(&self) {
        let imp = self.imp();
        if imp.link.flushing.get() || !imp.connected.get() {
            return;
        }
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let due: Vec<Pending> = imp
            .link
            .outbox
            .borrow()
            .due()
            .into_iter()
            .filter(|pending| self.can_send(&conversation_of(pending)))
            .collect();
        if due.is_empty() {
            return;
        }
        imp.link.flushing.set(true);
        let generation = imp.generation.get();
        let epoch = imp.connect_epoch.get();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let mut archive: HashMap<String, Option<HashSet<String>>> = HashMap::new();
            for pending in due {
                let imp = window.imp();
                if imp.generation.get() != generation
                    || imp.connect_epoch.get() != epoch
                    || !imp.connected.get()
                {
                    break;
                }
                // The user may have discarded it while this ran.
                if imp.link.outbox.borrow().state(&pending.origin_id) != Some(EntryState::Queued) {
                    continue;
                }
                let held = match archive.get(&pending.jid) {
                    Some(held) => held.clone(),
                    None => {
                        let found = session
                            .recent_origin_ids(&pending.jid, pending.kind())
                            .await;
                        archive.insert(pending.jid.clone(), found.clone());
                        found
                    }
                };
                if imp.generation.get() != generation || imp.connect_epoch.get() != epoch {
                    break;
                }
                if held.is_some_and(|ids| ids.contains(&pending.origin_id)) {
                    imp.link
                        .outbox
                        .borrow_mut()
                        .acknowledged(&pending.origin_id);
                    window.set_delivery(&pending.origin_id, Delivery::Sent);
                    continue;
                }
                imp.outbound
                    .borrow_mut()
                    .insert(pending.origin_id.clone(), conversation_of(&pending));
                let written = session
                    .send(&pending.jid, pending.kind(), pending.draft())
                    .await;
                if imp.generation.get() != generation {
                    return;
                }
                if written {
                    imp.link.outbox.borrow_mut().written(&pending.origin_id);
                } else {
                    imp.link
                        .outbox
                        .borrow_mut()
                        .not_written(&pending.origin_id, imp.connected.get());
                    window.edit_conversation(&pending.key, |timeline| {
                        timeline.set_delivery(&pending.origin_id, Delivery::Failed)
                    });
                }
            }
            let imp = window.imp();
            if imp.generation.get() == generation {
                imp.link.flushing.set(false);
                window.refresh_delivery_rows();
            }
        });
    }
}

fn echo_of(pending: &Pending, session: &Session) -> LocalEcho {
    LocalEcho {
        origin_id: pending.origin_id.clone(),
        sender: session.me.nick.clone(),
        body: pending.body.clone(),
        timestamp_ms: pending.created_ms,
        reference_id: pending.direct.then(|| pending.origin_id.clone()),
        reply_to: pending.quote.as_ref().map(|quote| ReplyTo {
            id: quote.id.clone(),
            sender: Some(quote.sender.clone()),
        }),
        attachments: Vec::new(),
    }
}

/// The conversation a queued message belongs to, as far as sending needs.
fn conversation_of(pending: &Pending) -> Conversation {
    Conversation {
        jid: pending.jid.clone(),
        name: pending.jid.clone(),
        kind: if pending.direct {
            Kind::Direct
        } else {
            Kind::Room
        },
        autojoin: false,
        thread: pending
            .thread
            .as_ref()
            .map(|thread| crate::threads::Thread {
                id: thread.id.clone(),
                parent: thread.parent.clone(),
            }),
        flavor: Default::default(),
    }
}
