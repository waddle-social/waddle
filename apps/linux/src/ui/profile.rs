//! Your profile: picture, status, the XEP-0292 details others see, and
//! what you share about the moment (mood, activity, tune). Opened from the
//! account row. What the values mean is in `crate::profile` and
//! `crate::presence`; this builds the dialog and performs its saves.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;
use waddle_xmpp_client_ffi::{WaddleActivity, WaddleMood};

use super::avatar;
use super::WaddleWindow;
use crate::presence::PresenceMode;
use crate::profile::{self, Details, Shared};
use crate::session_profile::describe;

/// The rows of the "what you share" groups, so one load can fill them.
#[derive(Clone)]
struct SharedRows {
    mood: adw::ComboRow,
    mood_note: adw::EntryRow,
    activity_group: adw::PreferencesGroup,
    general: adw::ComboRow,
    specific: adw::ComboRow,
    activity_note: adw::EntryRow,
    tune_title: adw::EntryRow,
    tune_artist: adw::EntryRow,
    tune_album: adw::EntryRow,
    tune_link: adw::EntryRow,
}

fn titles(first: &str, rest: impl IntoIterator<Item = String>) -> gtk::StringList {
    let list = gtk::StringList::new(&[first]);
    for title in rest {
        list.append(&title);
    }
    list
}

fn text_or_none(row: &adw::EntryRow) -> Option<String> {
    let text = row.text();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn specifics_model(general_index: u32) -> gtk::StringList {
    let specifics = usize::try_from(general_index)
        .ok()
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| profile::ACTIVITIES.get(index))
        .map(|kind| kind.specifics)
        .unwrap_or(&[]);
    titles(
        "Any",
        specifics
            .iter()
            .map(|specific| profile::specific_title(specific)),
    )
}

fn button_row(title: &str, suggested: bool) -> adw::ButtonRow {
    let row = adw::ButtonRow::builder().title(title).build();
    if suggested {
        row.add_css_class("suggested-action");
    }
    row
}

impl WaddleWindow {
    /// Tell the user in the profile dialog when it is open, else in the
    /// window.
    pub(super) fn profile_toast(&self, message: &str) {
        let overlay = self
            .imp()
            .profile
            .toasts
            .borrow()
            .as_ref()
            .and_then(glib::WeakRef::upgrade);
        match overlay.filter(|overlay| overlay.is_mapped()) {
            Some(overlay) => overlay.add_toast(adw::Toast::new(message)),
            None => self.toast(message),
        }
    }

