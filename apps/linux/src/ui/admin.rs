//! The Admin destination for community owners: spaces, their channels and
//! members, channel affiliations and occupants, and the user directory.
//!
//! What to ask, validate and confirm is decided in `admin.rs`; this draws
//! it. The sidebar row only appears once the server accepts the owner
//! probe, and every destructive step asks first.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};

use adw::prelude::{
    ActionRowExt, AdwDialogExt, AlertDialogExt, AlertDialogExtManual, BoxExt, ButtonExt, Cast,
    ComboRowExt, EditableExt, NavigationPageExt, ObjectExt, PreferencesGroupExt,
    PreferencesPageExt, WidgetExt,
};
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;
use jid::BareJid;
use waddle_xmpp_client_ffi::{
    WaddleAdminChannelAffiliationEntry, WaddleAdminChannelListEntry,
    WaddleAdminChannelOccupantEntry, WaddleAdminSpaceEntry, WaddleAdminSpaceMemberEntry,
    WaddleAdminUserEntry, WaddleMucAffiliation, WaddleSpaceRole,
};

use super::WaddleWindow;
use crate::admin::{self, ChannelForm, Confirmation, PageOf, Paged, SpaceForm};
use crate::session::Session;

type Fut<T> = Pin<Box<dyn Future<Output = T>>>;
/// Reload the list a row belongs to.
type Reload = Rc<dyn Fn()>;
/// Read one page after a cursor.
type Fetch<T> = Box<dyn Fn(Option<String>) -> Fut<Result<PageOf<T>, String>>>;
/// Draw one row.
type DrawRow<T> = Box<dyn Fn(&WaddleWindow, &T, &Reload) -> gtk::Widget>;

fn boxed<T>(future: impl Future<Output = T> + 'static) -> Fut<T> {
    Box::pin(future)
}

/// The window's admin state.
#[derive(Default)]
pub struct AdminState {
    /// The server accepted the owner probe.
    owner: bool,
    open: bool,
    row: Option<adw::ActionRow>,
    nav: Option<adw::NavigationView>,
    /// The lists on screen, kept until the console closes. Their widgets'
    /// handlers only hold them weakly.
    keep: Vec<Rc<dyn Any>>,
}

impl WaddleWindow {
    /// The sidebar's Admin row, hidden until the account is an owner.
    pub(super) fn setup_admin(&self) {
        let imp = self.imp();
        let row = adw::ActionRow::builder()
            .title("Admin")
            .title_lines(1)
            .activatable(true)
            .build();
        row.add_css_class("conversation");
        let icon = gtk::Image::from_icon_name("preferences-system-symbolic");
        icon.set_valign(gtk::Align::Center);
        row.add_prefix(&icon);
        imp.admin_list.append(&row);
        imp.admin.borrow_mut().row = Some(row);
    }

    #[must_use]
    pub(super) fn admin_open(&self) -> bool {
        self.imp().admin.borrow().open
    }

    /// Ask the server whether this account owns the community, on every
    /// connect. The server re-checks each command regardless.
    pub(super) fn probe_admin(&self, generation: u64) {
        let Some(session) = self.imp().session.borrow().clone() else {
            return;
        };
        let window = self.clone();
        glib::spawn_future_local(async move {
            let owner = session.admin_is_owner().await;
            if window.imp().generation.get() == generation {
                window.set_admin_owner(owner);
            }
        });
    }

    fn set_admin_owner(&self, owner: bool) {
        let imp = self.imp();
        imp.admin.borrow_mut().owner = owner;
        imp.admin_list.set_visible(owner);
        if !owner && self.admin_open() {
            self.leave_admin();
            imp.content_stack.set_visible_child_name("empty");
        }
    }

    /// Forget the account's admin state, on sign-out.
    pub(super) fn reset_admin(&self) {
        self.set_admin_owner(false);
        self.leave_admin();
    }

