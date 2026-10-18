//! What the user does to messages: send, reply, react, edit, delete.
//!
//! Each action updates the open conversation's timeline at once (the core
//! dedupes the server's echo against it) and undoes or flags it when the
//! send fails.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use waddle_client_session::{Delivery, IncomingMessage, LocalEcho, ReplyTo, TimelineItem};

use super::conversation::Row;
use super::WaddleWindow;
use crate::attachments::MAX_UPLOAD_BYTES;
use crate::session::{Kind, Outgoing, Quote};

/// One tap away in the message menu.
const QUICK_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "🎉", "😮", "😢"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageAction {
    Reply,
    Copy,
    Edit,
    Delete,
}

impl MessageAction {
    fn presentation(self) -> (&'static str, &'static str) {
        match self {
            Self::Reply => ("mail-reply-sender-symbolic", "Reply"),
            Self::Copy => ("edit-copy-symbolic", "Copy Text"),
            Self::Edit => ("document-edit-symbolic", "Edit"),
            Self::Delete => ("user-trash-symbolic", "Delete"),
        }
    }
}

/// What the composer's next send does.
#[derive(Debug, Clone, Default)]
pub enum Compose {
    #[default]
    Message,
    Reply(Quote),
    Edit {
        correction_id: String,
        /// The row as it was, to restore if the correction fails.
        original: Box<TimelineItem>,
    },
}

impl WaddleWindow {
    pub(super) fn setup_composer(&self) {
        let keys = gtk::EventControllerKey::new();
        let window = self.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(window) = window.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if key == gdk::Key::Escape
                && !matches!(*window.imp().compose.borrow(), Compose::Message)
            {
                window.cancel_compose();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.imp().composer_entry.add_controller(keys);

        // Files dropped on the conversation are sent to it.
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let window = self.downgrade();
        drop.connect_drop(move |_, value, _, _| {
            let (Some(window), Ok(files)) = (window.upgrade(), value.get::<gdk::FileList>()) else {
                return false;
            };
            window.send_files(files.files());
            true
        });
        self.imp().content_stack.add_controller(drop);
    }

    /// Why files cannot be attached right now, if they cannot.
    fn cannot_attach(&self) -> Option<&'static str> {
        let imp = self.imp();
        if imp.uploading.get() {
            return Some("Wait for the current upload to finish.");
        }
        // The entry holds the message being edited; it is not a caption.
        matches!(*imp.compose.borrow(), Compose::Edit { .. })
            .then_some("Finish editing the message first.")
    }

    /// The composer is usable when connected, in a conversation, and not
    /// waiting on an upload.
    pub(super) fn update_composer(&self) {
        let imp = self.imp();
        imp.composer.set_sensitive(
            imp.connected.get() && !imp.uploading.get() && imp.current.borrow().is_some(),
        );
    }