    pub(super) fn open_profile(&self) {
        let imp = self.imp();
        let (Some(session), Some(own)) = (imp.session.borrow().clone(), self.own_jid()) else {
            return;
        };
        let page = adw::PreferencesPage::new();
        page.add(&self.picture_group(&own));
        page.add(&self.status_group());
        let (details, load_details) = self.details_group();
        page.add(&details);
        let rows = self.shared_groups(&page);

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&page));
        imp.profile.toasts.replace(Some(toasts.downgrade()));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&toasts));
        let dialog = adw::Dialog::builder()
            .title("Your Profile")
            .content_width(480)
            .content_height(720)
            .child(&toolbar)
            .build();
        dialog.present(Some(self));

        // What is already published, to edit.
        let generation = imp.generation.get();
        let window = self.clone();
        let loading = session.clone();
        glib::spawn_future_local(async move {
            let loaded = loading.own_vcard().await;
            if window.imp().generation.get() != generation {
                return;
            }
            load_details(
                loaded
                    .map(Details::from_wire)
                    .map_err(|error| describe(&error)),
            );
        });
        let window = self.clone();
        glib::spawn_future_local(async move {
            let shared = session.shared_status(own).await;
            if window.imp().generation.get() != generation {
                return;
            }
            if let Ok(shared) = shared {
                fill_shared(&rows, &Shared::from_wire(shared));
            }
        });
    }

    fn picture_group(&self, own: &str) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::new();
        let name = own.split('@').next().unwrap_or(own);
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(name))
            .subtitle(glib::markup_escape_text(own))
            .build();
        let picture = avatar::avatar(48);
        avatar::set_name(&picture, name);
        self.watch_avatar(&picture, own);
        row.add_prefix(&picture);
        let choose = gtk::Button::with_label("Choose…");
        choose.set_valign(gtk::Align::Center);
        let window = self.clone();
        choose.connect_clicked(move |_| window.choose_picture());
        let remove = gtk::Button::with_label("Remove");
        remove.set_valign(gtk::Align::Center);
        let window = self.clone();
        remove.connect_clicked(move |_| window.remove_picture());
        row.add_suffix(&choose);
        row.add_suffix(&remove);
        group.add(&row);
        group
    }

    fn status_group(&self) -> adw::PreferencesGroup {
        let presence = self.imp().profile.presence.borrow();
        let group = adw::PreferencesGroup::builder()
            .title("Status")
            .description(
                "Automatic goes Away after 10 minutes without input in Waddle and Extended \
                 Away after 30. A status you pick applies on all your devices.",
            )
            .build();
        let picker = adw::ComboRow::builder()
            .title("Status")
            .model(&gtk::StringList::new(
                &PresenceMode::PICKER.map(|(title, _)| title),
            ))
            .selected(presence.mode().picker_index())
            .build();
        let window = self.clone();
        picker.connect_selected_notify(move |row| {
            if let Some(mode) = PresenceMode::from_picker_index(row.selected()) {
                window.pick_presence(mode);
            }
        });
        let message = adw::EntryRow::builder()
            .title("Status message")
            .show_apply_button(true)
            .text(presence.status_text().unwrap_or_default())
            .build();
        let window = self.clone();
        message.connect_apply(move |row| window.set_status_message(&row.text()));
        group.add(&picker);
        group.add(&message);
        group
    }

    /// The vCard rows, and what fills them once the current vCard is read.
    fn details_group(
        &self,
    ) -> (
        adw::PreferencesGroup,
        impl FnOnce(Result<Details, String>) + 'static,
    ) {
        let group = adw::PreferencesGroup::builder()
            .title("About You")
            .description("Loading…")
            .sensitive(false)
            .build();
        let entry = |title: &str| adw::EntryRow::builder().title(title).build();
        let (name, nickname, pronouns, bio, link) = (
            entry("Name"),
            entry("Nickname"),
            entry("Pronouns"),
            entry("About"),
            entry("Link"),
        );
        for row in [&name, &nickname, &pronouns, &bio, &link] {
            group.add(row);
        }
        let save = button_row("Save Profile", true);
        group.add(&save);
        // The picture address is written by whoever set the picture and
        // must survive a save of the text.
        let photo = Rc::new(RefCell::new(None::<String>));

        let window = self.clone();
        let (rows, kept) = (
            [
                name.clone(),
                nickname.clone(),
                pronouns.clone(),
                bio.clone(),
                link.clone(),
            ],
            photo.clone(),
        );
        save.connect_activated(move |_| {
            let [name, nickname, pronouns, bio, link] = &rows;
            let details = Details {
                full_name: name.text().to_string(),
                nickname: nickname.text().to_string(),
                pronouns: pronouns.text().to_string(),
                bio: bio.text().to_string(),
                link: link.text().to_string(),
                photo_uri: kept.borrow().clone(),
            };
            let Some(session) = window.imp().session.borrow().clone() else {
                return;
            };
            let window = window.clone();
            let generation = window.imp().generation.get();
            glib::spawn_future_local(async move {
                let saved = session.publish_vcard(details.to_wire()).await;
                if window.imp().generation.get() != generation {
                    return;
                }
                window.profile_toast(&match saved {
                    Ok(()) => "Profile saved.".to_string(),
                    Err(error) => format!("Could not save your profile. {}", describe(&error)),
                });
            });
        });

        let target = group.clone();
        let fill = move |loaded: Result<Details, String>| match loaded {
            Ok(details) => {
                name.set_text(&details.full_name);
                nickname.set_text(&details.nickname);
                pronouns.set_text(&details.pronouns);
                bio.set_text(&details.bio);
                link.set_text(&details.link);
                photo.replace(details.photo_uri);
                target.set_description(Some("Shown to others in your profile."));
                target.set_sensitive(true);
            }
            // Saving replaces the whole vCard, so it never opens empty on
            // a lookup that failed.
            Err(message) => target.set_description(Some(&format!(
                "Could not load your profile, so it cannot be edited now. {message}"
            ))),
        };
        (group, fill)
    }

    /// Mood, activity and tune, each with its own share and clear.
    fn shared_groups(&self, page: &adw::PreferencesPage) -> SharedRows {
        let entry = |title: &str| adw::EntryRow::builder().title(title).build();

        let mood_group = adw::PreferencesGroup::builder()
            .title("Mood")
            .description("Shown to people who can see your profile.")
            .build();
        let mood = adw::ComboRow::builder()
            .title("Mood")
            .model(&titles(
                "None",
                profile::MOODS.iter().map(|mood| profile::title_of(mood)),
            ))
            .build();
        let mood_note = entry("Note (optional)");
        let share_mood = button_row("Share Mood", true);
        let clear_mood = button_row("Clear Mood", false);
        for row in [
            mood.upcast_ref::<gtk::Widget>(),
            mood_note.upcast_ref(),
            share_mood.upcast_ref(),
            clear_mood.upcast_ref(),
        ] {
            mood_group.add(row);
        }

        let activity_group = adw::PreferencesGroup::builder().title("Activity").build();
        let general = adw::ComboRow::builder()
            .title("Activity")
            .model(&titles(
                "None",
                profile::ACTIVITIES
                    .iter()
                    .map(|kind| kind.title.to_string()),
            ))
            .build();
        let specific = adw::ComboRow::builder()
            .title("Specifically")
            .model(&specifics_model(0))
            .sensitive(false)
            .build();
        let specific_row = specific.clone();
        general.connect_selected_notify(move |row| {
            specific_row.set_model(Some(&specifics_model(row.selected())));
            specific_row.set_sensitive(row.selected() > 0);
        });
        let activity_note = entry("Note (optional)");
        let share_activity = button_row("Share Activity", true);
        let clear_activity = button_row("Clear Activity", false);
        for row in [
            general.upcast_ref::<gtk::Widget>(),
            specific.upcast_ref(),
            activity_note.upcast_ref(),
            share_activity.upcast_ref(),
            clear_activity.upcast_ref(),
        ] {
            activity_group.add(row);
        }

        let tune_group = adw::PreferencesGroup::builder()
            .title("Listening To")
            .description("Typed in; Waddle does not read your music player.")
            .build();
        let (tune_title, tune_artist, tune_album, tune_link) = (
            entry("Title"),
            entry("Artist"),
            entry("Album"),
            entry("Link"),
        );
        let share_tune = button_row("Share Tune", true);
        let clear_tune = button_row("Clear Tune", false);
        for row in [
            tune_title.upcast_ref::<gtk::Widget>(),
            tune_artist.upcast_ref(),
            tune_album.upcast_ref(),
            tune_link.upcast_ref(),
            share_tune.upcast_ref(),
            clear_tune.upcast_ref(),
        ] {
            tune_group.add(row);
        }
        page.add(&mood_group);
        page.add(&activity_group);
        page.add(&tune_group);

        let rows = SharedRows {
            mood,
            mood_note,
            activity_group,
            general,
            specific,
            activity_note,
            tune_title,
            tune_artist,
            tune_album,
            tune_link,
        };

        let (window, shared) = (self.clone(), rows.clone());
        share_mood.connect_activated(move |_| {
            let index = shared.mood.selected();
            let Some(kind) = usize::try_from(index)
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| profile::MOODS.get(index))
            else {
                return window.profile_toast("Pick a mood first.");
            };
            let mood = WaddleMood {
                kind: (*kind).to_string(),
                text: text_or_none(&shared.mood_note),
            };
            window.save_shared("Mood", move |session| async move {
                session.set_mood(Some(mood)).await
            });
        });
        let (window, shared) = (self.clone(), rows.clone());
        clear_mood.connect_activated(move |_| {
            shared.mood.set_selected(0);
            shared.mood_note.set_text("");
            window.save_shared(
                "Mood",
                |session| async move { session.set_mood(None).await },
            );
        });

        let (window, shared) = (self.clone(), rows.clone());
        share_activity.connect_activated(move |_| {
            let Some(kind) = usize::try_from(shared.general.selected())
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| profile::ACTIVITIES.get(index))
            else {
                return window.profile_toast("Pick an activity first.");
            };
            let specific = usize::try_from(shared.specific.selected())
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| kind.specifics.get(index))
                .map(|specific| (*specific).to_string());
            let activity = WaddleActivity {
                general: kind.general.to_string(),
                specific,
                text: text_or_none(&shared.activity_note),
            };
            window.save_activity(Some(activity));
        });
        let (window, shared) = (self.clone(), rows.clone());
        clear_activity.connect_activated(move |_| {
            shared.general.set_selected(0);
            shared.activity_note.set_text("");
            window.save_activity(None);
        });

        let (window, shared) = (self.clone(), rows.clone());
        share_tune.connect_activated(move |_| {
            let link = shared.tune_link.text();
            if !profile::blank_or_address(&link) {
                return window.profile_toast("The link must be a full address.");
            }
            let tune = profile::tune_from_fields(
                &shared.tune_title.text(),
                &shared.tune_artist.text(),
                &shared.tune_album.text(),
                &link,
            );
            match tune {
                Some(tune) => window.save_shared("Tune", move |session| async move {
                    session.set_tune(Some(tune)).await
                }),
                None => window.profile_toast("Type a title, artist or album first."),
            }
        });
        let (window, shared) = (self.clone(), rows.clone());
        clear_tune.connect_activated(move |_| {
            for row in [
                &shared.tune_title,
                &shared.tune_artist,
                &shared.tune_album,
                &shared.tune_link,
            ] {
                row.set_text("");
            }
            window.save_shared(
                "Tune",
                |session| async move { session.set_tune(None).await },
            );
        });
        rows
    }

    /// Publish or retract one shared item and say how it went.
    fn save_shared<F, Fut>(&self, what: &'static str, call: F)
    where
        F: FnOnce(crate::session::Session) -> Fut + 'static,
        Fut:
            std::future::Future<Output = Result<(), waddle_xmpp_client_ffi::WaddleError>> + 'static,
    {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let saved = call(session).await;
            if window.imp().generation.get() != generation {
                return;
            }
            window.profile_toast(&match saved {
                Ok(()) => format!("{what} updated."),
                Err(error) => format!(
                    "Could not update your {}. {}",
                    what.to_lowercase(),
                    describe(&error)
                ),
            });
        });
    }

    /// The activity node is account-wide, and another device may be using
    /// it to show a call: look before replacing or clearing it.
    fn save_activity(&self, activity: Option<WaddleActivity>) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let current = session.own_activity().await;
            if window.imp().generation.get() != generation {
                return;
            }
            match current {
                Ok(Some(current)) if profile::shows_a_call(&current) => {
                    return window.profile_toast(
                        "A call is showing as your activity, so it was left alone.",
                    );
                }
                Err(error) => {
                    return window.profile_toast(&format!(
                        "Could not check your current activity. {}",
                        describe(&error)
                    ));
                }
                _ => {}
            }
            let saved = session.set_activity(activity).await;
            if window.imp().generation.get() != generation {
                return;
            }
            window.profile_toast(&match saved {
                Ok(()) => "Activity updated.".to_string(),
                Err(error) => format!("Could not update your activity. {}", describe(&error)),
            });
        });
    }
}

