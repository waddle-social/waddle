//! The preferences dialog: appearance, message order, notifications,
//! accounts and about.
//!
//! What is chosen lives in `crate::settings` (a small file in the config
//! directory); sessions stay in the Secret Service, which the accounts list
//! reads through the same `CredentialStore` the window signs in with.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;

use super::WaddleWindow;
use crate::auth::store::{CredentialStore, StoredAccount};
use crate::runtime;
use crate::settings::{self, Appearance, MessageOrder, Settings};

const APPEARANCES: [(Appearance, &str); 3] = [
    (Appearance::System, "Follow System"),
    (Appearance::Light, "Light"),
    (Appearance::Dark, "Dark"),
];

const ORDERS: [(MessageOrder, &str, &str); 2] = [
    (
        MessageOrder::Chat,
        "Chat",
        "Newest at the bottom, composer at the bottom",
    ),
    (
        MessageOrder::Social,
        "Social",
        "Newest first, composer on top, older messages load below",
    ),
];

/// Apply the saved appearance to the whole app.
pub fn apply_appearance() {
    let scheme = match settings::get().appearance {
        Appearance::System => adw::ColorScheme::Default,
        Appearance::Light => adw::ColorScheme::ForceLight,
        Appearance::Dark => adw::ColorScheme::ForceDark,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

impl WaddleWindow {
    /// Open the preferences dialog over this window.
    pub fn show_preferences(&self) {
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Preferences");
        let page = adw::PreferencesPage::new();
        let current = settings::get();

        page.add(&self.appearance_group(&dialog, &current));
        page.add(&self.order_group(&dialog, &current));
        page.add(&self.notifications_group(&dialog, &current));
        page.add(&self.accounts_group(&dialog));
        page.add(&self.about_group());
        dialog.add(&page);
        dialog.present(Some(self));
    }

    fn appearance_group(
        &self,
        dialog: &adw::PreferencesDialog,
        current: &Settings,
    ) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("Appearance").build();
        let names: Vec<&str> = APPEARANCES.iter().map(|(_, name)| *name).collect();
        let row = adw::ComboRow::builder()
            .title("Color Scheme")
            .model(&gtk::StringList::new(&names))
            .selected(
                APPEARANCES
                    .iter()
                    .position(|(value, _)| *value == current.appearance)
                    .map_or(0, |at| at as u32),
            )
            .build();
        let dialog = dialog.downgrade();
        row.connect_selected_notify(move |row| {
            let Some((appearance, _)) = APPEARANCES.get(row.selected() as usize) else {
                return;
            };
            save(&dialog, |settings| settings.appearance = *appearance);
            apply_appearance();
        });
        group.add(&row);
        group
    }

    fn order_group(
        &self,
        dialog: &adw::PreferencesDialog,
        current: &Settings,
    ) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Message Order")
            .description("How a conversation reads.")
            .build();
        let names: Vec<String> = ORDERS
            .iter()
            .map(|(_, name, hint)| format!("{name}: {hint}"))
            .collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let row = adw::ComboRow::builder()
            .title("Order")
            .model(&gtk::StringList::new(&names))
            .selected(
                ORDERS
                    .iter()
                    .position(|(value, ..)| *value == current.message_order)
                    .map_or(0, |at| at as u32),
            )
            .build();
        let dialog = dialog.downgrade();
        let window = self.downgrade();
        row.connect_selected_notify(move |row| {
            let Some((order, ..)) = ORDERS.get(row.selected() as usize) else {
                return;
            };
            if settings::get().message_order == *order {
                return;
            }
            save(&dialog, |settings| settings.message_order = *order);
            if let Some(window) = window.upgrade() {
                window.apply_message_order();
            }
        });
        group.add(&row);
        group
    }

    fn notifications_group(
        &self,
        dialog: &adw::PreferencesDialog,
        current: &Settings,
    ) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Notifications")
            .description("Sound follows your desktop's notification settings.")
            .build();
        let enabled = adw::SwitchRow::builder()
            .title("Desktop Notifications")
            .subtitle("Announce new messages while you are not reading them")
            .active(current.notifications)
            .build();
        let mentions = adw::SwitchRow::builder()
            .title("Mentions Only in Rooms")
            .subtitle(
                "Direct messages always notify. A mode chosen for a room with the bell \
                 overrides this.",
            )
            .active(current.rooms_mentions_only)
            .sensitive(current.notifications)
            .build();

        let weak = dialog.downgrade();
        let window = self.downgrade();
        let mentions_row = mentions.downgrade();
        enabled.connect_active_notify(move |row| {
            let on = row.is_active();
            save(&weak, |settings| settings.notifications = on);
            if let Some(mentions) = mentions_row.upgrade() {
                mentions.set_sensitive(on);
            }
            if let Some(window) = window.upgrade() {
                window.apply_notification_settings();
            }
        });
        let weak = dialog.downgrade();
        let window = self.downgrade();
        mentions.connect_active_notify(move |row| {
            let only = row.is_active();
            save(&weak, |settings| settings.rooms_mentions_only = only);
            if let Some(window) = window.upgrade() {
                window.apply_notification_settings();
            }
        });
        group.add(&enabled);
        group.add(&mentions);
        group
    }

    fn accounts_group(&self, dialog: &adw::PreferencesDialog) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder()
            .title("Accounts")
            .description("One account for each community you are signed in to.")
            .build();
        let add = adw::ActionRow::builder()
            .title("Add Account…")
            .activatable(true)
            .build();
        add.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
        let window = self.clone();
        let weak = dialog.downgrade();
        add.connect_activated(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
            let window = window.clone();
            glib::spawn_future_local(async move { window.add_account().await });
        });
        group.add(&add);

        let rows = Rc::new(RefCell::new(Vec::new()));
        self.fill_accounts(dialog, &group, &rows);
        group
    }

    /// Replace the account rows with what the keyring holds now.
    fn fill_accounts(
        &self,
        dialog: &adw::PreferencesDialog,
        group: &adw::PreferencesGroup,
        rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
    ) {
        let window = self.clone();
        let dialog = dialog.clone();
        let group = group.clone();
        let rows = rows.clone();
        glib::spawn_future_local(async move {
            let stored = runtime::spawn(async {
                let store = CredentialStore::open().await?;
                store.accounts().await
            })
            .await;
            for row in rows.borrow_mut().drain(..) {
                group.remove(&row);
            }
            let active = window.imp().account.borrow().clone();
            let mut shown = Vec::new();
            match stored {
                Ok(Ok(accounts)) => {
                    for account in accounts {
                        let is_active = active.as_ref().is_some_and(|active| {
                            active.server == account.server
                                && active.session.jid == account.session.jid
                        });
                        shown.push(window.account_row(&dialog, &group, &rows, account, is_active));
                    }
                }
                Ok(Err(error)) => shown.push(note_row(&format!("Accounts unavailable: {error}"))),
                Err(_) => shown.push(note_row("Accounts unavailable.")),
            }
            for row in &shown {
                group.add(row);
            }
            *rows.borrow_mut() = shown;
        });
    }

    fn account_row(
        &self,
        dialog: &adw::PreferencesDialog,
        group: &adw::PreferencesGroup,
        rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
        account: StoredAccount,
        is_active: bool,
    ) -> adw::ActionRow {
        let jid = account.session.jid.to_string();
        let name = jid.split('@').next().unwrap_or(&jid).to_string();
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&jid))
            .subtitle(glib::markup_escape_text(&format!(
                "{}{}",
                account.server,
                if is_active { " · Signed in" } else { "" }
            )))
            .build();
        let avatar = adw::Avatar::new(32, Some(&name), true);
        super::avatar::set_name(&avatar, &name);
        row.add_prefix(&avatar);

        if !is_active {
            let switch = gtk::Button::builder()
                .label("Switch")
                .valign(gtk::Align::Center)
                .build();
            let window = self.clone();
            let weak = dialog.downgrade();
            let target = account.clone();
            switch.connect_clicked(move |_| {
                if let Some(dialog) = weak.upgrade() {
                    dialog.close();
                }
                let (window, target) = (window.clone(), target.clone());
                glib::spawn_future_local(async move { window.switch_account(target).await });
            });
            row.add_suffix(&switch);
        }

        let sign_out = gtk::Button::builder()
            .label("Sign Out")
            .valign(gtk::Align::Center)
            .build();
        sign_out.add_css_class("destructive-action");
        let window = self.clone();
        let (dialog, group, rows) = (dialog.clone(), group.clone(), rows.clone());
        sign_out.connect_clicked(move |button| {
            button.set_sensitive(false);
            let (window, dialog, group, rows, account) = (
                window.clone(),
                dialog.clone(),
                group.clone(),
                rows.clone(),
                account.clone(),
            );
            glib::spawn_future_local(async move {
                if is_active {
                    dialog.close();
                    window.sign_out().await;
                    return;
                }
                let removed = runtime::spawn(async move {
                    let store = CredentialStore::open().await?;
                    store.remove(&account).await
                })
                .await;
                if !matches!(removed, Ok(Ok(()))) {
                    dialog.add_toast(adw::Toast::new("Could not sign out of that account."));
                }
                window.fill_accounts(&dialog, &group, &rows);
            });
        });
        row.add_suffix(&sign_out);
        row
    }

    fn about_group(&self) -> adw::PreferencesGroup {
        let group = adw::PreferencesGroup::builder().title("About").build();
        let imp = self.imp();
        let mut facts = vec![("Version", env!("CARGO_PKG_VERSION").to_string())];
        if let Some(account) = imp.account.borrow().as_ref() {
            facts.push(("Server", account.server.to_string()));
            facts.push(("Signed in as", account.session.jid.to_string()));
            facts.push((
                "Connection",
                if imp.connected.get() {
                    "Connected"
                } else {
                    "Not connected"
                }
                .to_string(),
            ));
        }
        for (title, value) in facts {
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(glib::markup_escape_text(&value))
                .subtitle_selectable(true)
                .build();
            group.add(&row);
        }
        let about = adw::ActionRow::builder()
            .title("About Waddle")
            .activatable(true)
            .build();
        about.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let window = self.clone();
        about.connect_activated(move |_| {
            let _ = WidgetExt::activate_action(&window, "app.about", None);
        });
        group.add(&about);
        group
    }
}

fn note_row(message: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(glib::markup_escape_text(message))
        .build()
}

/// Save a change; the dialog says so when it cannot be kept.
fn save(dialog: &glib::WeakRef<adw::PreferencesDialog>, change: impl FnOnce(&mut Settings)) {
    if let Err(error) = settings::update(change) {
        tracing::warn!(%error, "could not save settings");
        if let Some(dialog) = dialog.upgrade() {
            dialog.add_toast(adw::Toast::new(
                "Could not save this setting; it will not survive a restart.",
            ));
        }
    }
}
