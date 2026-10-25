//! Thread navigation keeps the XMPP destination separate from UI cache keys.

use adw::prelude::{BoxExt, WidgetExt};
use adw::subclass::prelude::ObjectSubclassIsExt;
use waddle_client_session::{IncomingMessage, Timeline, TimelineItem};

use super::WaddleWindow;
use crate::session::Conversation;
use crate::threads::Thread;

impl WaddleWindow {
    pub(super) fn update_inbox(&self, entry: &waddle_xmpp_client_ffi::WaddleInboxEntry) {
        if entry.kind == "direct" {
            self.ensure_direct(&entry.partner);
        }
        let base = self
            .imp()
            .conversations
            .borrow()
            .iter()
            .find(|known| known.jid == entry.partner && known.thread.is_none())
            .cloned();
        let Some(base) = base else {
            return;
        };
        let destination = match entry.thread_id.as_ref() {
            Some(id) => self.ensure_thread(
                &base,
                Thread {
                    id: id.clone(),
                    parent: None,
                },
                entry.thread_title.as_deref(),
            ),
            None => base,
        };
        let key = destination.key();
        self.imp()
            .unread
            .borrow_mut()
            .insert(key.clone(), entry.unread);
        self.render_conversations();
        if self.is_reading(&key) {
            self.read_current();
        }
    }
    /// Keep a thread's original message for context, including its verified IDs.
    pub(super) fn remember_thread_root(
        &self,
        conversation: &Conversation,
        message: &IncomingMessage,
    ) {
        if super::conversation::is_fastening(message) {
            return;
        }
        if let Some(reference) = message.reference_id.as_ref() {
            self.imp().thread_roots.borrow_mut().insert(
                (conversation.jid.clone(), reference.clone()),
                message.clone(),
            );
        }
    }

    pub(super) fn ensure_thread(
        &self,
        base: &Conversation,
        thread: Thread,
        title: Option<&str>,
    ) -> Conversation {
        let mut conversation = base.clone();
        conversation.thread = Some(thread.clone());
        conversation.autojoin = false;
        let root = self
            .imp()
            .thread_roots
            .borrow()
            .get(&(base.jid.clone(), thread.id.clone()))
            .cloned();
        let fallback = root
            .as_ref()
            .and_then(|message| message.body.as_deref())
            .unwrap_or(&thread.id);
        conversation.name = title.unwrap_or(fallback).chars().take(60).collect();
        let key = conversation.key();
        {
            let mut conversations = self.imp().conversations.borrow_mut();
            if let Some(known) = conversations.iter_mut().find(|known| known.key() == key) {
                if title.is_some() || known.name == thread.id {
                    known.name.clone_from(&conversation.name);
                }
                // An inbox title may arrive before thread parent metadata.
                if known
                    .thread
                    .as_ref()
                    .is_some_and(|known| known.parent.is_none())
                {
                    known.thread = Some(thread);
                }
                conversation = known.clone();
            } else {
                let at = conversations
                    .iter()
                    .rposition(|known| known.jid == base.jid)
                    .map_or(conversations.len(), |at| at + 1);
                conversations.insert(at, conversation.clone());
            }
        }
        conversation
    }

