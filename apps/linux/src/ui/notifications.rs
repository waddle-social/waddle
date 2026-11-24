//! Notification modes and desktop notifications.
//!
//! The rules (modes, defaults, what is announced) are in `crate::notify`;
//! this module only connects them to the header menu, the sidebar and
//! GNotification.

use adw::prelude::{
    ActionMapExt, ApplicationExt, Cast, GtkWindowExt, ObjectExt, StaticVariantType, ToVariant,
    WidgetExt,
};
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};

use super::conversation::notification_id;
use super::WaddleWindow;
use crate::notify::{self, NotifyMode};
use crate::session::Conversation;

impl WaddleWindow {
    /// The header menu's `win.notify-mode` radio action.
    pub(super) fn setup_notifications(&self) {
        let action = gio::SimpleAction::new_stateful(
            "notify-mode",
            Some(&String::static_variant_type()),
            &NotifyMode::OnMention.id().to_variant(),
        );
        let window = self.downgrade();
        action.connect_activate(move |_, target| {
            let (Some(window), Some(mode)) = (
                window.upgrade(),
                target
                    .and_then(|target| target.get::<String>())
                    .and_then(|id| NotifyMode::from_id(&id)),
            ) else {
                return;
            };
            window.choose_notify_mode(mode);
        });
        self.add_action(&action);
    }

    /// The conversation a mode applies to: a thread uses its room's.
    fn notify_target(&self) -> Option<Conversation> {
        let current = self.current_conversation()?;
        if current.thread.is_none() {
            return Some(current);
        }
        self.imp()
            .conversations
            .borrow()
            .iter()
            .find(|known| known.jid == current.jid && known.thread.is_none())
            .cloned()
    }

    /// Show the open conversation's mode in the header button and menu.
    pub(super) fn refresh_notify_button(&self) {
        let imp = self.imp();
        let Some(target) = self.notify_target() else {
            imp.notify_button.set_visible(false);
            return;
        };
        let mode = imp.notifications.borrow().mode(target.kind, &target.jid);
        imp.notify_button.set_icon_name(mode.icon());
        imp.notify_button.set_visible(true);
        if let Some(action) = self
            .lookup_action("notify-mode")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_state(&mode.id().to_variant());
        }
    }

    fn choose_notify_mode(&self, mode: NotifyMode) {
        let imp = self.imp();
        let (Some(target), Some(session)) = (self.notify_target(), imp.session.borrow().clone())
        else {
            return;
        };
        let stored = imp.notifications.borrow().stored(target.kind, &target.jid);
        if stored.mode == Some(mode) {
            return;
        }
        let window = self.clone();
        let generation = imp.generation.get();
        glib::spawn_future_local(async move {
            let confirmed = session.set_notify_mode(&target, mode, stored.rich).await;
            if window.imp().generation.get() != generation {
                return;
            }
            let Some(confirmed) = confirmed else {
                window.toast("Could not change notifications.");
                window.refresh_notify_button();
                return;
            };
            window
                .imp()
                .notifications
                .borrow_mut()
                .record(target.kind, &target.jid, confirmed);
            if mode == NotifyMode::Never {
                window.withdraw_conversation_notifications(&target.jid);
            }
            window.refresh_notify_button();
            window.render_conversations();
        });
    }

    /// Whether the sidebar shows this conversation as muted.
    pub(super) fn is_muted(&self, conversation: &Conversation) -> bool {
        self.imp()
            .notifications
            .borrow()
            .is_muted(conversation.kind, &conversation.jid)
    }

    /// Announce a message that arrived while the user was not reading,
    /// unless the conversation's mode says otherwise.
    pub(super) fn announce_arrival(
        &self,
        conversation: &Conversation,
        mentions_me: bool,
        sender: &str,
        body: &str,
    ) {
        let imp = self.imp();
        if !imp
            .notifications
            .borrow()
            .wants(conversation.kind, &conversation.jid, mentions_me)
        {
            return;
        }
        let Some(app) = self.application() else {
            return;
        };
        let key = conversation.key();
        let room = imp
            .conversations
            .borrow()
            .iter()
            .find(|known| known.jid == conversation.jid && known.thread.is_none())
            .map(|known| known.name.clone())
            .unwrap_or_else(|| conversation.name.clone());
        let notification = gio::Notification::new(&notify::title(conversation.kind, sender, &room));
        notification.set_body(Some(&crate::message_text::preview(sender, body)));
        let target = key.to_variant();
        notification.set_default_action_and_target_value("app.open-conversation", Some(&target));
        notification.add_button_with_target_value("Reply", "app.reply-conversation", Some(&target));
        notification.add_button_with_target_value(
            "Mark as Read",
            "app.mark-conversation-read",
            Some(&target),
        );
        // The same id replaces the previous notification of this conversation.
        app.send_notification(Some(&notification_id(&key)), &notification);
        imp.notifications.borrow_mut().shown(&key);
    }

    /// Withdraw the notifications of conversations that are no longer unread.
    pub(super) fn withdraw_read_notifications(&self) {
        let imp = self.imp();
        let read = {
            let unread = imp.unread.borrow();
            imp.notifications
                .borrow_mut()
                .take_read(|key| unread.get(key).copied().unwrap_or(0) > 0)
        };
        if let Some(app) = self.application() {
            for key in read {
                app.withdraw_notification(&notification_id(&key));
            }
        }
    }

    /// Withdraw every notification of a conversation and its threads.
    fn withdraw_conversation_notifications(&self, jid: &str) {
        let imp = self.imp();
        let keys: Vec<String> = imp
            .conversations
            .borrow()
            .iter()
            .filter(|known| known.jid == jid)
            .map(Conversation::key)
            .collect();
        if let Some(app) = self.application() {
            for key in keys {
                app.withdraw_notification(&notification_id(&key));
                imp.notifications.borrow_mut().dismissed(&key);
            }
        }
    }

    /// A notification's "Mark as Read": read here and on the server.
    pub fn mark_conversation_read(&self, key: &str) {
        let imp = self.imp();
        let had_unread = imp.unread.borrow_mut().remove(key).is_some();
        if had_unread {
            imp.live_unread_keys.borrow_mut().insert(key.to_string());
        }
        self.queue_read(key);
        self.render_conversations();
    }

    /// A notification's "Reply": open the conversation, ready to type.
    pub fn reply_in_conversation(&self, key: &str) {
        self.present();
        self.open_conversation(key);
        self.imp().composer_entry.grab_focus();
    }
}
