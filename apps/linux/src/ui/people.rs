//! The People view: everyone the account knows by address, with name
//! search, opened from the sidebar. Who is listed, and how they are split,
//! is `crate::people`; this shows it and feeds it.

use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;

use super::avatar;
use super::WaddleWindow;
use crate::people::{self, Listing, Person, Sources};
use crate::session::Kind;

/// How long typing must pause before the directory is searched.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// A search shorter than this is not sent to the directory.
const MIN_DIRECTORY_QUERY: usize = 2;
/// Rows shown per section; a longer list is narrowed by searching.
const MAX_ROWS: usize = 150;

/// The open People dialog.
pub struct PeopleView {
    search: gtk::SearchEntry,
    sections: gtk::Box,
    /// Directory results for the latest search: bare JID, username and
    /// display name.
    directory: Vec<(String, String, Option<String>)>,
    /// The latest directory lookup; any other answer is stale.
    serial: u64,
}

impl WaddleWindow {
    pub(super) fn open_people(&self) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        if imp.profile.people.borrow().is_some() {
            return;
        }
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search people")
            .hexpand(true)
            .build();
        let sections = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(12)
            .margin_bottom(18)
            .margin_start(12)
            .margin_end(12)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(
                &adw::Clamp::builder()
                    .maximum_size(560)
                    .child(&sections)
                    .build(),
            )
            .build();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&search));
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroller));
        let dialog = adw::Dialog::builder()
            .title("People")
            .content_width(440)
            .content_height(640)
            .child(&toolbar)
            .build();
        imp.profile.people.replace(Some(PeopleView {
            search: search.clone(),
            sections,
            directory: Vec::new(),
            serial: 0,
        }));

        let window = self.clone();
        search.connect_search_changed(move |entry| window.people_query_changed(&entry.text()));
        let window = self.clone();
        dialog.connect_closed(move |_| {
            window.imp().profile.people.replace(None);
        });
        dialog.present(Some(self));
        search.grab_focus();
        self.refresh_people();

        // The contacts, read fresh each time the view opens.
        let generation = imp.generation.get();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let contacts = session.contacts().await;
            if window.imp().generation.get() != generation {
                return;
            }
            if let Some(contacts) = contacts {
                window.imp().profile.contacts.replace(contacts);
                window.refresh_people();
            }
        });
    }

    /// Rebuild the open view from what is known now.
    pub(super) fn refresh_people(&self) {
        let imp = self.imp();
        let Some(own) = self.own_jid() else {
            return;
        };
        let (listing, sections) = {
            let view = imp.profile.people.borrow();
            let Some(view) = view.as_ref() else {
                return;
            };
            let sources = Sources {
                contacts: imp
                    .profile
                    .contacts
                    .borrow()
                    .iter()
                    .map(|contact| (contact.jid.clone(), contact.name.clone()))
                    .collect(),
                direct: imp
                    .conversations
                    .borrow()
                    .iter()
                    .filter(|conversation| {
                        conversation.kind == Kind::Direct && conversation.thread.is_none()
                    })
                    .map(|conversation| (conversation.jid.clone(), conversation.name.clone()))
                    .collect(),
                occupants: imp.membership.borrow().known_occupants(),
                directory: view.directory.clone(),
            };
            let listing = people::list(
                &sources,
                &imp.profile.book.borrow(),
                &own,
                &view.search.text(),
            );
            (listing, view.sections.clone())
        };
        self.show_listing(&sections, &listing);
    }

    fn show_listing(&self, sections: &gtk::Box, listing: &Listing) {
        while let Some(child) = sections.first_child() {
            sections.remove(&child);
        }
        if listing.is_empty() {
            let empty = adw::StatusPage::builder()
                .icon_name("system-users-symbolic")
                .title("No one found")
                .description("People you chat with, your contacts and search results appear here.")
                .build();
            sections.append(&empty);
            return;
        }
        for (title, people) in [
            ("Here now", &listing.here),
            ("Away and offline", &listing.away),
            ("Directory", &listing.directory),
        ] {
            if people.is_empty() {
                continue;
            }
            let group = adw::PreferencesGroup::builder().title(title).build();
            for person in people.iter().take(MAX_ROWS) {
                group.add(&self.person_row(person));
            }
            sections.append(&group);
        }
    }

    fn person_row(&self, person: &Person) -> adw::ActionRow {
        let subtitle = match &person.seen {
            Some(seen) => match &seen.status {
                Some(status) => format!("{} · {status}", seen.show.title()),
                None => seen.show.title().to_string(),
            },
            None => person.jid.clone(),
        };
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&person.name))
            .subtitle(glib::markup_escape_text(&subtitle))
            .subtitle_lines(1)
            .activatable(true)
            .build();
        let picture = avatar::avatar(36);
        avatar::set_name(&picture, &person.name);
        picture.set_custom_image(self.avatar_texture(&person.jid).as_ref());
        row.add_prefix(&picture);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let (window, jid, name) = (self.clone(), person.jid.clone(), person.name.clone());
        row.connect_activated(move |_| window.open_person(Some(jid.clone()), name.clone()));
        row
    }

    /// The search text changed: filter at once, and look in the directory
    /// after a pause.
    fn people_query_changed(&self, query: &str) {
        let imp = self.imp();
        let query = query.trim().to_string();
        let serial = {
            let mut view = imp.profile.people.borrow_mut();
            let Some(view) = view.as_mut() else {
                return;
            };
            view.serial = view.serial.wrapping_add(1);
            if query.chars().count() < MIN_DIRECTORY_QUERY {
                view.directory.clear();
            }
            view.serial
        };
        self.refresh_people();
        if query.chars().count() < MIN_DIRECTORY_QUERY {
            return;
        }
        let (window, generation) = (self.downgrade(), imp.generation.get());
        glib::timeout_add_local_once(DEBOUNCE, move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let imp = window.imp();
            let latest = imp
                .profile
                .people
                .borrow()
                .as_ref()
                .is_some_and(|view| view.serial == serial);
            let Some(session) = imp.session.borrow().clone() else {
                return;
            };
            if imp.generation.get() != generation || !latest {
                return;
            }
            glib::spawn_future_local(async move {
                let found = session.search_users(query).await;
                if window.imp().generation.get() != generation {
                    return;
                }
                {
                    let mut view = window.imp().profile.people.borrow_mut();
                    let Some(view) = view.as_mut().filter(|view| view.serial == serial) else {
                        return;
                    };
                    // A failed search leaves what is listed as it is.
                    let Some(found) = found else {
                        return;
                    };
                    view.directory = found
                        .into_iter()
                        .map(|person| (person.jid, person.nick, person.display_name))
                        .collect();
                }
                window.refresh_people();
            });
        });
    }
}