    /// The original message is context above the paged replies, so it
    /// cannot interfere with the thread archive's ordering or cursor.
    pub(super) fn refresh_thread_context(&self) {
        let container = &self.imp().thread_context;
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }
        let Some(conversation) = self.current_conversation() else {
            container.set_visible(false);
            return;
        };
        let Some(thread) = conversation.thread.as_ref() else {
            container.set_visible(false);
            return;
        };
        container.set_visible(true);
        let root = self
            .imp()
            .thread_roots
            .borrow()
            .get(&(conversation.jid.clone(), thread.id.clone()))
            .cloned();
        let mut timeline = Timeline::new();
        if let Some(root) = root {
            let updates = self
                .imp()
                .thread_updates
                .borrow()
                .get(&conversation.jid)
                .map(|updates| updates.for_message(&root))
                .unwrap_or_default();
            timeline.apply_live(root);
            for update in updates {
                timeline.apply_live(update);
            }
        }
        let text = match timeline.items().first() {
            Some(item) if item.retracted => "Original message deleted".to_string(),
            Some(item) => format!("{}: {}", item.sender, item.body),
            None => "Original message not loaded".to_string(),
        };
        let label = gtk::Label::builder()
            .label(&text)
            .xalign(0.0)
            .wrap(true)
            .lines(3)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        label.add_css_class("dim-label");
        label.set_tooltip_text(Some(&text));
        container.append(&label);
        if let Some(item) = timeline.items().first() {
            let badges = gtk::Box::new(gtk::Orientation::Vertical, 4);
            super::judgments::bind(&badges, item);
            container.append(&badges);
        }
    }

    pub(super) fn open_message_thread(&self, item: &TimelineItem, nested: bool) {
        let Some(base) = self.current_conversation() else {
            return;
        };
        let Some(reference) = item.reference_id.as_ref() else {
            return;
        };
        let thread = match (&base.thread, nested) {
            (Some(thread), false) => thread.clone(),
            (parent, _) => Thread {
                id: reference.clone(),
                parent: parent.as_ref().map(|thread| thread.id.clone()),
            },
        };
        let conversation = self.ensure_thread(&base, thread, Some(&item.body));
        self.open_conversation(&conversation.key());
    }

    pub(super) fn replay_thread_updates(
        &self,
        conversation: &Conversation,
        message: &IncomingMessage,
    ) {
        let updates = self
            .imp()
            .thread_updates
            .borrow()
            .get(&conversation.jid)
            .map(|updates| updates.for_message(message))
            .unwrap_or_default();
        for update in updates {
            self.edit_conversation(&conversation.key(), |timeline| timeline.apply_live(update));
        }
    }

    /// Apply a threadless fastening to all cached views of its destination.
    pub(super) fn broadcast_fastening(&self, jid: &str, message: IncomingMessage) {
        self.imp()
            .thread_updates
            .borrow_mut()
            .entry(jid.to_string())
            .or_default()
            .insert(message.clone());
        let keys: Vec<String> = self
            .imp()
            .conversations
            .borrow()
            .iter()
            .filter(|conversation| conversation.jid == jid)
            .map(Conversation::key)
            .collect();
        for key in keys {
            self.edit_conversation(&key, |timeline| timeline.apply_live(message.clone()));
        }
        self.refresh_thread_context();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Kind;
    use waddle_client_session::HistoryPage;
    use adw::prelude::{ApplicationExt, ButtonExt, CastNone, GtkWindowExt, ListModelExt};
    use gtk::gio;
    use waddle_client_session::judgments::{
        Probability, SafetyCategory, SafetyScore, SafetyScores, SafetyScoresFastening,
    };

    #[test]
    #[ignore = "requires a GTK display; run with xvfb-run and --ignored --test-threads=1"]
    fn thread_navigation_and_context() {
        adw::init().expect("GTK display");
        let app = adw::Application::builder()
            .application_id("social.waddle.ThreadTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>)
            .expect("register application");
        let window = WaddleWindow::new(&app);
        let base = Conversation {
            jid: "general@rooms.example".into(),
            name: "General".into(),
            kind: Kind::Room,
            autojoin: false,
            thread: None,
        };
        window.imp().conversations.borrow_mut().push(base.clone());
        let target =
            crate::judgments::target(&base.jid.parse().unwrap(), Some("root"), Some("original"))
                .unwrap();
        let root = IncomingMessage {
            stanza_id: Some("root".into()),
            reference_id: Some("root".into()),
            origin_id: Some("original".into()),
            sender: "Alice".into(),
            body: Some("Original text".into()),
            timestamp_ms: 1,
            score_target: Some(target.clone()),
            ..IncomingMessage::default()
        };
        window.remember_thread_root(&base, &root);
        window.edit_conversation(&base.key(), |timeline| timeline.apply_live(root));
        let thread = window.ensure_thread(
            &base,
            Thread {
                id: "root".into(),
                parent: None,
            },
            None,
        );
        window.open_conversation(&thread.key());
        assert_eq!(window.current_conversation(), Some(thread.clone()));
        assert!(window.imp().thread_back.get_visible());
        assert_eq!(
            window.imp().rows.borrow().as_ref().unwrap().n_items(),
            0,
            "root context must not contaminate reply history"
        );
        window.broadcast_fastening(
            &base.jid,
            IncomingMessage {
                timestamp_ms: 2,
                safety_scores: Some(SafetyScoresFastening {
                    target,
                    source_revision_id: "root".into(),
                    scores: SafetyScores {
                        model_version: "jev-v1".into(),
                        scores: vec![SafetyScore {
                            category: SafetyCategory::IsQuestion,
                            probability: Probability::try_new(0.95).unwrap(),
                            taxonomy_version: "question-v1".into(),
                        }],
                    },
                }),
                ..IncomingMessage::default()
            },
        );
        let badges = window
            .imp()
            .thread_context
            .last_child()
            .and_downcast::<gtk::Box>()
            .unwrap();
        let expander = badges
            .first_child()
            .and_downcast::<gtk::Expander>()
            .expect("late Jev badge renders");
        expander.set_expanded(true);
        assert!(expander.is_expanded());
        let child = IncomingMessage {
            stanza_id: Some("child".into()),
            reference_id: Some("child".into()),
            sender: "Bob".into(),
            body: Some("Reply".into()),
            timestamp_ms: 2,
            ..IncomingMessage::default()
        };
        window.edit_conversation(&thread.key(), |timeline| {
            timeline.apply_history_page(vec![child], Some("child".into()), HistoryPage::Complete)
        });
        window.on_arrival(crate::session::Arrival {
            conversation: base.jid.clone(),
            kind: Kind::Room,
            thread: None,
            message: IncomingMessage {
                stanza_id: Some("legacy".into()),
                reference_id: Some("legacy".into()),
                sender: "Carol".into(),
                body: Some("Legacy reply".into()),
                timestamp_ms: 3,
                reply_to: Some(waddle_client_session::ReplyTo {
                    id: "root".into(),
                    sender: Some("Alice".into()),
                }),
                ..IncomingMessage::default()
            },
            mentions_me: false,
            delayed: true,
        });
        assert_eq!(
            window.imp().rows.borrow().as_ref().unwrap().n_items(),
            2,
            "live legacy replies appear immediately in the selected thread"
        );
        assert_eq!(
            window
                .imp()
                .timelines
                .borrow()
                .get(&base.key())
                .unwrap()
                .items()
                .len(),
            2,
            "legacy replies retain their main-conversation copy"
        );
        window.broadcast_fastening(
            &base.jid,
            IncomingMessage {
                sender: "Alice".into(),
                replaces_id: Some("original".into()),
                body: Some("Corrected text".into()),
                timestamp_ms: 3,
                ..IncomingMessage::default()
            },
        );
        let context = window
            .imp()
            .thread_context
            .first_child()
            .and_downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(context.text(), "Alice: Corrected text");
        let badges = window
            .imp()
            .thread_context
            .last_child()
            .and_downcast::<gtk::Box>()
            .unwrap();
        assert!(
            badges.first_child().is_none(),
            "an old revision's badge must disappear"
        );
        assert_eq!(window.imp().rows.borrow().as_ref().unwrap().n_items(), 2);
        window.broadcast_fastening(
            &base.jid,
            IncomingMessage {
                sender: "Alice".into(),
                retracts_id: Some("root".into()),
                timestamp_ms: 4,
                ..IncomingMessage::default()
            },
        );
        let context = window
            .imp()
            .thread_context
            .first_child()
            .and_downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(context.text(), "Original message deleted");
        window.imp().thread_back.emit_clicked();
        assert_eq!(window.current_conversation(), Some(base));
        assert!(!window.imp().thread_context.get_visible());
        assert!(!window.imp().thread_back.get_visible());
        // Destroy before the queued asynchronous account restore runs.
        window.close();
    }
}