    pub(super) fn show_admin(&self) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        if !imp.admin.borrow().owner {
            return;
        }
        self.leave_inbox();
        self.leave_calendar();
        imp.admin.borrow_mut().open = true;
        // Nothing is being read while the console is on screen.
        imp.current.replace(None);
        self.render_conversations();
        if let Some(row) = imp.admin.borrow().row.as_ref() {
            imp.admin_list.select_row(Some(row));
        }
        imp.room_title.set_title("Admin");
        imp.room_page.set_title("Admin");
        imp.thread_back.set_visible(false);
        imp.load_earlier.set_visible(false);
        self.build_admin(&session);
        imp.content_stack.set_visible_child_name("admin");
        if let Some(composer) = imp.composer.parent() {
            composer.set_visible(false);
        }
        self.cancel_compose();
        self.refresh_composer();
        self.refresh_notify_button();
        self.refresh_message_tools();
        self.refresh_thread_context();
        imp.split_view.set_show_content(true);
    }

    /// A conversation or the inbox is opening: the console gives way.
    pub(super) fn leave_admin(&self) {
        let imp = self.imp();
        let was_open = std::mem::take(&mut imp.admin.borrow_mut().open);
        self.clear_admin_pages();
        if !was_open {
            return;
        }
        imp.admin_list.unselect_all();
        if let Some(composer) = imp.composer.parent() {
            composer.set_visible(true);
        }
    }

    fn clear_admin_pages(&self) {
        let imp = self.imp();
        let (nav, keep) = {
            let mut state = imp.admin.borrow_mut();
            (state.nav.take(), std::mem::take(&mut state.keep))
        };
        if let Some(nav) = nav {
            imp.admin_page.remove(&nav);
        }
        drop(keep);
    }

    fn admin_keep(&self, list: Rc<dyn Any>) {
        self.imp().admin.borrow_mut().keep.push(list);
    }

    fn admin_session(&self) -> Option<Session> {
        self.imp().session.borrow().clone()
    }

    fn admin_nav(&self) -> Option<adw::NavigationView> {
        self.imp().admin.borrow().nav.clone()
    }

    fn build_admin(&self, session: &Session) {
        self.clear_admin_pages();
        let nav = adw::NavigationView::new();
        nav.set_vexpand(true);
        self.imp().admin.borrow_mut().nav = Some(nav.clone());
        nav.add(&self.admin_root(session));
        self.imp().admin_page.append(&nav);
    }

    // ── Pages ────────────────────────────────────────────────────────

    fn admin_root(&self, session: &Session) -> adw::NavigationPage {
        let stack = adw::ViewStack::new();
        stack.set_vexpand(true);
        stack.add_titled(
            &scrolled(&self.spaces_tab(session)),
            Some("spaces"),
            "Spaces",
        );
        stack.add_titled(&scrolled(&self.users_tab(session)), Some("users"), "Users");
        let switcher = adw::InlineViewSwitcher::new();
        switcher.set_stack(Some(&stack));
        switcher.set_halign(gtk::Align::Center);
        switcher.set_margin_top(12);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.append(&switcher);
        content.append(&stack);
        adw::NavigationPage::builder()
            .title("Admin")
            .child(&content)
            .build()
    }

    fn push_admin_page(&self, title: &str, body: &gtk::Box) {
        let Some(nav) = self.admin_nav() else {
            return;
        };
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&scrolled(body)));
        nav.push(
            &adw::NavigationPage::builder()
                .title(title)
                .child(&toolbar)
                .build(),
        );
    }

    fn spaces_tab(&self, session: &Session) -> gtk::Box {
        let fetch_session = session.clone();
        let list = PagedList::new(
            self,
            "No spaces yet. Create the first one.",
            move |after| {
                let session = fetch_session.clone();
                boxed(async move { session.admin_spaces(after).await })
            },
            |window, space, reload| window.space_row(space, reload),
        );
        let add = gtk::Button::with_label("New Space");
        add.add_css_class("suggested-action");
        let (window, reload) = (self.downgrade(), list.reloader());
        add.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.space_dialog(None, reload.clone());
            }
        });
        let tab = gtk::Box::new(gtk::Orientation::Vertical, 12);
        tab.append(&heading("Spaces", Some(&add)));
        tab.append(&list.root);
        list.reload();
        self.admin_keep(list);
        tab
    }

    fn users_tab(&self, session: &Session) -> gtk::Box {
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Filter users")
            .hexpand(true)
            .build();
        let (fetch_session, fetch_search) = (session.clone(), search.clone());
        let list = PagedList::new(
            self,
            "No users found.",
            move |after| {
                let session = fetch_session.clone();
                let prefix = admin::search_prefix(&fetch_search.text());
                boxed(async move { session.admin_users(prefix, after).await })
            },
            |_, user, _| user_row(user),
        );
        let weak = Rc::downgrade(&list);
        search.connect_search_changed(move |_| {
            if let Some(list) = weak.upgrade() {
                list.reload();
            }
        });
        let tab = gtk::Box::new(gtk::Orientation::Vertical, 12);
        tab.append(&search);
        tab.append(&list.root);
        list.reload();
        self.admin_keep(list);
        tab
    }

    fn space_row(&self, space: &WaddleAdminSpaceEntry, reload: &Reload) -> gtk::Widget {
        let row = adw::ActionRow::builder()
            .title(&space.name)
            .subtitle(admin::space_summary(space))
            .subtitle_lines(3)
            .use_markup(false)
            .activatable(true)
            .build();
        let edit = icon_button("document-edit-symbolic", "Edit Space");
        let delete = icon_button("user-trash-symbolic", "Delete Space");
        row.add_suffix(&edit);
        row.add_suffix(&delete);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

        let (window, entry, after) = (self.downgrade(), space.clone(), reload.clone());
        edit.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.space_dialog(Some(entry.clone()), after.clone());
            }
        });
        let (window, entry, after) = (self.downgrade(), space.clone(), reload.clone());
        delete.connect_clicked(move |_| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let (confirmed, entry, after) = (window.clone(), entry.clone(), after.clone());
            window.admin_confirm(
                admin::delete_space_prompt(&entry),
                false,
                move |_| {
                    let Some(session) = confirmed.admin_session() else {
                        return;
                    };
                    let (target, after) = (entry.clone(), after.clone());
                    confirmed.admin_task(
                        async move { session.admin_delete_space(&target).await },
                        "Space deleted.",
                        move |_| after(),
                    );
                },
                || {},
            );
        });
        let (window, entry) = (self.downgrade(), space.clone());
        row.connect_activated(move |_| {
            if let Some(window) = window.upgrade() {
                window.push_space(&entry);
            }
        });
        row.upcast()
    }

    /// One space: its channels and its members.
    fn push_space(&self, space: &WaddleAdminSpaceEntry) {
        let Some(session) = self.admin_session() else {
            return;
        };
        let (fetch_session, fetch_space) = (session.clone(), space.clone());
        let channels = PagedList::new(
            self,
            "This space has no channels.",
            move |after| {
                let (session, space) = (fetch_session.clone(), fetch_space.clone());
                boxed(async move { session.admin_channels(&space, after).await })
            },
            {
                let space = space.clone();
                move |window, channel, reload| window.channel_row(&space, channel, reload)
            },
        );
        let (fetch_session, fetch_space) = (session.clone(), space.clone());
        let me = session.me.bare.clone();
        let members = PagedList::new(
            self,
            "This space has no members.",
            move |after| {
                let (session, space) = (fetch_session.clone(), fetch_space.clone());
                boxed(async move { session.admin_space_members(&space, after).await })
            },
            {
                let (space, session) = (space.clone(), session.clone());
                move |window, member, reload| {
                    window.member_row(&session, &space, &me, member, reload)
                }
            },
        );

        let add = gtk::Button::with_label("New Channel");
        let (window, entry, reload) = (self.downgrade(), space.clone(), channels.reloader());
        add.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.channel_dialog(&entry, None, reload.clone());
            }
        });
        let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
        body.append(&heading("Channels", Some(&add)));
        body.append(&channels.root);
        body.append(&heading("Members", None));
        body.append(&members.root);
        channels.reload();
        members.reload();
        self.admin_keep(channels);
        self.admin_keep(members);
        self.push_admin_page(&space.name, &body);
    }

    fn channel_row(
        &self,
        space: &WaddleAdminSpaceEntry,
        channel: &WaddleAdminChannelListEntry,
        reload: &Reload,
    ) -> gtk::Widget {
        let row = adw::ActionRow::builder()
            .title(format!("# {}", channel.name))
            .subtitle(admin::channel_summary(channel))
            .use_markup(false)
            .activatable(true)
            .build();
        let edit = icon_button("document-edit-symbolic", "Edit Channel");
        let delete = icon_button("user-trash-symbolic", "Delete Channel");
        row.add_suffix(&edit);
        row.add_suffix(&delete);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));

        let (window, owner, entry, after) = (
            self.downgrade(),
            space.clone(),
            channel.clone(),
            reload.clone(),
        );
        edit.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.channel_dialog(&owner, Some(entry.clone()), after.clone());
            }
        });
        let (window, entry, after) = (self.downgrade(), channel.clone(), reload.clone());
        delete.connect_clicked(move |_| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let (confirmed, entry, after) = (window.clone(), entry.clone(), after.clone());
            window.admin_confirm(
                admin::delete_channel_prompt(&entry),
                false,
                move |_| {
                    let Some(session) = confirmed.admin_session() else {
                        return;
                    };
                    let (jid, after) = (entry.channel_jid.clone(), after.clone());
                    confirmed.admin_task(
                        async move { session.admin_delete_channel(jid).await },
                        "Channel deleted.",
                        move |_| after(),
                    );
                },
                || {},
            );
        });
        let (window, entry) = (self.downgrade(), channel.clone());
        row.connect_activated(move |_| {
            if let Some(window) = window.upgrade() {
                window.push_channel(&entry);
            }
        });
        row.upcast()
    }

    fn member_row(
        &self,
        session: &Session,
        space: &WaddleAdminSpaceEntry,
        me: &BareJid,
        member: &WaddleAdminSpaceMemberEntry,
        reload: &Reload,
    ) -> gtk::Widget {
        let row = adw::ActionRow::builder()
            .title(&member.jid)
            .use_markup(false)
            .build();
        let picker = gtk::DropDown::from_strings(&admin::role_names());
        picker.set_valign(gtk::Align::Center);
        picker.set_selected(admin::space_role_index(member.role));
        row.add_suffix(&picker);

        let current = Rc::new(Cell::new(member.role));
        let quiet = Rc::new(Cell::new(false));
        let (window, session, space) = (self.downgrade(), session.clone(), space.clone());
        let (jid, myself, reload) = (
            member.jid.clone(),
            admin::is_me(&member.jid, me),
            reload.clone(),
        );
        picker.connect_selected_notify(move |picker| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let Some(role) = admin::space_role_at(picker.selected()) else {
                return;
            };
            if quiet.get() || role == current.get() {
                return;
            }
            let revert = {
                let (picker, current, quiet) = (picker.downgrade(), current.clone(), quiet.clone());
                move || {
                    if let Some(picker) = picker.upgrade() {
                        quiet.set(true);
                        picker.set_selected(admin::space_role_index(current.get()));
                        quiet.set(false);
                    }
                }
            };
            let apply = {
                let (window, session, space, jid) =
                    (window.clone(), session.clone(), space.clone(), jid.clone());
                let (current, reload, revert) = (current.clone(), reload.clone(), revert.clone());
                move || {
                    let (session, space, jid) = (session.clone(), space.clone(), jid.clone());
                    let (current, reload, revert) =
                        (current.clone(), reload.clone(), revert.clone());
                    window.admin_task(
                        async move { session.admin_set_space_role(&space, jid, role).await },
                        "Role updated.",
                        move |ok| {
                            if !ok {
                                revert();
                                return;
                            }
                            current.set(role);
                            if role == WaddleSpaceRole::None {
                                reload();
                            }
                        },
                    );
                }
            };
            match admin::confirm_space_role(&jid, current.get(), role, myself) {
                Some(prompt) => {
                    let apply = apply.clone();
                    window.admin_confirm(prompt, false, move |_| apply(), revert);
                }
                None => apply(),
            }
        });
        row.upcast()
    }

    /// One channel: who has standing in it and who is in it now.
    fn push_channel(&self, channel: &WaddleAdminChannelListEntry) {
        let Some(session) = self.admin_session() else {
            return;
        };
        let me = session.me.bare.clone();
        let (fetch_session, jid) = (session.clone(), channel.channel_jid.clone());
        let affiliations = PagedList::new(
            self,
            "Nobody has standing in this channel.",
            move |after| {
                let (session, jid) = (fetch_session.clone(), jid.clone());
                boxed(async move { session.admin_affiliations(jid, after).await })
            },
            {
                let (jid, session) = (channel.channel_jid.clone(), session.clone());
                move |window, entry, reload| {
                    window.affiliation_row(&session, &jid, &me, entry, reload)
                }
            },
        );
        let (fetch_session, jid) = (session.clone(), channel.channel_jid.clone());
        let occupants = PagedList::new(
            self,
            "Nobody is in this channel right now.",
            move |after| {
                let (session, jid) = (fetch_session.clone(), jid.clone());
                boxed(async move { session.admin_occupants(jid, after).await })
            },
            {
                let jid = channel.channel_jid.clone();
                move |window, entry, reload| window.occupant_row(&jid, entry, reload)
            },
        );

        let add = gtk::Button::with_label("Add");
        add.set_tooltip_text(Some("Set someone's standing in this channel"));
        let (window, jid, reload) = (
            self.downgrade(),
            channel.channel_jid.clone(),
            affiliations.reloader(),
        );
        add.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.affiliation_dialog(&jid, reload.clone());
            }
        });
        let refresh = icon_button("view-refresh-symbolic", "Refresh");
        let reload = occupants.reloader();
        refresh.connect_clicked(move |_| reload());
        let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
        body.append(&heading("Affiliations", Some(&add)));
        body.append(&affiliations.root);
        body.append(&heading("In the Room Now", Some(&refresh)));
        body.append(&occupants.root);
        affiliations.reload();
        occupants.reload();
        self.admin_keep(affiliations);
        self.admin_keep(occupants);
        self.push_admin_page(&format!("# {}", channel.name), &body);
    }

    fn affiliation_row(
        &self,
        session: &Session,
        channel_jid: &str,
        me: &BareJid,
        entry: &WaddleAdminChannelAffiliationEntry,
        reload: &Reload,
    ) -> gtk::Widget {
        let row = adw::ActionRow::builder()
            .title(&entry.jid)
            .use_markup(false)
            .build();
        if let Some(reason) = entry.reason.as_deref().filter(|r| !r.trim().is_empty()) {
            row.set_subtitle(reason);
        }
        let picker = gtk::DropDown::from_strings(&admin::affiliation_names());
        picker.set_valign(gtk::Align::Center);
        picker.set_selected(admin::affiliation_index(entry.affiliation));
        row.add_suffix(&picker);

        let current = Rc::new(Cell::new(entry.affiliation));
        let quiet = Rc::new(Cell::new(false));
        let (window, session, channel_jid) =
            (self.downgrade(), session.clone(), channel_jid.to_string());
        let (jid, myself, reload) = (
            entry.jid.clone(),
            admin::is_me(&entry.jid, me),
            reload.clone(),
        );
        picker.connect_selected_notify(move |picker| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let Some(affiliation) = admin::affiliation_at(picker.selected()) else {
                return;
            };
            if quiet.get() || affiliation == current.get() {
                return;
            }
            let revert = {
                let (picker, current, quiet) = (picker.downgrade(), current.clone(), quiet.clone());
                move || {
                    if let Some(picker) = picker.upgrade() {
                        quiet.set(true);
                        picker.set_selected(admin::affiliation_index(current.get()));
                        quiet.set(false);
                    }
                }
            };
            let apply = {
                let (window, session, channel_jid, jid) = (
                    window.clone(),
                    session.clone(),
                    channel_jid.clone(),
                    jid.clone(),
                );
                let (current, reload, revert) = (current.clone(), reload.clone(), revert.clone());
                move |reason: Option<String>| {
                    let (session, channel_jid, jid) =
                        (session.clone(), channel_jid.clone(), jid.clone());
                    let (current, reload, revert) =
                        (current.clone(), reload.clone(), revert.clone());
                    window.admin_task(
                        async move {
                            session
                                .admin_set_affiliation(channel_jid, jid, affiliation, reason)
                                .await
                        },
                        "Affiliation updated.",
                        move |ok| {
                            if !ok {
                                revert();
                                return;
                            }
                            current.set(affiliation);
                            if affiliation == WaddleMucAffiliation::None {
                                reload();
                            }
                        },
                    );
                }
            };
            match admin::confirm_affiliation(&jid, current.get(), affiliation, myself) {
                Some(prompt) => window.admin_confirm(prompt, true, apply, revert),
                None => apply(None),
            }
        });
        row.upcast()
    }

    fn occupant_row(
        &self,
        channel_jid: &str,
        occupant: &WaddleAdminChannelOccupantEntry,
        reload: &Reload,
    ) -> gtk::Widget {
        let row = adw::ActionRow::builder()
            .title(&occupant.nick)
            .subtitle(admin::occupant_summary(occupant))
            .use_markup(false)
            .build();
        let kick = icon_button("system-log-out-symbolic", "Remove from Room");
        row.add_suffix(&kick);
        let (window, channel_jid, entry, reload) = (
            self.downgrade(),
            channel_jid.to_string(),
            occupant.clone(),
            reload.clone(),
        );
        kick.connect_clicked(move |_| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let (confirmed, channel_jid, entry, reload) = (
                window.clone(),
                channel_jid.clone(),
                entry.clone(),
                reload.clone(),
            );
            window.admin_confirm(
                admin::kick_prompt(&entry),
                true,
                move |reason| {
                    let Some(session) = confirmed.admin_session() else {
                        return;
                    };
                    let target = admin::kick_target(&entry.real_jid);
                    let (channel_jid, reload) = (channel_jid.clone(), reload.clone());
                    confirmed.admin_task(
                        async move { session.admin_kick(channel_jid, target, reason).await },
                        "Removed from the room.",
                        move |_| reload(),
                    );
                },
                || {},
            );
        });
        row.upcast()
    }

    // ── Confirmation and background work ─────────────────────────────

    /// Ask before something destructive. `on_cancel` runs for anything but
    /// the confirming response, so a picker can put its old value back.
    fn admin_confirm(
        &self,
        prompt: Confirmation,
        with_reason: bool,
        on_confirm: impl Fn(Option<String>) + 'static,
        on_cancel: impl Fn() + 'static,
    ) {
        let dialog = adw::AlertDialog::new(Some(&prompt.heading), Some(&prompt.body));
        dialog.add_responses(&[("cancel", "Cancel"), ("confirm", prompt.action)]);
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let reason = gtk::Entry::builder()
            .placeholder_text("Reason (optional)")
            .activates_default(false)
            .build();
        if with_reason {
            dialog.set_extra_child(Some(&reason));
        }
        dialog.connect_response(None, move |_, response| {
            if response == "confirm" {
                let text = admin::search_prefix(&reason.text());
                on_confirm(with_reason.then_some(text).flatten());
            } else {
                on_cancel();
            }
        });
        dialog.present(Some(self));
    }

    /// Run a command, toast how it went, then `after(succeeded)`.
    fn admin_task(
        &self,
        work: impl Future<Output = Result<(), String>> + 'static,
        done: &str,
        after: impl Fn(bool) + 'static,
    ) {
        let (window, generation, done) =
            (self.clone(), self.imp().generation.get(), done.to_string());
        glib::spawn_future_local(async move {
            let result = work.await;
            if window.imp().generation.get() != generation {
                return;
            }
            match &result {
                Ok(()) => window.toast(&done),
                Err(message) => window.toast(message),
            }
            after(result.is_ok());
        });
    }

    // ── Forms ────────────────────────────────────────────────────────

    fn space_dialog(&self, original: Option<WaddleAdminSpaceEntry>, done: Reload) {
        let Some(session) = self.admin_session() else {
            return;
        };
        let form = original
            .as_ref()
            .map(SpaceForm::from_entry)
            .unwrap_or_default();
        let name = entry_row("Name", &form.name);
        let description = entry_row("Description", &form.description);
        let icon = entry_row("Icon URL", &form.icon_url);
        let group = adw::PreferencesGroup::new();
        for row in [&name, &description, &icon] {
            group.add(row);
        }
        let title = if original.is_some() {
            "Edit Space"
        } else {
            "New Space"
        };
        self.admin_form(
            title,
            "Save",
            &group,
            move || {
                let form = SpaceForm {
                    name: name.text().to_string(),
                    description: description.text().to_string(),
                    icon_url: icon.text().to_string(),
                };
                let session = session.clone();
                match &original {
                    None => {
                        let args = form.create_args().map_err(str::to_string)?;
                        Ok(Prepared::run(async move {
                            session.admin_create_space(args).await?;
                            Ok("Space created.".to_string())
                        }))
                    }
                    Some(space) => {
                        let args = form
                            .update_args(space)
                            .map_err(str::to_string)?
                            .ok_or_else(|| "Nothing has changed.".to_string())?;
                        Ok(Prepared::run(async move {
                            session.admin_update_space(args).await?;
                            Ok("Space updated.".to_string())
                        }))
                    }
                }
            },
            done,
        );
    }

    fn channel_dialog(
        &self,
        space: &WaddleAdminSpaceEntry,
        original: Option<WaddleAdminChannelListEntry>,
        done: Reload,
    ) {
        let Some(session) = self.admin_session() else {
            return;
        };
        let form = original
            .as_ref()
            .map(ChannelForm::from_entry)
            .unwrap_or_default();
        let name = entry_row("Name", &form.name);
        let topic = entry_row("Topic", &form.topic);
        let kinds: Vec<&str> = admin::CHANNEL_TYPES
            .iter()
            .map(|(_, label)| *label)
            .collect();
        let kind = adw::ComboRow::builder()
            .title("Type")
            .model(&gtk::StringList::new(&kinds))
            .selected(admin::channel_type_index(&form.channel_type))
            .build();
        let public = adw::SwitchRow::builder()
            .title("Public")
            .subtitle("Anyone in the space can find it")
            .active(form.is_public)
            .build();
        let members_only = adw::SwitchRow::builder()
            .title("Members Only")
            .subtitle("Only members can join")
            .active(form.members_only)
            .build();
        let group = adw::PreferencesGroup::new();
        group.add(&name);
        group.add(&topic);
        group.add(&kind);
        group.add(&public);
        group.add(&members_only);
        let title = if original.is_some() {
            "Edit Channel"
        } else {
            "New Channel"
        };
        let space = space.clone();
        self.admin_form(
            title,
            "Save",
            &group,
            move || {
                let form = ChannelForm {
                    name: name.text().to_string(),
                    topic: topic.text().to_string(),
                    channel_type: admin::channel_type_wire(kind.selected()).to_string(),
                    is_public: public.is_active(),
                    members_only: members_only.is_active(),
                };
                let session = session.clone();
                match &original {
                    None => {
                        let args = form.create_args(&space).map_err(str::to_string)?;
                        Ok(Prepared::run(async move {
                            session.admin_create_channel(args).await?;
                            Ok("Channel created.".to_string())
                        }))
                    }
                    Some(channel) => {
                        let args = form
                            .update_args(channel)
                            .map_err(str::to_string)?
                            .ok_or_else(|| "Nothing has changed.".to_string())?;
                        Ok(Prepared::run(async move {
                            session.admin_update_channel(args).await?;
                            Ok("Channel updated.".to_string())
                        }))
                    }
                }
            },
            done,
        );
    }

    /// Give someone a standing in a channel by address.
    fn affiliation_dialog(&self, channel_jid: &str, done: Reload) {
        let Some(session) = self.admin_session() else {
            return;
        };
        let who = entry_row("Address", "");
        who.set_tooltip_text(Some("name@example.com"));
        let tier = adw::ComboRow::builder()
            .title("Standing")
            .model(&gtk::StringList::new(&admin::affiliation_names()))
            .selected(admin::affiliation_index(WaddleMucAffiliation::Member))
            .build();
        let reason = entry_row("Reason (optional)", "");
        let group = adw::PreferencesGroup::new();
        group.add(&who);
        group.add(&tier);
        group.add(&reason);
        let channel_jid = channel_jid.to_string();
        self.admin_form(
            "Set Affiliation",
            "Apply",
            &group,
            move || {
                let member = admin::member_jid(&who.text()).map_err(str::to_string)?;
                let affiliation = admin::affiliation_at(tier.selected())
                    .ok_or_else(|| "Choose a standing.".to_string())?;
                let reason = admin::search_prefix(&reason.text());
                let (session, channel_jid) = (session.clone(), channel_jid.clone());
                let confirm = admin::confirm_affiliation(
                    &member,
                    WaddleMucAffiliation::Member,
                    affiliation,
                    false,
                );
                let work = Rc::new(move || {
                    let (session, channel_jid, member, reason) = (
                        session.clone(),
                        channel_jid.clone(),
                        member.clone(),
                        reason.clone(),
                    );
                    boxed(async move {
                        session
                            .admin_set_affiliation(channel_jid, member, affiliation, reason)
                            .await?;
                        Ok("Affiliation updated.".to_string())
                    })
                });
                Ok(Prepared { confirm, work })
            },
            done,
        );
    }

    /// A form in a dialog with Cancel and a save button. `prepare` reads
    /// the fields, returning what to run (and what to confirm first) or a
    /// message to show in the form.
    fn admin_form(
        &self,
        title: &str,
        save_label: &str,
        group: &adw::PreferencesGroup,
        prepare: impl Fn() -> Result<Prepared, String> + 'static,
        done: Reload,
    ) {
        let message = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .build();
        message.add_css_class("error");
        let notes = adw::PreferencesGroup::new();
        notes.add(&message);
        let page = adw::PreferencesPage::new();
        page.add(group);
        page.add(&notes);

        let cancel = gtk::Button::with_label("Cancel");
        let save = gtk::Button::with_label(save_label);
        save.add_css_class("suggested-action");
        let header = adw::HeaderBar::new();
        header.set_show_start_title_buttons(false);
        header.set_show_end_title_buttons(false);
        header.pack_start(&cancel);
        header.pack_end(&save);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page));
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width(440)
            .child(&toolbar)
            .build();
        dialog.set_default_widget(Some(&save));

        let weak = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
        });
        let (window, weak) = (self.downgrade(), dialog.downgrade());
        save.connect_clicked(move |button| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let prepared = match prepare() {
                Ok(prepared) => prepared,
                Err(text) => {
                    message.set_label(&text);
                    message.set_visible(true);
                    return;
                }
            };
            message.set_visible(false);
            let run = {
                let (window, weak, button, message, done) = (
                    window.clone(),
                    weak.clone(),
                    button.clone(),
                    message.clone(),
                    done.clone(),
                );
                let work = prepared.work.clone();
                move || {
                    button.set_sensitive(false);
                    let generation = window.imp().generation.get();
                    let (window, weak, button, message, done) = (
                        window.clone(),
                        weak.clone(),
                        button.clone(),
                        message.clone(),
                        done.clone(),
                    );
                    let pending = work();
                    glib::spawn_future_local(async move {
                        let result = pending.await;
                        if window.imp().generation.get() != generation {
                            return;
                        }
                        match result {
                            Ok(text) => {
                                if let Some(dialog) = weak.upgrade() {
                                    dialog.close();
                                }
                                window.toast(&text);
                                done();
                            }
                            Err(text) => {
                                message.set_label(&text);
                                message.set_visible(true);
                                button.set_sensitive(true);
                            }
                        }
                    });
                }
            };
            match prepared.confirm {
                Some(prompt) => window.admin_confirm(prompt, false, move |_| run(), || {}),
                None => run(),
            }
        });
        dialog.present(Some(self));
    }
}

