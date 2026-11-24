//! Someone else's profile: picture, name and address, how they show now,
//! their XEP-0292 details and what they share (mood, activity, tune).
//! Opened from an avatar or name in the timeline, and from People.

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};

use super::avatar;
use super::WaddleWindow;
use crate::profile::{Details, Shared};

/// A row that stays hidden until it has something to say.
fn detail_row(title: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle_selectable(true)
        .visible(false)
        .build();
    row.add_css_class("property");
    row
}

fn show(row: &adw::ActionRow, text: &str) {
    row.set_subtitle(&glib::markup_escape_text(text));
    row.set_visible(true);
}

impl WaddleWindow {
    /// Open `jid`'s profile. A room participant whose address the room
    /// hides has none to look up: only the name shows.
    pub(super) fn open_person(&self, jid: Option<String>, name: String) {
        let imp = self.imp();
        let page = adw::PreferencesPage::new();

        let head = adw::PreferencesGroup::new();
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .halign(gtk::Align::Center)
            .build();
        let picture = avatar::avatar(96);
        avatar::set_name(&picture, &name);
        let title = gtk::Label::builder()
            .label(&name)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        title.add_css_class("title-1");
        let address = gtk::Label::builder()
            .label(jid.as_deref().unwrap_or("Address not shared by this room"))
            .selectable(true)
            .build();
        address.add_css_class("dim-label");
        let presence_label = gtk::Label::new(None);
        presence_label.add_css_class("caption");
        column.append(&picture);
        column.append(&title);
        column.append(&address);
        column.append(&presence_label);
        head.add(&column);
        page.add(&head);

        let about = adw::PreferencesGroup::builder().title("About").build();
        let pronouns = detail_row("Pronouns");
        let bio = detail_row("About");
        let link = detail_row("Link");
        for row in [&pronouns, &bio, &link] {
            about.add(row);
        }
        about.set_visible(false);
        page.add(&about);

        let now = adw::PreferencesGroup::builder().title("Right Now").build();
        let now_rows = [
            detail_row("Mood"),
            detail_row("Activity"),
            detail_row("Listening to"),
        ];
        for row in &now_rows {
            now.add(row);
        }
        now.set_visible(false);
        page.add(&now);

        let mut online = false;
        if let Some(jid) = &jid {
            self.watch_avatar(&picture, jid);
            let seen = imp.profile.book.borrow().seen(jid);
            online = seen.is_some();
            presence_label.set_label(&match seen {
                Some(seen) => match seen.status {
                    Some(status) => format!("{} · {status}", seen.show.title()),
                    None => seen.show.title().to_string(),
                },
                None => "Offline".to_string(),
            });
            let actions = adw::PreferencesGroup::new();
            let message = adw::ButtonRow::builder().title("Send Message").build();
            message.add_css_class("suggested-action");
            actions.add(&message);
            page.add(&actions);
            let (window, peer) = (self.clone(), jid.clone());
            message.connect_activated(move |row| {
                window.ensure_direct(&peer);
                window.open_conversation(&peer);
                if let Some(dialog) = row.ancestor(adw::Dialog::static_type()) {
                    if let Some(dialog) = dialog.downcast_ref::<adw::Dialog>() {
                        dialog.close();
                    }
                }
            });
        } else {
            presence_label.set_visible(false);
        }

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        let dialog = adw::Dialog::builder()
            .title("Profile")
            .content_width(420)
            .content_height(560)
            .child(&toolbar)
            .build();
        dialog.present(Some(self));

        let (Some(jid), Some(session)) = (jid, imp.session.borrow().clone()) else {
            return;
        };
        let generation = imp.generation.get();
        let (window, lookup, peer) = (self.clone(), session.clone(), jid.clone());
        glib::spawn_future_local(async move {
            let found = lookup.vcard(peer).await;
            if window.imp().generation.get() != generation {
                return;
            }
            let Ok(vcard) = found else {
                return;
            };
            let details = Details::from_wire(vcard);
            if details.is_empty() {
                return;
            }
            title.set_label(&details.display_name(&name));
            if !details.pronouns.trim().is_empty() {
                show(&pronouns, details.pronouns.trim());
            }
            if !details.bio.trim().is_empty() {
                show(&bio, details.bio.trim());
            }
            if !details.link.trim().is_empty() {
                show(&link, details.link.trim());
                if let Some(url) = details.openable_link() {
                    let open = gtk::Button::from_icon_name("external-link-symbolic");
                    open.set_valign(gtk::Align::Center);
                    open.set_tooltip_text(Some("Open Link"));
                    open.add_css_class("flat");
                    let owner = window.clone();
                    open.connect_clicked(move |_| {
                        gtk::UriLauncher::new(url.as_str()).launch(
                            Some(&owner),
                            None::<&gio::Cancellable>,
                            |_| {},
                        );
                    });
                    link.add_suffix(&open);
                }
            }
            about.set_visible(true);
        });
        let window = self.clone();
        glib::spawn_future_local(async move {
            let shared = session.shared_status(jid).await;
            if window.imp().generation.get() != generation {
                return;
            }
            let Ok(shared) = shared else {
                return;
            };
            for (heading, text) in Shared::from_wire(shared).lines(online) {
                let at = match heading {
                    "Mood" => 0,
                    "Activity" => 1,
                    _ => 2,
                };
                show(&now_rows[at], &text);
                now.set_visible(true);
            }
        });
    }
}