    pub(super) fn pick_files(&self) {
        // Say so before the chooser, not after a file is picked.
        if let Some(reason) = self.cannot_attach() {
            return self.toast(reason);
        }
        let dialog = gtk::FileDialog::builder().title("Attach Files").build();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let Ok(files) = dialog.open_multiple_future(Some(&window)).await else {
                return;
            };
            let files: Vec<gio::File> = files.iter::<gio::File>().filter_map(Result::ok).collect();
            window.send_files(files);
        });
    }

    /// Upload these files and send them in one message, with the
    /// composer's text as the caption.
    fn send_files(&self, files: Vec<gio::File>) {
        let imp = self.imp();
        let (Some(conversation), Some(session)) =
            (self.current_conversation(), imp.session.borrow().clone())
        else {
            return;
        };
        if files.is_empty() {
            return;
        }
        if let Some(reason) = self.cannot_attach() {
            return self.toast(reason);
        }
        imp.uploading.set(true);
        let generation = imp.generation.get();
        let caption = imp.composer_entry.text().trim().to_string();
        let quote = match imp.compose.replace(Compose::Message) {
            Compose::Reply(quote) => Some(quote),
            Compose::Message | Compose::Edit { .. } => None,
        };
        imp.composer_entry.set_text("");
        imp.composer_context_label.set_label(&match files.len() {
            1 => "Uploading 1 file…".to_string(),
            count => format!("Uploading {count} files…"),
        });
        imp.composer_context.set_visible(true);
        imp.composer.set_sensitive(false);

        let window = self.clone();
        glib::spawn_future_local(async move {
            let result = window.upload_all(&session, files).await;
            let imp = window.imp();
            // Signed out meanwhile: this belongs to a session that is gone.
            if imp.generation.get() != generation {
                return;
            }
            imp.uploading.set(false);
            if matches!(*imp.compose.borrow(), Compose::Message) {
                imp.composer_context.set_visible(false);
            }
            window.update_composer();
            let uploaded = match result {
                Ok(uploaded) => uploaded,
                Err(message) => {
                    // Give the caption and reply back, unless the user has
                    // moved on to another conversation or started typing.
                    let same = imp.current.borrow().as_deref() == Some(conversation.jid.as_str());
                    if same && imp.composer_entry.text().is_empty() {
                        imp.composer_entry.set_text(&caption);
                        if let Some(quote) = quote {
                            window.set_compose(Compose::Reply(quote));
                        }
                    }
                    return window.toast(&message);
                }
            };
            let origin = uuid::Uuid::new_v4().to_string();
            let nick = session.me.nick.clone();
            let echo = LocalEcho {
                origin_id: origin.clone(),
                sender: nick,
                body: caption.clone(),
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                reference_id: (conversation.kind == Kind::Direct).then(|| origin.clone()),
                reply_to: quote.as_ref().map(|quote| ReplyTo {
                    id: quote.id.clone(),
                    sender: Some(quote.sender.clone()),
                }),
                attachments: uploaded
                    .iter()
                    .map(|file| file.attachment.clone())
                    .collect(),
            };
            window.edit_conversation(&conversation.jid, |timeline| timeline.add_local_echo(echo));
            window.scroll_to_end();
            let wire = uploaded.into_iter().map(|file| file.wire).collect();
            let sent = session
                .send(
                    &conversation.jid,
                    conversation.kind,
                    caption,
                    origin.clone(),
                    quote,
                    wire,
                )
                .await;
            if !sent {
                window.edit_conversation(&conversation.jid, |timeline| {
                    timeline.set_delivery(&origin, Delivery::Failed)
                });
                window.toast("The message could not be sent.");
            }
        });
    }

    async fn upload_all(
        &self,
        session: &crate::session::Session,
        files: Vec<gio::File>,
    ) -> Result<Vec<crate::session::Uploaded>, String> {
        let mut uploaded = Vec::with_capacity(files.len());
        for file in files {
            let name = file
                .basename()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            // Check the size before reading: a dropped multi-gigabyte file
            // must not be read into memory first.
            let size = file
                .query_info_future(
                    gio::FILE_ATTRIBUTE_STANDARD_SIZE,
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::DEFAULT,
                )
                .await
                .map_err(|_| format!("{name} could not be read."))?
                .size();
            if u64::try_from(size).map_or(true, |size| size > MAX_UPLOAD_BYTES) {
                return Err(format!("{name} is larger than 10 MB."));
            }
            let (bytes, _) = file
                .load_contents_future()
                .await
                .map_err(|_| format!("{name} could not be read."))?;
            if bytes.len() as u64 > MAX_UPLOAD_BYTES {
                return Err(format!("{name} is larger than 10 MB."));
            }
            let (guess, _) = gio::content_type_guess(Some(name.as_str()), Some(&bytes[..]));
            let media_type = gio::content_type_get_mime_type(&guess)
                .map(|mime| mime.to_string())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let outgoing = Outgoing {
                name: name.clone(),
                media_type,
                bytes: bytes.to_vec(),
            };
            uploaded.push(
                session
                    .upload(outgoing)
                    .await
                    .map_err(|error| format!("{name} could not be uploaded: {error}"))?,
            );
        }
        Ok(uploaded)
    }

    pub(super) fn cancel_compose(&self) {
        let imp = self.imp();
        let editing = matches!(*imp.compose.borrow(), Compose::Edit { .. });
        imp.compose.replace(Compose::Message);
        // An upload in flight keeps its "Uploading…" note.
        imp.composer_context.set_visible(imp.uploading.get());
        if editing {
            imp.composer_entry.set_text("");
        }
    }

    fn set_compose(&self, compose: Compose) {
        let imp = self.imp();
        if imp.uploading.get() {
            return self.toast("Wait for the current upload to finish.");
        }
        let label = match &compose {
            Compose::Message => None,
            Compose::Reply(quote) => Some(format!("Replying to {}", quote.sender)),
            Compose::Edit { original, .. } => {
                imp.composer_entry.set_text(&original.body);
                Some("Editing message".to_string())
            }
        };
        imp.composer_context.set_visible(label.is_some());
        if let Some(label) = label {
            imp.composer_context_label.set_label(&label);
        }
        imp.compose.replace(compose);
        imp.composer_entry.grab_focus();
        imp.composer_entry.set_position(-1);
    }

    pub(super) fn send_current(&self) {
        let imp = self.imp();
        let body = imp.composer_entry.text().trim().to_string();
        let (Some(conversation), Some(session)) =
            (self.current_conversation(), imp.session.borrow().clone())
        else {
            return;
        };
        let compose = imp.compose.replace(Compose::Message);
        imp.composer_context.set_visible(false);
        imp.composer_entry.set_text("");
        let now = chrono::Utc::now().timestamp_millis();
        let nick = session.me.nick.clone();

        if let Compose::Edit {
            correction_id,
            original,
        } = compose
        {
            if body.is_empty() || body == original.body {
                return;
            }
            let jid = conversation.jid.clone();
            self.edit_conversation(&jid, |timeline| {
                timeline.apply_live(IncomingMessage {
                    replaces_id: Some(correction_id.clone()),
                    sender: nick,
                    own: true,
                    body: Some(body.clone()),
                    timestamp_ms: now,
                    ..IncomingMessage::default()
                })
            });
            let window = self.clone();
            glib::spawn_future_local(async move {
                if !session
                    .correct(&conversation.jid, conversation.kind, correction_id, body)
                    .await
                {
                    window.edit_conversation(&jid, |timeline| timeline.restore(&original));
                    window.toast("The edit could not be sent.");
                }
            });
            return;
        }

        if body.is_empty() {
            return;
        }
        let quote = match compose {
            Compose::Reply(quote) => Some(quote),
            Compose::Message | Compose::Edit { .. } => None,
        };
        let origin = uuid::Uuid::new_v4().to_string();
        let echo = LocalEcho {
            origin_id: origin.clone(),
            sender: nick,
            body: body.clone(),
            timestamp_ms: now,
            // A DM is referenced by the sender's id; a room assigns one.
            reference_id: (conversation.kind == Kind::Direct).then(|| origin.clone()),
            reply_to: quote.as_ref().map(|quote| ReplyTo {
                id: quote.id.clone(),
                sender: Some(quote.sender.clone()),
            }),
            attachments: Vec::new(),
        };
        self.edit_conversation(&conversation.jid, |timeline| timeline.add_local_echo(echo));
        self.scroll_to_end();

        let window = self.clone();
        glib::spawn_future_local(async move {
            let sent = session
                .send(
                    &conversation.jid,
                    conversation.kind,
                    body,
                    origin.clone(),
                    quote,
                    Vec::new(),
                )
                .await;
            if !sent {
                window.edit_conversation(&conversation.jid, |timeline| {
                    timeline.set_delivery(&origin, Delivery::Failed)
                });
                window.toast("The message could not be sent.");
            }
        });
    }

    /// XEP-0444 sends the user's complete set, so toggle within it.
    pub(super) fn toggle_reaction(&self, target: &str, emoji: &str) {
        let imp = self.imp();
        let (Some(conversation), Some(session)) =
            (self.current_conversation(), imp.session.borrow().clone())
        else {
            return;
        };
        let previous = imp
            .timelines
            .borrow()
            .get(&conversation.jid)
            .map(|timeline| timeline.own_reactions(target))
            .unwrap_or_default();
        let mut next = previous.clone();
        match next.iter().position(|held| held == emoji) {
            Some(at) => {
                next.remove(at);
            }
            None => next.push(emoji.to_string()),
        }
        let now = chrono::Utc::now().timestamp_millis();
        let nick = session.me.nick.clone();
        self.edit_conversation(&conversation.jid, |timeline| {
            timeline.apply_own_reaction(target, &nick, &next, now)
        });

        let (window, target) = (self.clone(), target.to_string());
        glib::spawn_future_local(async move {
            if session
                .react(&conversation.jid, conversation.kind, target.clone(), next)
                .await
            {
                return;
            }
            // Roll back, stamped after the optimistic set so it wins.
            window.edit_conversation(&conversation.jid, |timeline| {
                timeline.apply_own_reaction(&target, &nick, &previous, now + 1)
            });
            window.toast("The reaction could not be sent.");
        });
    }

    fn reply_to(&self, item: &TimelineItem) {
        let Some(id) = item.reference_id.clone() else {
            return;
        };
        self.set_compose(Compose::Reply(Quote {
            id,
            sender: item.sender.clone(),
            own: item.own,
            text: item.body.clone(),
        }));
    }

    fn edit(&self, item: &TimelineItem) {
        let Some(correction_id) = item.correction_id.clone() else {
            return;
        };
        self.set_compose(Compose::Edit {
            correction_id,
            original: Box::new(item.clone()),
        });
    }

    fn confirm_delete(&self, item: &TimelineItem) {
        if item.reference_id.is_none() {
            return;
        }
        let item = item.clone();
        let dialog = adw::AlertDialog::new(
            Some("Delete Message?"),
            Some("It is removed for everyone in the conversation."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let window = self.clone();
        dialog.connect_response(Some("delete"), move |_, _| window.delete(&item));
        dialog.present(Some(self));
    }

    fn delete(&self, item: &TimelineItem) {
        let imp = self.imp();
        let (Some(conversation), Some(session), Some(reference)) = (
            self.current_conversation(),
            imp.session.borrow().clone(),
            item.reference_id.clone(),
        ) else {
            return;
        };
        let nick = session.me.nick.clone();
        let original = item.clone();
        self.edit_conversation(&conversation.jid, |timeline| {
            timeline.apply_live(IncomingMessage {
                retracts_id: Some(reference.clone()),
                sender: nick,
                own: true,
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                ..IncomingMessage::default()
            })
        });
        let window = self.clone();
        glib::spawn_future_local(async move {
            if !session
                .retract(&conversation.jid, conversation.kind, reference)
                .await
            {
                window.edit_conversation(&conversation.jid, |timeline| timeline.restore(&original));
                window.toast("The message could not be deleted.");
            }
        });
    }

    /// The message menu: quick reactions, then what the user may do.
    pub(super) fn show_message_actions(&self, list_item: &gtk::ListItem, x: f64, y: f64) {
        let (Some(anchor), Some(object)) = (
            list_item.child(),
            list_item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        let item = object.borrow::<Row>().item.clone();
        if item.retracted {
            return;
        }
        let popover = gtk::Popover::builder().has_arrow(true).build();
        popover.set_parent(&anchor);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();

        if let Some(reference) = item.reference_id.clone() {
            let own = self
                .current_conversation()
                .and_then(|conversation| {
                    self.imp()
                        .timelines
                        .borrow()
                        .get(&conversation.jid)
                        .map(|timeline| timeline.own_reactions(&reference))
                })
                .unwrap_or_default();
            let emojis = gtk::Box::builder().spacing(2).build();
            for emoji in QUICK_REACTIONS {
                let button = gtk::ToggleButton::builder()
                    .label(emoji)
                    .active(own.iter().any(|held| held == emoji))
                    .build();
                button.add_css_class("flat");
                let (window, popover, target) = (self.clone(), popover.clone(), reference.clone());
                button.connect_clicked(move |_| {
                    popover.popdown();
                    window.toggle_reaction(&target, emoji);
                });
                emojis.append(&button);
            }
            content.append(&emojis);
        }

        let mut actions = Vec::new();
        if item.reference_id.is_some() {
            actions.push(MessageAction::Reply);
        }
        actions.push(MessageAction::Copy);
        if item.own && item.correction_id.is_some() && item.delivery != Delivery::Failed {
            actions.push(MessageAction::Edit);
        }
        if item.own && item.reference_id.is_some() {
            actions.push(MessageAction::Delete);
        }
        for action in actions {
            let (icon, label) = action.presentation();
            let button = gtk::Button::builder()
                .child(
                    &adw::ButtonContent::builder()
                        .icon_name(icon)
                        .label(label)
                        .halign(gtk::Align::Start)
                        .build(),
                )
                .build();
            button.add_css_class("flat");
            if action == MessageAction::Delete {
                button.add_css_class("error");
            }
            let (window, popover, item) = (self.clone(), popover.clone(), item.clone());
            button.connect_clicked(move |_| {
                popover.popdown();
                match action {
                    MessageAction::Reply => window.reply_to(&item),
                    MessageAction::Copy => window.clipboard().set_text(&item.body),
                    MessageAction::Edit => window.edit(&item),
                    MessageAction::Delete => window.confirm_delete(&item),
                }
            });
            content.append(&button);
        }

        popover.set_child(Some(&content));
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }
}