/// What a form's save button runs, and what to ask first.
struct Prepared {
    confirm: Option<Confirmation>,
    work: Rc<dyn Fn() -> Fut<Result<String, String>>>,
}

impl Prepared {
    /// A one-shot piece of work with nothing to confirm.
    fn run(work: impl Future<Output = Result<String, String>> + 'static) -> Self {
        let cell = Rc::new(RefCell::new(Some(work)));
        Self {
            confirm: None,
            work: Rc::new(move || match cell.borrow_mut().take() {
                Some(work) => boxed(work),
                None => boxed(async { Err("That was already sent.".to_string()) }),
            }),
        }
    }
}

// ── A list read page by page ─────────────────────────────────────────

/// A boxed list of rows with a footer for loading, failure, emptiness and
/// "Load More".
struct PagedList<T> {
    root: gtk::Box,
    list: gtk::ListBox,
    footer: gtk::Box,
    state: RefCell<Paged<T>>,
    fetch: Fetch<T>,
    row: DrawRow<T>,
    empty: &'static str,
    window: glib::WeakRef<WaddleWindow>,
    generation: u64,
    me: Weak<Self>,
}

impl<T: 'static> PagedList<T> {
    fn new(
        window: &WaddleWindow,
        empty: &'static str,
        fetch: impl Fn(Option<String>) -> Fut<Result<PageOf<T>, String>> + 'static,
        row: impl Fn(&WaddleWindow, &T, &Reload) -> gtk::Widget + 'static,
    ) -> Rc<Self> {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .visible(false)
            .build();
        list.add_css_class("boxed-list");
        let footer = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.append(&list);
        root.append(&footer);
        Rc::new_cyclic(|me| Self {
            root,
            list,
            footer,
            state: RefCell::new(Paged::default()),
            fetch: Box::new(fetch),
            row: Box::new(row),
            empty,
            window: window.downgrade(),
            generation: window.imp().generation.get(),
            me: me.clone(),
        })
    }

    /// A callback for rows and buttons that reloads from the first page.
    fn reloader(&self) -> Reload {
        let me = self.me.clone();
        Rc::new(move || {
            if let Some(list) = me.upgrade() {
                list.reload();
            }
        })
    }

    fn reload(self: &Rc<Self>) {
        self.load(true);
    }

    fn load(self: &Rc<Self>, restart: bool) {
        let Some(ticket) = self.state.borrow_mut().begin(restart) else {
            return;
        };
        self.draw();
        let pending = (self.fetch)(ticket.after.clone());
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let result = pending.await;
            let current = this
                .window
                .upgrade()
                .is_some_and(|window| window.imp().generation.get() == this.generation);
            if current && this.state.borrow_mut().finish(&ticket, result) {
                this.draw();
            }
        });
    }

    fn draw(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let reload = self.reloader();
        let state = self.state.borrow();
        self.list.remove_all();
        for item in &state.items {
            self.list.append(&(self.row)(&window, item, &reload));
        }
        self.list.set_visible(!state.items.is_empty());

        while let Some(child) = self.footer.first_child() {
            self.footer.remove(&child);
        }
        if state.is_loading() {
            let spinner = adw::Spinner::builder()
                .width_request(24)
                .height_request(24)
                .halign(gtk::Align::Center)
                .build();
            self.footer.append(&spinner);
            return;
        }
        if let Some(message) = state.error() {
            let note = gtk::Label::builder()
                .label(message)
                .wrap(true)
                .xalign(0.0)
                .build();
            note.add_css_class("error");
            let retry = gtk::Button::with_label("Retry");
            retry.set_halign(gtk::Align::Start);
            let me = self.me.clone();
            retry.connect_clicked(move |_| {
                if let Some(list) = me.upgrade() {
                    list.load(false);
                }
            });
            self.footer.append(&note);
            self.footer.append(&retry);
        } else if state.is_loaded() && state.items.is_empty() {
            let note = gtk::Label::builder()
                .label(self.empty)
                .wrap(true)
                .xalign(0.0)
                .build();
            note.add_css_class("dim-label");
            self.footer.append(&note);
        }
        if state.has_more() {
            let more = gtk::Button::with_label("Load More");
            more.set_halign(gtk::Align::Center);
            let me = self.me.clone();
            more.connect_clicked(move |_| {
                if let Some(list) = me.upgrade() {
                    list.load(false);
                }
            });
            self.footer.append(&more);
        }
    }
}

