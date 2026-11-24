//! Creating, editing, leaving and moderating rooms, and group DMs.
//!
//! The rules (what may be offered to whom, addresses, what an edit
//! changes) are in `rooms.rs`; the protocol calls are in
//! `session_rooms.rs`. This module is the dialogs and the bookkeeping that
//! keeps the sidebar in step: the new-room and new-group dialogs, the room
//! details dialog (settings for owners, people with their moderation
//! actions, bots, leave and delete) and the group details dialog (rename,
//! add people, leave).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};

use super::{avatar, WaddleWindow};
use crate::composer::Mentionable;
use crate::rooms::{self, Action, Affiliation, Person, Roster, Settings, Standing};
use crate::session::{Conversation, Flavor, Kind, Session};
use crate::session_rooms::{occupant_from, Bot, Members, RoomError};

/// Rooms' rosters and the account's standing in each, from room presence.
#[derive(Default)]
pub struct RoomsState {
    rosters: HashMap<String, Roster>,
    standing: HashMap<String, Standing>,
}

/// The sidebar tile: `#` for a room, a forum or group icon for the others.
pub(super) fn room_tile(flavor: Flavor) -> gtk::Widget {
    let tile: gtk::Widget = match flavor {
        Flavor::Plain => gtk::Label::builder().label("#").build().upcast(),
        Flavor::Forum => gtk::Image::from_icon_name("chat-bubbles-text-symbolic").upcast(),
        Flavor::Group => gtk::Image::from_icon_name("system-users-symbolic").upcast(),
    };
    tile.set_size_request(28, 28);
    tile.add_css_class("room-tile");
    tile
}

fn error_text(action: &str, error: &RoomError) -> String {
    rooms::failure_text(action, error.condition.as_deref())
}

/// A dialog whose content can show toasts.
struct Shell {
    dialog: adw::Dialog,
    toasts: adw::ToastOverlay,
}

impl Shell {
    fn new(title: &str, width: i32, content: &impl IsA<gtk::Widget>, end: &[&gtk::Button]) -> Self {
        let header = adw::HeaderBar::new();
        for button in end {
            header.pack_end(*button);
        }
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(content));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&toasts));
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(width)
            .content_height(560)
            .child(&view)
            .build();
        Self { dialog, toasts }
    }

    fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    /// Show the dialog. The handlers hold the shell weakly, so it is kept
    /// alive until the dialog closes.
    fn present(shell: &Rc<Self>, parent: &impl IsA<gtk::Widget>) {
        let keep = Rc::new(RefCell::new(Some(Rc::clone(shell))));
        shell.dialog.connect_closed(move |_| {
            keep.borrow_mut().take();
        });
        shell.dialog.present(Some(parent));
    }
}

fn form_box() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(12)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build()
}

fn boxed_list() -> gtk::ListBox {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");
    list
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

fn heading(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).build();
    label.add_css_class("heading");
    label
}

fn note(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .build();
    label.add_css_class("dim-label");
    label
}

fn badge(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .valign(gtk::Align::Center)
        .build();
    label.add_css_class("role-badge");
    label
}

