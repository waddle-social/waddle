//! Who a message is from: the state the profile surfaces share, how a
//! timeline row resolves its author's address, and the nick color, picture
//! and click-through to a profile that follow from it.

use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gdk, glib};
use waddle_client_session::TimelineItem;
use waddle_xmpp_client_ffi::WaddleRosterContact;

use super::conversation::Row;
use super::people::PeopleView;
use super::WaddleWindow;
use crate::avatars;
use crate::nick_color;
use crate::people::PresenceBook;
use crate::presence;
use crate::session::Kind;

/// What the presence, picture and profile surfaces keep for the signed-in
/// account. One field of the window, reset on sign-out.
#[derive(Default)]
pub struct ProfileState {
    pub presence: RefCell<presence::Controller>,
    pub avatars: RefCell<avatars::Cache<gdk::Texture>>,
    /// Contacts' and others' presence, for the People view.
    pub book: RefCell<PresenceBook>,
    pub contacts: RefCell<Vec<WaddleRosterContact>>,
    /// Avatars in dialogs, redrawn when their picture arrives.
    pub watchers: RefCell<Vec<(String, glib::WeakRef<adw::Avatar>)>>,
    pub people: RefCell<Option<PeopleView>>,
    /// The open profile dialog's toasts, so results show above it.
    pub toasts: RefCell<Option<glib::WeakRef<adw::ToastOverlay>>>,
}

impl ProfileState {
    pub fn reset(&self) {
        self.presence.borrow_mut().reset();
        self.avatars.borrow_mut().clear();
        self.book.borrow_mut().clear();
        self.contacts.borrow_mut().clear();
        self.watchers.borrow_mut().clear();
    }
}

impl WaddleWindow {
    /// This account's bare JID.
    pub(super) fn own_jid(&self) -> Option<String> {
        self.imp()
            .session
            .borrow()
            .as_ref()
            .map(|session| session.me.bare.to_string())
    }

    /// The address of whoever wrote `item` in the open conversation: the
    /// peer of a direct message, or a room participant whose real address
    /// the room exposes. A room that hides it leaves none.
    pub(super) fn author_jid(&self, item: &TimelineItem) -> Option<String> {
        let own = self.own_jid()?;
        if item.own {
            return Some(own);
        }
        let conversation = self.current_conversation()?;
        match conversation.kind {
            Kind::Direct => Some(conversation.jid),
            Kind::Room => item
                .author_id
                .clone()
                .filter(|id| id.parse::<jid::BareJid>().is_ok())
                .or_else(|| {
                    self.imp()
                        .membership
                        .borrow()
                        .real_jid(&conversation.jid, &item.sender)
                }),
        }
    }

    /// Color the sender's name (XEP-0392) and show their picture, for a
    /// row being bound.
    pub(super) fn decorate_author(
        &self,
        avatar: &adw::Avatar,
        sender: &gtk::Label,
        item: &TimelineItem,
    ) {
        let jid = self.author_jid(item);
        let in_room = self
            .current_conversation()
            .is_some_and(|conversation| conversation.kind == Kind::Room);
        let color = nick_color::hex(nick_color::identifier(
            in_room,
            &item.sender,
            jid.as_deref(),
        ));
        sender.set_markup(&format!(
            "<span foreground=\"{color}\">{}</span>",
            glib::markup_escape_text(&item.sender)
        ));
        let picture = jid.as_deref().and_then(|jid| self.avatar_texture(jid));
        avatar.set_custom_image(picture.as_ref());
        let clickable = jid.is_some() || in_room;
        let cursor = clickable.then_some("pointer");
        avatar.set_cursor_from_name(cursor);
        sender.set_cursor_from_name(cursor);
    }

    /// Redraw the timeline rows written by `jid`.
    pub(super) fn refresh_author_rows(&self, jid: &str) {
        let imp = self.imp();
        let Some(rows) = imp.rows.borrow().clone() else {
            return;
        };
        let wanted = jid.to_lowercase();
        for at in 0..rows.n_items() {
            let Some(object) = rows.item(at).and_downcast::<glib::BoxedAnyObject>() else {
                continue;
            };
            let author = self.author_jid(&object.borrow::<Row>().item);
            if author.is_some_and(|author| author.to_lowercase() == wanted) {
                rows.items_changed(at, 1, 1);
            }
        }
    }

    /// Open the profile of whoever wrote `item`.
    fn open_author(&self, item: &TimelineItem) {
        match self.author_jid(item) {
            Some(jid)
                if Some(jid.to_lowercase()) == self.own_jid().map(|own| own.to_lowercase()) =>
            {
                self.open_profile()
            }
            jid => self.open_person(jid, item.sender.clone()),
        }
    }
}