// ── Small widgets ────────────────────────────────────────────────────

fn user_row(user: &WaddleAdminUserEntry) -> gtk::Widget {
    let title = admin::user_title(user);
    let row = adw::ActionRow::builder()
        .title(&title)
        .use_markup(false)
        .build();
    if title != user.jid {
        row.set_subtitle(&user.jid);
    }
    if user.has_owner_hat {
        let badge = gtk::Label::new(Some("Owner"));
        badge.set_valign(gtk::Align::Center);
        badge.add_css_class("dim-label");
        row.add_suffix(&badge);
    }
    row.upcast()
}

fn entry_row(title: &str, text: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).build();
    row.set_text(text);
    row
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.set_valign(gtk::Align::Center);
    button.add_css_class("flat");
    button
}

fn heading(title: &str, action: Option<&gtk::Button>) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .hexpand(true)
        .build();
    label.add_css_class("title-3");
    bar.append(&label);
    if let Some(action) = action {
        bar.append(action);
    }
    bar
}

fn scrolled(body: &impl glib::object::IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_margin_top(12);
    body.set_margin_bottom(16);
    let clamp = adw::Clamp::builder().maximum_size(720).child(body).build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::gio::{self, prelude::ApplicationExt};

    #[test]
    #[ignore = "requires a GTK display; run with xvfb-run and --ignored --test-threads=1"]
    fn the_sidebar_row_follows_the_owner_probe() {
        adw::init().expect("GTK display");
        let app = adw::Application::builder()
            .application_id("social.waddle.AdminTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>)
            .expect("register application");
        let window = WaddleWindow::new(&app);
        assert!(!window.imp().admin_list.get_visible());
        window.set_admin_owner(true);
        assert!(window.imp().admin_list.get_visible());
        // Without a session there is nothing to administer.
        window.show_admin();
        assert!(!window.admin_open());
        window.set_admin_owner(false);
        assert!(!window.imp().admin_list.get_visible());
    }
}