fn confirm(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    body: &str,
    verb: &str,
    destructive: bool,
    then: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::new(Some(title), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", verb)]);
    dialog.set_response_appearance(
        "confirm",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(Some("confirm"), move |_, _| then());
    dialog.present(Some(parent));
}

impl WaddleWindow {
    pub(super) fn setup_room_tools(&self) {
        let new_room = gio::ActionEntry::builder("new-room")
            .activate(|window: &WaddleWindow, _, _| window.show_new_room())
            .build();
        let new_group = gio::ActionEntry::builder("new-group-dm")
            .activate(|window: &WaddleWindow, _, _| window.show_new_group())
            .build();
        let details = gio::ActionEntry::builder("room-details")
            .activate(|window: &WaddleWindow, _, _| window.show_details())
            .build();
        self.add_action_entries([new_room, new_group, details]);
    }

    /// The header's details button belongs to rooms and group DMs.
    pub(super) fn refresh_room_tools(&self) {
        let show = self
            .current_conversation()
            .is_some_and(|conversation| conversation.kind == Kind::Room);
        self.imp().details_button.set_visible(show);
    }

    pub(super) fn clear_rosters(&self) {
        self.imp().rooms.replace(RoomsState::default());
    }

    /// Keep the room rosters, the account's standing, and react to a room
    /// that was deleted.
    pub(super) fn note_roster(&self, presence: &waddle_xmpp_client_ffi::WaddlePresence) {
        let Some((room, nick, occupant)) = occupant_from(presence) else {
            return;
        };
        let own = presence.muc_status_codes.contains(&110);
        {
            let mut state = self.imp().rooms.borrow_mut();
            let roster = state.rosters.entry(room.clone()).or_default();
            match occupant.clone() {
                Some(occupant) => roster.set(occupant),
                None => roster.remove(&nick),
            }
            if own {
                match occupant {
                    Some(occupant) => {
                        state.standing.insert(
                            room.clone(),
                            Standing {
                                affiliation: occupant.affiliation.unwrap_or(Affiliation::None),
                                role: occupant.role,
                            },
                        );
                    }
                    None => {
                        state.standing.remove(&room);
                    }
                }
            }
        }
        if own && presence.muc_destroyed {
            let name = self
                .imp()
                .conversations
                .borrow()
                .iter()
                .find(|conversation| conversation.jid == room)
                .map(|conversation| conversation.name.clone());
            if let Some(name) = name {
                self.forget_conversation(&room);
                self.toast(&format!("{name} was deleted."));
            }
        }
    }

    fn standing_in(&self, room: &str) -> Standing {
        self.imp()
            .rooms
            .borrow()
            .standing
            .get(room)
            .copied()
            .unwrap_or(Standing {
                affiliation: Affiliation::None,
                role: None,
            })
    }

    fn ready_session(&self) -> Option<Session> {
        let imp = self.imp();
        let session = imp.session.borrow().clone();
        match session {
            Some(session) if imp.connected.get() => Some(session),
            _ => {
                self.toast("Connect to the server first.");
                None
            }
        }
    }

    /// Add a conversation to the sidebar in order and open it.
    fn add_and_open(&self, conversation: Conversation) {
        let imp = self.imp();
        let key = conversation.key();
        {
            let mut all = imp.conversations.borrow_mut();
            if all.iter().all(|known| known.key() != key) {
                let at = rooms::placement(&all, conversation.flavor);
                all.insert(at, conversation);
            }
        }
        self.render_conversations();
        self.open_conversation(&key);
    }

    /// Drop a room the account is no longer in.
    pub(super) fn forget_conversation(&self, jid: &str) {
        let imp = self.imp();
        let prefix = format!("{jid}\n");
        imp.conversations
            .borrow_mut()
            .retain(|conversation| conversation.jid != jid);
        imp.timelines
            .borrow_mut()
            .retain(|key, _| key != jid && !key.starts_with(&prefix));
        imp.unread
            .borrow_mut()
            .retain(|key, _| key != jid && !key.starts_with(&prefix));
        let viewing = imp
            .current
            .borrow()
            .as_deref()
            .is_some_and(|current| current == jid || current.starts_with(&prefix));
        if viewing {
            imp.current.replace(None);
            imp.content_stack.set_visible_child_name("empty");
            imp.split_view.set_show_content(false);
            imp.room_title.set_title("Waddle");
            imp.room_page.set_title("Waddle");
            self.refresh_message_tools();
            self.refresh_room_tools();
            self.refresh_composer();
        }
        imp.rooms.borrow_mut().rosters.remove(jid);
        self.render_conversations();
    }

    fn rename_conversation(&self, jid: &str, name: &str, flavor: Option<Flavor>) {
        let imp = self.imp();
        for conversation in imp
            .conversations
            .borrow_mut()
            .iter_mut()
            .filter(|conversation| conversation.jid == jid)
        {
            if conversation.thread.is_none() {
                conversation.name = name.to_string();
            }
            if let Some(flavor) = flavor {
                conversation.flavor = flavor;
            }
        }
        self.render_conversations();
        if let Some(current) = self.current_conversation() {
            if current.jid == jid && current.thread.is_none() {
                let title = match (current.kind, current.flavor) {
                    (Kind::Room, Flavor::Plain | Flavor::Forum) => format!("# {name}"),
                    _ => name.to_string(),
                };
                imp.room_title.set_title(&title);
                imp.room_page.set_title(name);
            }
        }
    }

    // ── New room ────────────────────────────────────────────────────────

    fn show_new_room(&self) {
        let Some(session) = self.ready_session() else {
            return;
        };
        let name = adw::EntryRow::builder().title("Name").build();
        let description = adw::EntryRow::builder()
            .title("Description (optional)")
            .build();
        let members_only = adw::SwitchRow::builder()
            .title("Members only")
            .subtitle("Only people you add can join.")
            .build();
        let listed = adw::SwitchRow::builder()
            .title("List in the room directory")
            .subtitle("Anyone on the server can find it.")
            .active(true)
            .build();
        let forum = adw::SwitchRow::builder()
            .title("Forum")
            .subtitle("Marks the room as a forum, with its own icon.")
            .build();
        let list = boxed_list();
        for row in [
            name.upcast_ref::<gtk::Widget>(),
            description.upcast_ref(),
            members_only.upcast_ref(),
            listed.upcast_ref(),
            forum.upcast_ref(),
        ] {
            list.append(row);
        }
        let problem = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .build();
        problem.add_css_class("error");
        let form = form_box();
        form.append(&list);
        form.append(&problem);
        let create = gtk::Button::with_label("Create");
        create.add_css_class("suggested-action");
        let shell = Rc::new(Shell::new("New Room", 440, &scrolled(&form), &[&create]));

        let window = self.downgrade();
        let weak_shell = Rc::downgrade(&shell);
        let busy = Rc::new(Cell::new(false));
        create.connect_clicked(move |button| {
            let (Some(window), Some(shell)) = (window.upgrade(), weak_shell.upgrade()) else {
                return;
            };
            if busy.get() {
                return;
            }
            let existing: Vec<String> = window
                .imp()
                .conversations
                .borrow()
                .iter()
                .map(|conversation| conversation.jid.clone())
                .collect();
            let address = match rooms::new_room_address(&name.text(), &existing) {
                Ok(address) => address,
                Err(why) => {
                    problem.set_label(why.message());
                    problem.set_visible(true);
                    return;
                }
            };
            problem.set_visible(false);
            let settings = Settings {
                name: name.text().trim().to_string(),
                description: description.text().trim().to_string(),
                members_only: members_only.is_active(),
                public: listed.is_active(),
                forum: forum.is_active(),
            };
            busy.set(true);
            button.set_sensitive(false);
            let (session, busy, problem, button) = (
                session.clone(),
                Rc::clone(&busy),
                problem.clone(),
                button.clone(),
            );
            let generation = window.imp().generation.get();
            glib::spawn_future_local(async move {
                let created = session.create_room(address, settings.clone()).await;
                busy.set(false);
                button.set_sensitive(true);
                if window.imp().generation.get() != generation {
                    return;
                }
                match created {
                    Ok(jid) => {
                        let flavor = if settings.forum {
                            Flavor::Forum
                        } else {
                            Flavor::Plain
                        };
                        let conversation = Conversation {
                            jid: jid.clone(),
                            name: settings.name.clone(),
                            kind: Kind::Room,
                            autojoin: true,
                            flavor,
                            thread: None,
                        };
                        shell.dialog.close();
                        window.add_and_open(conversation);
                        // Other devices find the room through its bookmark.
                        if let Err(error) = session.bookmark_room(&jid, &settings.name).await {
                            tracing::debug!(?error, "could not bookmark the new room");
                        }
                    }
                    Err(error) => {
                        problem.set_label(&error_text("The room could not be created", &error));
                        problem.set_visible(true);
                    }
                }
            });
        });
        Shell::present(&shell, self);
    }

    // ── Details ─────────────────────────────────────────────────────────

    fn show_details(&self) {
        let Some(conversation) = self.current_conversation() else {
            return;
        };
        if conversation.kind != Kind::Room {
            return;
        }
        let Some(session) = self.ready_session() else {
            return;
        };
        match conversation.flavor {
            Flavor::Group => self.show_group_details(conversation, session),
            _ => self.show_room_details(conversation, session),
        }
    }

    fn show_room_details(&self, conversation: Conversation, session: Session) {
        let standing = self.standing_in(&conversation.jid);
        let page = adw::PreferencesPage::new();

        let about = adw::PreferencesGroup::builder().title("Room").build();
        let address = adw::ActionRow::builder()
            .title("Address")
            .subtitle(&conversation.jid)
            .subtitle_selectable(true)
            .use_markup(false)
            .build();
        about.add(&address);
        let name = adw::EntryRow::builder()
            .title("Name")
            .sensitive(false)
            .build();
        let description = adw::EntryRow::builder()
            .title("Description")
            .sensitive(false)
            .build();
        let members_only = adw::SwitchRow::builder()
            .title("Members only")
            .sensitive(false)
            .build();
        let listed = adw::SwitchRow::builder()
            .title("Listed in the room directory")
            .sensitive(false)
            .build();
        let forum = adw::SwitchRow::builder()
            .title("Forum")
            .sensitive(false)
            .build();
        let editable = standing.is_owner();
        if editable {
            for row in [
                name.upcast_ref::<gtk::Widget>(),
                description.upcast_ref(),
                members_only.upcast_ref(),
                listed.upcast_ref(),
                forum.upcast_ref(),
            ] {
                about.add(row);
            }
        } else {
            about.set_description(Some("Only the room's owners can change its settings."));
        }
        page.add(&about);

        let people_group = adw::PreferencesGroup::builder().title("People").build();
        let people = boxed_list();
        people_group.add(&people);
        page.add(&people_group);

        let bots_group = adw::PreferencesGroup::builder()
            .title("Bots")
            .visible(false)
            .build();
        let bots = boxed_list();
        bots_group.add(&bots);
        page.add(&bots_group);

        let danger = adw::PreferencesGroup::new();
        let leave = gtk::Button::with_label("Leave Room…");
        leave.add_css_class("destructive-action");
        leave.set_halign(gtk::Align::Start);
        let delete = gtk::Button::with_label("Delete Room…");
        delete.add_css_class("destructive-action");
        delete.set_halign(gtk::Align::Start);
        delete.set_visible(editable);
        let buttons = gtk::Box::builder().spacing(12).build();
        buttons.append(&leave);
        buttons.append(&delete);
        danger.add(&buttons);
        page.add(&danger);

        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        save.set_visible(editable);
        save.set_sensitive(false);
        let shell = Rc::new(Shell::new(&conversation.name, 460, &page, &[&save]));

        let view = Rc::new(PeopleView {
            window: self.downgrade(),
            session: session.clone(),
            room: conversation.jid.clone(),
            list: people,
            bots_group,
            bots,
            manage: true,
            shell: Rc::downgrade(&shell),
            listed: RefCell::default(),
        });
        view.reload();

        // Settings, for owners.
        let before: Rc<RefCell<Option<Settings>>> = Rc::new(RefCell::new(None));
        if editable {
            let (window, weak_shell, before) =
                (self.downgrade(), Rc::downgrade(&shell), Rc::clone(&before));
            let (session, room) = (session.clone(), conversation.jid.clone());
            let (name, description, members_only, listed, forum, save) = (
                name.clone(),
                description.clone(),
                members_only.clone(),
                listed.clone(),
                forum.clone(),
                save.clone(),
            );
            glib::spawn_future_local(async move {
                let details = session.room_details(&room).await;
                let (Some(_window), Some(shell)) = (window.upgrade(), weak_shell.upgrade()) else {
                    return;
                };
                match details {
                    Ok(details) => {
                        let settings = details.settings;
                        name.set_text(&settings.name);
                        description.set_text(&settings.description);
                        members_only.set_active(settings.members_only);
                        listed.set_active(settings.public);
                        forum.set_active(settings.forum);
                        for row in [
                            name.upcast_ref::<gtk::Widget>(),
                            description.upcast_ref(),
                            members_only.upcast_ref(),
                            listed.upcast_ref(),
                            forum.upcast_ref(),
                        ] {
                            row.set_sensitive(true);
                        }
                        save.set_sensitive(true);
                        before.replace(Some(settings));
                    }
                    Err(error) => {
                        shell.toast(&error_text("The settings could not be loaded", &error))
                    }
                }
            });
        }

        let (window, weak_shell) = (self.downgrade(), Rc::downgrade(&shell));
        let (save_session, room, original) = (session.clone(), conversation.jid.clone(), before);
        let fields = (name, description, members_only, listed, forum);
        save.connect_clicked(move |button| {
            let (Some(window), Some(shell)) = (window.upgrade(), weak_shell.upgrade()) else {
                return;
            };
            let Some(first) = original.borrow().clone() else {
                return;
            };
            let (name, description, members_only, listed, forum) = &fields;
            let wanted = Settings {
                name: name.text().to_string(),
                description: description.text().to_string(),
                members_only: members_only.is_active(),
                public: listed.is_active(),
                forum: forum.is_active(),
            };
            let changes = rooms::changes(&first, &wanted);
            if changes.is_empty() {
                shell.dialog.close();
                return;
            }
            button.set_sensitive(false);
            let (session, room, button) = (save_session.clone(), room.clone(), button.clone());
            let generation = window.imp().generation.get();
            glib::spawn_future_local(async move {
                let result = session.edit_room(&room, &changes).await;
                button.set_sensitive(true);
                if window.imp().generation.get() != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        let flavor =
                            changes.forum.map(
                                |forum| {
                                    if forum {
                                        Flavor::Forum
                                    } else {
                                        Flavor::Plain
                                    }
                                },
                            );
                        let new_name = changes.name.clone();
                        let current = window
                            .imp()
                            .conversations
                            .borrow()
                            .iter()
                            .find(|conversation| conversation.jid == room)
                            .map(|conversation| conversation.name.clone());
                        if let Some(current) = current {
                            window.rename_conversation(
                                &room,
                                new_name.as_deref().unwrap_or(&current),
                                flavor,
                            );
                        }
                        window.toast("Room settings saved.");
                        shell.dialog.close();
                    }
                    Err(error) => shell.toast(&error_text("The settings were not saved", &error)),
                }
            });
        });

        // Leave.
        let (window, weak_shell) = (self.downgrade(), Rc::downgrade(&shell));
        let (leave_session, target) = (session.clone(), conversation.clone());
        leave.connect_clicked(move |button| {
            let (window, weak_shell) = (window.clone(), weak_shell.clone());
            let (session, target) = (leave_session.clone(), target.clone());
            confirm(
                button,
                &format!("Leave {}?", target.name),
                "It disappears from your list on all your devices.",
                "Leave",
                true,
                move || {
                    let (window, weak_shell) = (window.clone(), weak_shell.clone());
                    let (session, target) = (session.clone(), target.clone());
                    glib::spawn_future_local(async move {
                        let result = session.leave_room(&target.jid, &target.name).await;
                        let Some(window) = window.upgrade() else {
                            return;
                        };
                        match result {
                            Ok(()) => {
                                if let Some(shell) = weak_shell.upgrade() {
                                    shell.dialog.close();
                                }
                                window.forget_conversation(&target.jid);
                                window.toast(&format!("You left {}.", target.name));
                            }
                            Err(error) => match weak_shell.upgrade() {
                                Some(shell) => {
                                    shell.toast(&error_text("You could not leave", &error))
                                }
                                None => window.toast(&error_text("You could not leave", &error)),
                            },
                        }
                    });
                },
            );
        });

        // Delete.
        let (window, weak_shell) = (self.downgrade(), Rc::downgrade(&shell));
        let (delete_session, target) = (session, conversation);
        delete.connect_clicked(move |button| {
            let (window, weak_shell) = (window.clone(), weak_shell.clone());
            let (session, target) = (delete_session.clone(), target.clone());
            confirm(
                button,
                &format!("Delete {}?", target.name),
                "The room and its history are deleted for everyone. This cannot be undone.",
                "Delete",
                true,
                move || {
                    let (window, weak_shell) = (window.clone(), weak_shell.clone());
                    let (session, target) = (session.clone(), target.clone());
                    glib::spawn_future_local(async move {
                        let result = session.destroy_room(&target.jid).await;
                        let Some(window) = window.upgrade() else {
                            return;
                        };
                        match result {
                            Ok(()) => {
                                if let Some(shell) = weak_shell.upgrade() {
                                    shell.dialog.close();
                                }
                                window.forget_conversation(&target.jid);
                                window.toast(&format!("{} was deleted.", target.name));
                            }
                            Err(error) => match weak_shell.upgrade() {
                                Some(shell) => {
                                    shell.toast(&error_text("The room was not deleted", &error))
                                }
                                None => {
                                    window.toast(&error_text("The room was not deleted", &error))
                                }
                            },
                        }
                    });
                },
            );
        });

        Shell::present(&shell, self);
    }

    // ── Group DMs ───────────────────────────────────────────────────────

    fn show_new_group(&self) {
        let Some(session) = self.ready_session() else {
            return;
        };
        let own = session.me.bare.to_string();
        let name = adw::EntryRow::builder().title("Name (optional)").build();
        let list = boxed_list();
        list.append(&name);
        let create = gtk::Button::with_label("Create");
        create.add_css_class("suggested-action");
        create.set_sensitive(false);
        let weak_create = create.downgrade();
        let picker = Picker::new(self, Vec::new(), move |chosen| {
            if let Some(create) = weak_create.upgrade() {
                create.set_sensitive(chosen > 0);
            }
        });
        let form = form_box();
        form.append(&list);
        form.append(&heading("Add people"));
        form.append(&picker.root);
        let shell = Rc::new(Shell::new(
            "New Group Chat",
            460,
            &scrolled(&form),
            &[&create],
        ));

        let window = self.downgrade();
        let weak_shell = Rc::downgrade(&shell);
        let busy = Rc::new(Cell::new(false));
        create.connect_clicked(move |button| {
            let (Some(window), Some(shell)) = (window.upgrade(), weak_shell.upgrade()) else {
                return;
            };
            if busy.get() {
                return;
            }
            let members = match rooms::group_members(&own, &picker.jids()) {
                Ok(members) => members,
                Err(problem) => return shell.toast(problem.message()),
            };
            let title = rooms::group_title(&name.text(), &members, &own);
            busy.set(true);
            button.set_sensitive(false);
            let (session, busy, button) = (session.clone(), Rc::clone(&busy), button.clone());
            let generation = window.imp().generation.get();
            glib::spawn_future_local(async move {
                let created = session.create_group_dm(title.clone(), members).await;
                busy.set(false);
                button.set_sensitive(true);
                if window.imp().generation.get() != generation {
                    return;
                }
                match created {
                    Ok(jid) => {
                        shell.dialog.close();
                        window.add_and_open(Conversation {
                            jid,
                            name: title,
                            kind: Kind::Room,
                            autojoin: true,
                            flavor: Flavor::Group,
                            thread: None,
                        });
                    }
                    Err(error) => {
                        shell.toast(&error_text("The group chat could not be created", &error))
                    }
                }
            });
        });
        Shell::present(&shell, self);
    }

    fn show_group_details(&self, conversation: Conversation, session: Session) {
        let page = adw::PreferencesPage::new();
        let about = adw::PreferencesGroup::builder().title("Group Chat").build();
        let name = adw::EntryRow::builder()
            .title("Name")
            .text(&conversation.name)
            .show_apply_button(true)
            .build();
        about.add(&name);
        let add = adw::ActionRow::builder()
            .title("Add People…")
            .activatable(true)
            .build();
        add.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
        about.add(&add);
        page.add(&about);

        let people_group = adw::PreferencesGroup::builder().title("People").build();
        let people = boxed_list();
        people_group.add(&people);
        page.add(&people_group);

        let danger = adw::PreferencesGroup::new();
        let leave = gtk::Button::with_label("Leave Group…");
        leave.add_css_class("destructive-action");
        leave.set_halign(gtk::Align::Start);
        danger.add(&leave);
        page.add(&danger);

        let shell = Rc::new(Shell::new(&conversation.name, 460, &page, &[]));
        let view = Rc::new(PeopleView {
            window: self.downgrade(),
            session: session.clone(),
            room: conversation.jid.clone(),
            list: people,
            bots_group: adw::PreferencesGroup::new(),
            bots: boxed_list(),
            manage: false,
            shell: Rc::downgrade(&shell),
            listed: RefCell::default(),
        });
        view.reload();

        // Rename.
        let (window, weak_shell) = (self.downgrade(), Rc::downgrade(&shell));
        let (rename_session, room) = (session.clone(), conversation.jid.clone());
        name.connect_apply(move |row| {
            let (window, weak_shell) = (window.clone(), weak_shell.clone());
            let (session, room, text) = (
                rename_session.clone(),
                room.clone(),
                row.text().trim().to_string(),
            );
            glib::spawn_future_local(async move {
                let result = session.rename_group_dm(&room, &text).await;
                let Some(window) = window.upgrade() else {
                    return;
                };
                match result {
                    Ok(()) => {
                        let shown = if text.is_empty() {
                            "Group chat".to_string()
                        } else {
                            text
                        };
                        window.rename_conversation(&room, &shown, None);
                        if let Some(shell) = weak_shell.upgrade() {
                            shell.toast("Group renamed.");
                        }
                    }
                    Err(error) => match weak_shell.upgrade() {
                        Some(shell) => {
                            shell.toast(&error_text("The group was not renamed", &error))
                        }
                        None => window.toast(&error_text("The group was not renamed", &error)),
                    },
                }
            });
        });

        // Add people.
        let (window, view_for_add) = (self.downgrade(), Rc::downgrade(&view));
        let room = conversation.jid.clone();
        add.connect_activated(move |_| {
            if let (Some(window), Some(view)) = (window.upgrade(), view_for_add.upgrade()) {
                window.show_add_people(&room, view.member_jids());
            }
        });

        // Leave.
        let (window, weak_shell) = (self.downgrade(), Rc::downgrade(&shell));
        let (leave_session, target) = (session, conversation);
        leave.connect_clicked(move |button| {
            let (window, weak_shell) = (window.clone(), weak_shell.clone());
            let (session, target) = (leave_session.clone(), target.clone());
            confirm(
                button,
                &format!("Leave {}?", target.name),
                "You stop receiving its messages, and someone has to add you again to bring you back.",
                "Leave",
                true,
                move || {
                    let (window, weak_shell) = (window.clone(), weak_shell.clone());
                    let (session, target) = (session.clone(), target.clone());
                    glib::spawn_future_local(async move {
                        let result = session.leave_group_dm(&target.jid).await;
                        let Some(window) = window.upgrade() else {
                            return;
                        };
                        match result {
                            Ok(()) => {
                                if let Some(shell) = weak_shell.upgrade() {
                                    shell.dialog.close();
                                }
                                window.forget_conversation(&target.jid);
                                window.toast(&format!("You left {}.", target.name));
                            }
                            Err(error) => match weak_shell.upgrade() {
                                Some(shell) => {
                                    shell.toast(&error_text("You could not leave", &error))
                                }
                                None => window.toast(&error_text("You could not leave", &error)),
                            },
                        }
                    });
                },
            );
        });

        Shell::present(&shell, self);
    }

    fn show_add_people(&self, room: &str, members: Vec<String>) {
        let Some(session) = self.ready_session() else {
            return;
        };
        let add = gtk::Button::with_label("Add");
        add.add_css_class("suggested-action");
        add.set_sensitive(false);
        let weak_add = add.downgrade();
        let picker = Picker::new(self, members, move |chosen| {
            if let Some(add) = weak_add.upgrade() {
                add.set_sensitive(chosen > 0);
            }
        });
        let earlier = adw::SwitchRow::builder()
            .title("Share earlier messages")
            .subtitle("Works only if you can see all of them.")
            .build();
        let list = boxed_list();
        list.append(&earlier);
        let form = form_box();
        form.append(&picker.root);
        form.append(&list);
        let shell = Rc::new(Shell::new("Add People", 440, &scrolled(&form), &[&add]));

        let window = self.downgrade();
        let weak_shell = Rc::downgrade(&shell);
        let room = room.to_string();
        add.connect_clicked(move |button| {
            let (Some(window), Some(shell)) = (window.upgrade(), weak_shell.upgrade()) else {
                return;
            };
            let people = picker.jids();
            if people.is_empty() {
                return;
            }
            button.set_sensitive(false);
            let (session, room, full) = (session.clone(), room.clone(), earlier.is_active());
            glib::spawn_future_local(async move {
                let count = people.len();
                let sent = session.invite_to_group_dm(&room, people, full).await;
                shell.dialog.close();
                window.toast(&match (sent, count) {
                    (0, _) => "The invitations could not be sent.".to_string(),
                    (1, 1) => "Invitation sent.".to_string(),
                    (sent, count) if sent == count => format!("{sent} invitations sent."),
                    (sent, count) => format!("{sent} of {count} invitations sent."),
                });
            });
        });
        Shell::present(&shell, self);
    }
}