/// A click on a row's avatar or sender name opens that person's profile.
pub(super) fn attach_profile_gesture(
    widget: &gtk::Box,
    item: &gtk::ListItem,
    window: &glib::WeakRef<WaddleWindow>,
) {
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_PRIMARY)
        .build();
    let (item, owner, root) = (item.downgrade(), window.clone(), widget.downgrade());
    click.connect_released(move |_, _, x, y| {
        let (Some(item), Some(window), Some(root)) =
            (item.upgrade(), owner.upgrade(), root.upgrade())
        else {
            return;
        };
        let mut hit = root.pick(x, y, gtk::PickFlags::DEFAULT);
        let mut on_author = false;
        while let Some(widget) = hit {
            if widget.has_css_class("waddle-avatar") || widget.has_css_class("sender") {
                on_author = true;
                break;
            }
            if widget == *root.upcast_ref::<gtk::Widget>() {
                break;
            }
            hit = widget.parent();
        }
        if !on_author {
            return;
        }
        if let Some(object) = item.item().and_downcast::<glib::BoxedAnyObject>() {
            let row = object.borrow::<Row>().item.clone();
            window.open_author(&row);
        }
    });
    widget.add_controller(click);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::device::ServerUrl;
    use crate::auth::store::StoredAccount;
    use crate::presence::{ManualStatus, PresenceMode};
    use crate::session::{Conversation, Session};
    use waddle_client_session::{IncomingMessage, Timeline};

    /// Builds the window, signs a session in without a server to reach,
    /// and opens each dialog, so a template or borrow mistake in the
    /// profile surfaces shows up here rather than in front of a user.
    #[test]
    #[ignore = "requires a GTK display; run with xvfb-run and --ignored --test-threads=1"]
    fn profile_surfaces_open_and_resolve_authors() {
        adw::init().expect("GTK display");
        crate::install_crypto_provider();
        let app = adw::Application::builder()
            .application_id("social.waddle.ProfileTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>)
            .expect("register application");
        let window = WaddleWindow::new(&app);
        let imp = window.imp();
        let account = StoredAccount::new(
            ServerUrl::parse("http://127.0.0.1:1").expect("server"),
            serde_json::from_value(serde_json::json!({
                "session_id": "token",
                "username": "me",
                "jid": "me@localhost",
                "websocket_url": "ws://127.0.0.1:1/xmpp",
            }))
            .expect("session"),
        );
        let transfers =
            crate::attachments::Transfers::try_new(account.server.as_url()).expect("transfers");
        let (session, _events) = Session::start(&account, transfers);
        imp.session.replace(Some(session));
        imp.account.replace(Some(account));

        // A direct message from the peer resolves to the peer's address and
        // is colored by it; one from the account resolves to the account.
        let peer = Conversation {
            jid: "ann@localhost".into(),
            name: "ann".into(),
            kind: Kind::Direct,
            autojoin: false,
            flavor: Default::default(),
            thread: None,
        };
        imp.conversations.borrow_mut().push(peer.clone());
        imp.current.replace(Some(peer.key()));
        let mut timeline = Timeline::new();
        timeline.apply_live(IncomingMessage {
            stanza_id: Some("1".into()),
            reference_id: Some("1".into()),
            sender: "ann".into(),
            body: Some("hi".into()),
            timestamp_ms: 1,
            ..IncomingMessage::default()
        });
        timeline.apply_live(IncomingMessage {
            stanza_id: Some("2".into()),
            reference_id: Some("2".into()),
            sender: "me".into(),
            own: true,
            body: Some("hello".into()),
            timestamp_ms: 2,
            ..IncomingMessage::default()
        });
        let theirs = timeline.items()[0].clone();
        let mine = timeline.items()[1].clone();
        assert_eq!(window.author_jid(&theirs).as_deref(), Some("ann@localhost"));
        assert_eq!(window.author_jid(&mine).as_deref(), Some("me@localhost"));
        let (avatar, label) = (adw::Avatar::new(36, None, true), gtk::Label::new(None));
        window.decorate_author(&avatar, &label, &theirs);
        assert_eq!(label.text(), "ann");
        assert!(label.label().contains(&nick_color::hex("ann@localhost")));

        // Picking a status shows in the account row and is remembered.
        window.pick_presence(PresenceMode::Manual(ManualStatus::Away));
        assert_eq!(
            imp.profile.presence.borrow().mode(),
            PresenceMode::Manual(ManualStatus::Away)
        );
        assert!(imp.account_label.label().starts_with("Away"));

        window.open_profile();
        window.open_people();
        assert!(imp.profile.people.borrow().is_some());
        window.refresh_people();
        window.open_person(Some("ann@localhost".into()), "ann".into());
        window.open_person(None, "Someone".into());
        window.profile_toast("shown somewhere");
    }
}