/// Show what is published in the share rows.
fn fill_shared(rows: &SharedRows, shared: &Shared) {
    if let Some(mood) = &shared.mood {
        if let Some(index) = profile::MOODS.iter().position(|known| *known == mood.kind) {
            rows.mood
                .set_selected(u32::try_from(index + 1).unwrap_or(0));
        }
        rows.mood_note
            .set_text(mood.text.as_deref().unwrap_or_default());
    }
    if let Some(activity) = &shared.activity {
        if profile::shows_a_call(activity) {
            // It belongs to the call: shown, not editable from here.
            rows.activity_group
                .set_description(Some("Your activity shows a call in progress."));
            rows.activity_group.set_sensitive(false);
        } else if let Some(index) = profile::ACTIVITIES
            .iter()
            .position(|kind| kind.general == activity.general)
        {
            rows.general
                .set_selected(u32::try_from(index + 1).unwrap_or(0));
            let specifics = profile::ACTIVITIES[index].specifics;
            if let Some(at) = activity
                .specific
                .as_deref()
                .and_then(|specific| specifics.iter().position(|known| *known == specific))
            {
                rows.specific
                    .set_selected(u32::try_from(at + 1).unwrap_or(0));
            }
            rows.activity_note
                .set_text(activity.text.as_deref().unwrap_or_default());
        } else {
            rows.activity_group.set_description(Some(&format!(
                "Another app set your activity to {}.",
                profile::describe_activity(activity)
            )));
        }
    }
    if let Some(tune) = &shared.tune {
        rows.tune_title
            .set_text(tune.title.as_deref().unwrap_or_default());
        rows.tune_artist
            .set_text(tune.artist.as_deref().unwrap_or_default());
        rows.tune_album
            .set_text(tune.source.as_deref().unwrap_or_default());
        rows.tune_link
            .set_text(tune.uri.as_deref().unwrap_or_default());
    }
}
