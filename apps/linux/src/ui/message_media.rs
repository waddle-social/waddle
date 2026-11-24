//! What a message row shows beyond its text and files: link preview cards
//! and the pack behind a sticker (see `media_index.rs`).
//!
//! Each bind owns its widgets; late image loads cannot paint a recycled
//! row because the loading task holds only a weak reference to its
//! picture.

use adw::prelude::{AdwDialogExt, BoxExt, ButtonExt, CastNone, ObjectExt, WidgetExt};
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use url::Url;
use waddle_client_session::TimelineItem;

use super::media;
use super::WaddleWindow;
use crate::link_cards::LinkCard;
use crate::media_index::{self, MessageMedia};
use crate::runtime;
use crate::stickers::{Mark, Pack};

/// The message's extras, when it has any. They are keyed by the open
/// conversation, which is the one whose rows are bound.
pub(super) fn of(window: &WaddleWindow, item: &TimelineItem) -> Option<MessageMedia> {
    let conversation = window.current_conversation()?;
    media_index::lookup(&conversation.jid, item.reference_id.as_deref()?)
}

/// Show the cards under a message: one per preview, the page's title,
/// description, site and image. A bind that finds the cards it already
/// shows keeps them, so scrolling does not fetch their images again.
pub(super) fn bind_cards(container: &gtk::Box, cards: &[LinkCard], retracted: bool) {
    let cards = if retracted { &[][..] } else { cards };
    let mut child = container.first_child();
    let unchanged = cards.iter().all(|card| {
        let matches = child
            .as_ref()
            .is_some_and(|child| child.widget_name() == card.url.as_str());
        child = child.as_ref().and_then(|child| child.next_sibling());
        matches
    }) && child.is_none();
    container.set_visible(!cards.is_empty());
    if unchanged {
        return;
    }
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    for card in cards {
        container.append(&card_widget(card));
    }
}

fn card_widget(card: &LinkCard) -> gtk::Button {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    let site = label(&card.site(), &["caption", "dim-label"], 1);
    body.append(&site);
    if let Some(title) = &card.title {
        body.append(&label(title, &["heading"], 2));
    }
    if let Some(description) = &card.description {
        body.append(&label(description, &["caption"], 3));
    }
    if let Some(image) = &card.image {
        let picture = gtk::Picture::builder()
            .halign(gtk::Align::Start)
            .can_shrink(true)
            .content_fit(gtk::ContentFit::Cover)
            .alternative_text("Preview image")
            .overflow(gtk::Overflow::Hidden)
            .margin_top(4)
            .visible(false)
            .build();
        picture.add_css_class("media");
        body.append(&picture);
        load_picture(&picture, image.clone());
    }
    let button = gtk::Button::builder()
        .child(&body)
        .halign(gtk::Align::Start)
        .tooltip_text(card.url.as_str())
        .build();
    button.set_widget_name(card.url.as_str());
    button.add_css_class("link-card");
    let url = card.url.clone();
    button.connect_clicked(move |button| open(button, &url));
    button
}

fn label(text: &str, classes: &[&str], lines: i32) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .lines(lines)
        .max_width_chars(48)
        .build();
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// Hand the page to the user's browser. Only `http` and `https` get here
/// (`link_cards::web_url`).
fn open(widget: &impl glib::object::IsA<gtk::Widget>, url: &Url) {
    let parent = widget.root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(url.as_str()).launch(parent.as_ref(), gio::Cancellable::NONE, |result| {
        if let Err(error) = result {
            tracing::warn!(%error, "could not open a link");
        }
    });
}

/// Fetch an image into `picture`, which stays hidden if it cannot be
/// shown. The download stops when the picture is dropped.
fn load_picture(picture: &gtk::Picture, url: Url) {
    let weak = picture.downgrade();
    glib::spawn_future_local(async move {
        let download = runtime::spawn(media::download(url));
        let cancel = download.abort_handle();
        let Some(picture) = weak.upgrade() else {
            download.abort();
            return;
        };
        picture.add_weak_ref_notify_local(move || cancel.abort());
        let result = download.await;
        let Some(picture) = weak.upgrade() else {
            return;
        };
        if let Ok(Ok(bytes)) = result {
            if let Ok(animation) = media::decode(&bytes) {
                media::play(&picture, &animation);
                picture.set_visible(true);
            }
        }
    });
}