// ── People ─────────────────────────────────────────────────────────────

/// The people and bots lists of a details dialog.
struct PeopleView {
    window: glib::WeakRef<WaddleWindow>,
    session: Session,
    room: String,
    list: gtk::ListBox,
    bots_group: adw::PreferencesGroup,
    bots: gtk::ListBox,
    /// Offer moderation (rooms), or only list (group DMs).
    manage: bool,
    shell: Weak<Shell>,
    /// Bare JIDs on the last affiliation lists.
    listed: RefCell<Vec<String>>,
}

impl PeopleView {
    fn reload(self: &Rc<Self>) {
        self.list.remove_all();
        let loading = gtk::Box::builder()
            .halign(gtk::Align::Center)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        loading.append(&adw::Spinner::new());
        self.list.append(&loading);
        let view = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (members, bots) = tokio::join!(
                view.session.members(&view.room),
                view.session.bots(&view.room)
            );
            if view.shell.upgrade().is_none() {
                return;
            }
            view.render(members, bots);
        });
    }

    /// Bare JIDs of everyone listed or present, so they are not offered
    /// again for adding.
    fn member_jids(&self) -> Vec<String> {
        let Some(window) = self.window.upgrade() else {
            return Vec::new();
        };
        let present: Vec<String> = window
            .imp()
            .rooms
            .borrow()
            .rosters
            .get(&self.room)
            .map(|roster| {
                roster
                    .occupants()
                    .into_iter()
                    .filter_map(|occupant| occupant.jid)
                    .collect()
            })
            .unwrap_or_default();
        let mut jids = present;
        jids.extend(self.listed.borrow().iter().cloned());
        jids
    }

    fn render(self: &Rc<Self>, members: Result<Members, RoomError>, bots: Vec<Bot>) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let occupants = window
            .imp()
            .rooms
            .borrow()
            .rosters
            .get(&self.room)
            .map(Roster::occupants)
            .unwrap_or_default();
        let (listed, hidden) = match &members {
            Ok(members) => (members.listed.clone(), members.bans_hidden),
            Err(_) => (Vec::new(), true),
        };
        self.listed
            .replace(listed.iter().map(|entry| entry.jid.clone()).collect());
        let people = rooms::merge_people(&listed, &occupants, true);
        let standing = window.standing_in(&self.room);
        let own_jid = self.session.me.bare.to_string();
        self.list.remove_all();
        if people.is_empty() {
            self.list.append(&note(match members {
                Err(_) => "The member list is not available to you.",
                Ok(_) => "Nobody is listed.",
            }));
        }
        for person in &people {
            self.list
                .append(&self.person_row(person, standing, &own_jid));
        }
        if hidden && members.is_ok() && self.manage {
            self.list
                .append(&note("The list of banned people is not available to you."));
        }
        self.bots.remove_all();
        self.bots_group.set_visible(!bots.is_empty());
        for bot in bots {
            let row = adw::ActionRow::builder()
                .title(bot.name.as_deref().unwrap_or(&bot.jid))
                .subtitle(&bot.jid)
                .use_markup(false)
                .build();
            row.add_suffix(&badge("Bot"));
            self.bots.append(&row);
        }
    }

    fn person_row(
        self: &Rc<Self>,
        person: &Person,
        standing: Standing,
        own_jid: &str,
    ) -> adw::ActionRow {
        let mut parts = vec![person.affiliation.label().to_string()];
        if let Some(role) = person.role {
            parts.push(role.label().to_string());
        }
        parts.push(if person.here { "Here" } else { "Away" }.to_string());
        if let Some(reason) = person.reason.as_deref().filter(|reason| !reason.is_empty()) {
            parts.push(reason.to_string());
        }
        let row = adw::ActionRow::builder()
            .title(&person.name)
            .subtitle(parts.join(" · "))
            .use_markup(false)
            .build();
        let face = avatar::avatar(32);
        avatar::set_name(&face, &person.name);
        row.add_prefix(&face);
        for hat in &person.hats {
            row.add_suffix(&badge(hat));
        }
        let own_nick = self.session.me.nick.as_str();
        let mine = person.nick.as_deref() == Some(own_nick);
        let actions = if self.manage && !mine {
            rooms::actions_for(standing, person, own_jid)
        } else {
            Vec::new()
        };
        if !actions.is_empty() {
            row.add_suffix(&self.actions_button(person, &actions));
        }
        row
    }

    fn actions_button(self: &Rc<Self>, person: &Person, actions: &[Action]) -> gtk::MenuButton {
        let popover = gtk::Popover::new();
        let menu = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        for action in actions {
            let item = gtk::Button::builder()
                .label(rooms::action_label(*action, person))
                .build();
            item.add_css_class("flat");
            if matches!(action, Action::Ban | Action::Kick) {
                item.add_css_class("destructive-action");
            }
            let (view, person, action) = (Rc::downgrade(self), person.clone(), *action);
            let weak_popover = popover.downgrade();
            item.connect_clicked(move |_| {
                if let Some(popover) = weak_popover.upgrade() {
                    popover.popdown();
                }
                if let Some(view) = view.upgrade() {
                    view.perform(action, person.clone());
                }
            });
            menu.append(&item);
        }
        popover.set_child(Some(&menu));
        let button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .valign(gtk::Align::Center)
            .tooltip_text("Actions")
            .popover(&popover)
            .build();
        button.add_css_class("flat");
        button
    }

    fn perform(self: &Rc<Self>, action: Action, person: Person) {
        match rooms::confirmation(action, &person.name) {
            Some((title, body, verb)) => {
                let Some(shell) = self.shell.upgrade() else {
                    return;
                };
                let view = Rc::downgrade(self);
                confirm(
                    &shell.dialog,
                    &title,
                    &body,
                    verb,
                    matches!(action, Action::Ban | Action::Kick),
                    move || {
                        if let Some(view) = view.upgrade() {
                            view.run(action, person.clone());
                        }
                    },
                );
            }
            None => self.run(action, person),
        }
    }

    fn run(self: &Rc<Self>, action: Action, person: Person) {
        let view = Rc::clone(self);
        glib::spawn_future_local(async move {
            let (verb, result) = match action {
                Action::Kick => (
                    "The person was not kicked",
                    view.session
                        .kick(&view.room, person.nick.as_deref().unwrap_or_default())
                        .await,
                ),
                Action::Ban => (
                    "The person was not banned",
                    view.set(&person, Affiliation::Outcast).await,
                ),
                Action::SetAffiliation(affiliation) => (
                    "The change was not made",
                    view.set(&person, affiliation).await,
                ),
            };
            let Some(shell) = view.shell.upgrade() else {
                return;
            };
            match result {
                Ok(()) => {
                    shell.toast(&match action {
                        Action::Kick => format!("{} was kicked.", person.name),
                        Action::Ban => format!("{} was banned.", person.name),
                        Action::SetAffiliation(_) => format!("{} was updated.", person.name),
                    });
                    view.reload();
                }
                Err(error) => shell.toast(&error_text(verb, &error)),
            }
        });
    }

    async fn set(&self, person: &Person, affiliation: Affiliation) -> Result<(), RoomError> {
        match person.jid.as_deref() {
            Some(jid) => {
                self.session
                    .set_affiliation(&self.room, jid, affiliation)
                    .await
            }
            None => Err(RoomError { condition: None }),
        }
    }
}

// ── Choosing people ────────────────────────────────────────────────────

/// A search box over the user directory, the matches, and the people
/// picked so far.
struct Picker {
    root: gtk::Box,
    window: glib::WeakRef<WaddleWindow>,
    /// Already in the group; never offered.
    excluded: Vec<String>,
    chosen: RefCell<Vec<Mentionable>>,
    found: RefCell<Vec<Mentionable>>,
    results: gtk::ListBox,
    picked: gtk::ListBox,
    status: gtk::Label,
    seq: Cell<u64>,
    on_change: Box<dyn Fn(usize)>,
}

/// Matches shown at once.
const MATCHES: usize = 20;

impl Picker {
    fn new(
        window: &WaddleWindow,
        excluded: Vec<String>,
        on_change: impl Fn(usize) + 'static,
    ) -> Rc<Self> {
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search people by username")
            .build();
        let results = boxed_list();
        let picked = boxed_list();
        let status = note("");
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        root.append(&search);
        root.append(&status);
        root.append(&results);
        root.append(&picked);
        let picker = Rc::new(Self {
            root,
            window: window.downgrade(),
            excluded,
            chosen: RefCell::default(),
            found: RefCell::default(),
            results,
            picked,
            status,
            seq: Cell::new(0),
            on_change: Box::new(on_change),
        });
        picker.rebuild();
        let weak = Rc::downgrade(&picker);
        search.connect_search_changed(move |entry| {
            if let Some(picker) = weak.upgrade() {
                picker.search(&entry.text());
            }
        });
        picker
    }