/// Make a sticker open the pack it is from, when that pack can be
/// opened: on the sender's own sticker node only.
pub(super) fn link_pack(
    widget: &gtk::Widget,
    window: &WaddleWindow,
    item: &TimelineItem,
    extras: &MessageMedia,
) {
    let Some(mark) = extras.pack.clone() else {
        return;
    };
    let Some(owner) = mark.owner(item.author_id.as_deref()).map(str::to_string) else {
        return;
    };
    widget.set_cursor_from_name(Some("pointer"));
    widget.set_tooltip_text(Some("View sticker pack"));
    let (window, click) = (window.downgrade(), gtk::GestureClick::new());
    click.connect_released(move |_, _, _, _| {
        if let Some(window) = window.upgrade() {
            window.view_pack(&owner, &mark);
        }
    });
    widget.add_controller(click);
}

impl WaddleWindow {
    /// Show the pack a sticker is from, with a button that copies it to
    /// the account's own stickers.
    fn view_pack(&self, owner: &str, mark: &Mark) {
        let Some(session) = self.imp().session.borrow().clone() else {
            return;
        };
        let own = owner == session.me.bare.as_str();
        let dialog = adw::Dialog::builder()
            .title("Sticker Pack")
            .content_width(380)
            .content_height(420)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let status = gtk::Label::builder()
            .label("Loading…")
            .vexpand(true)
            .valign(gtk::Align::Center)
            .build();
        status.add_css_class("dim-label");
        content.append(&status);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&content));
        dialog.set_child(Some(&view));
        dialog.present(Some(self));

        let (window, owner, pack_id) = (self.clone(), owner.to_string(), mark.pack_id.clone());
        let weak = dialog.downgrade();
        glib::spawn_future_local(async move {
            let loaded = session.sticker_pack(&owner, &pack_id).await;
            let Some(dialog) = weak.upgrade() else {
                return;
            };
            let Some(content) = dialog
                .child()
                .and_downcast::<adw::ToolbarView>()
                .and_then(|view| view.content())
                .and_downcast::<gtk::Box>()
            else {
                return;
            };
            while let Some(child) = content.first_child() {
                content.remove(&child);
            }
            match loaded {
                Ok(Some(pack)) if !pack.stickers.is_empty() => {
                    window.fill_pack_dialog(&content, &dialog, &session, pack, &owner, own);
                }
                Ok(_) => content.append(&note("This sticker pack is not available.")),
                Err(message) => content.append(&note(&message)),
            }
        });
    }

    fn fill_pack_dialog(
        &self,
        content: &gtk::Box,
        dialog: &adw::Dialog,
        session: &crate::session::Session,
        pack: Pack,
        owner: &str,
        own: bool,
    ) {
        let title = gtk::Label::builder()
            .label(pack.title())
            .xalign(0.0)
            .wrap(true)
            .build();
        title.add_css_class("title-3");
        content.append(&title);
        if let Some(summary) = pack.summary.as_deref().filter(|s| !s.trim().is_empty()) {
            let summary = gtk::Label::builder()
                .label(summary)
                .xalign(0.0)
                .wrap(true)
                .build();
            summary.add_css_class("dim-label");
            content.append(&summary);
        }
        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(4)
            .homogeneous(true)
            .valign(gtk::Align::Start)
            .build();
        for sticker in &pack.stickers {
            grid.append(&super::media_picker::sticker_picture(
                self, session, sticker, 72,
            ));
        }
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&grid)
            .build();
        content.append(&scroller);
        // The author can mark a pack as not to be copied, and a pack that
        // is already ours needs no copy.
        if own {
            return;
        }
        let add = gtk::Button::builder()
            .label(if pack.restricted {
                "Its author does not allow copying"
            } else {
                "Add to My Stickers"
            })
            .sensitive(!pack.restricted)
            .build();
        add.add_css_class("suggested-action");
        add.add_css_class("pill");
        let (window, session, owner, id) = (
            self.downgrade(),
            session.clone(),
            owner.to_string(),
            pack.id.clone(),
        );
        let dialog = dialog.downgrade();
        add.connect_clicked(move |button| {
            button.set_sensitive(false);
            let (window, session, owner, id, dialog) = (
                window.clone(),
                session.clone(),
                owner.clone(),
                id.clone(),
                dialog.clone(),
            );
            glib::spawn_future_local(async move {
                let result = session.import_sticker_pack(&owner, &id).await;
                let Some(window) = window.upgrade() else {
                    return;
                };
                match result {
                    Ok(()) => {
                        window.toast("Added to your stickers.");
                        if let Some(dialog) = dialog.upgrade() {
                            dialog.close();
                        }
                    }
                    Err(message) => window.toast(&message),
                }
            });
        });
        content.append(&add);
    }
}

fn note(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .vexpand(true)
        .valign(gtk::Align::Center)
        .build();
    label.add_css_class("dim-label");
    label
}