    fn jids(&self) -> Vec<String> {
        self.chosen
            .borrow()
            .iter()
            .map(|person| person.jid.clone())
            .collect()
    }

    fn search(self: &Rc<Self>, query: &str) {
        let query = query.trim().trim_start_matches('@').to_string();
        let seq = self.seq.get().wrapping_add(1);
        self.seq.set(seq);
        let session = self.window.upgrade().and_then(|window| {
            window
                .imp()
                .session
                .borrow()
                .clone()
                .filter(|_| window.imp().connected.get())
        });
        let Some(session) = session.filter(|_| !query.is_empty()) else {
            self.found.borrow_mut().clear();
            self.status.set_label("");
            self.rebuild();
            return;
        };
        self.status.set_label("Searching…");
        let picker = Rc::clone(self);
        glib::spawn_future_local(async move {
            let found = session.find_people(query).await;
            if picker.seq.get() != seq {
                return;
            }
            match found {
                Ok(people) => {
                    picker.status.set_label(if people.is_empty() {
                        "Nobody found."
                    } else {
                        ""
                    });
                    picker.found.replace(people);
                }
                Err(_) => {
                    picker.status.set_label("The search did not finish.");
                    picker.found.borrow_mut().clear();
                }
            }
            picker.rebuild();
        });
    }

    fn rebuild(self: &Rc<Self>) {
        let own = self
            .window
            .upgrade()
            .and_then(|window| {
                window
                    .imp()
                    .session
                    .borrow()
                    .as_ref()
                    .map(|session| session.me.bare.to_string())
            })
            .unwrap_or_default();
        let mut skip = self.excluded.clone();
        skip.extend(self.jids());
        self.results.remove_all();
        let found = self.found.borrow();
        for person in rooms::addable(&found, |person| person.jid.as_str(), &own, &skip)
            .into_iter()
            .take(MATCHES)
        {
            let row = adw::ActionRow::builder()
                .title(&person.nick)
                .use_markup(false)
                .activatable(true)
                .build();
            if let Some(display) = person.display_name.as_deref() {
                row.set_subtitle(display);
            }
            let face = avatar::avatar(32);
            avatar::set_name(&face, &person.nick);
            row.add_prefix(&face);
            row.add_suffix(&gtk::Image::from_icon_name("list-add-symbolic"));
            let (weak, person) = (Rc::downgrade(self), person.clone());
            row.connect_activated(move |_| {
                if let Some(picker) = weak.upgrade() {
                    picker.chosen.borrow_mut().push(person.clone());
                    picker.rebuild();
                }
            });
            self.results.append(&row);
        }
        self.results
            .set_visible(self.results.first_child().is_some());
        drop(found);

        self.picked.remove_all();
        for person in self.chosen.borrow().iter() {
            let row = adw::ActionRow::builder()
                .title(&person.nick)
                .use_markup(false)
                .build();
            let face = avatar::avatar(32);
            avatar::set_name(&face, &person.nick);
            row.add_prefix(&face);
            let remove = gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text("Remove")
                .build();
            remove.add_css_class("flat");
            let (weak, jid) = (Rc::downgrade(self), person.jid.clone());
            remove.connect_clicked(move |_| {
                if let Some(picker) = weak.upgrade() {
                    picker
                        .chosen
                        .borrow_mut()
                        .retain(|person| person.jid != jid);
                    picker.rebuild();
                }
            });
            row.add_suffix(&remove);
            self.picked.append(&row);
        }
        self.picked.set_visible(self.picked.first_child().is_some());
        (self.on_change)(self.chosen.borrow().len());
    }
}
