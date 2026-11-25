//! The composer's "+" menu: photo or file, GIF, sticker.
//!
//! One popover with three pages. The GIF page searches the web app's
//! GIPHY proxy (`gifs.rs`) and sends the picked GIF's address as the
//! message body; the sticker page lists the account's packs
//! (`stickers.rs`) and sends a sticker with its pack (XEP-0449).

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::{BoxExt, ButtonExt, EditableExt, ObjectExt, PopoverExt, WidgetExt};
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;
use waddle_client_session::{Delivery, LocalEcho, ReplyTo};
use waddle_xmpp_client_ffi::{WaddleSharedFile, WaddleStickerRef};

use super::actions::Compose;
use super::media;
use super::WaddleWindow;
use crate::gifs::{self, Gif, Outcome};
use crate::media_index::{self, MessageMedia};
use crate::runtime;
use crate::session::{Draft, Kind, Session};
use crate::stickers::{Mark, Pack, Sticker};

const MENU: &str = "menu";
const GIF_PAGE: &str = "gif";
const STICKER_PAGE: &str = "stickers";
const PAGE_SIZE: (i32, i32) = (360, 400);
const GIF_COLUMNS: u32 = 2;

impl WaddleWindow {
    /// Open the "+" menu on the composer's button.
    pub(super) fn show_plus_menu(&self) {
        let Some(anchor) = self.imp().composer.first_child() else {
            return;
        };
        let popover = gtk::Popover::new();
        popover.set_parent(&anchor);
        popover.set_position(gtk::PositionType::Top);
        popover.connect_closed(|popover| {
            // Unparenting inside the signal would pull the popover out
            // from under its own handler.
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .vhomogeneous(false)
            .build();
        stack.add_named(&self.menu_page(&popover, &stack), Some(MENU));
        let (gif, start_gifs) = self.gif_page(&popover, &stack);
        stack.add_named(&gif, Some(GIF_PAGE));
        let (stickers, start_stickers) = self.sticker_page(&popover, &stack);
        stack.add_named(&stickers, Some(STICKER_PAGE));
        stack.connect_visible_child_name_notify(move |stack| {
            match stack.visible_child_name().as_deref() {
                Some(GIF_PAGE) => start_gifs(),
                Some(STICKER_PAGE) => start_stickers(),
                _ => {}
            }
        });
        popover.set_child(Some(&stack));
        popover.popup();
    }

    fn menu_page(&self, popover: &gtk::Popover, stack: &gtk::Stack) -> gtk::Box {
        let page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        let entry = |icon: &str, title: &str| {
            let button = gtk::Button::builder()
                .child(
                    &adw::ButtonContent::builder()
                        .icon_name(icon)
                        .label(title)
                        .halign(gtk::Align::Start)
                        .build(),
                )
                .build();
            button.add_css_class("flat");
            page.append(&button);
            button
        };
        let (window, popover_ref) = (self.downgrade(), popover.clone());
        entry("mail-attachment-symbolic", "Photo or File…").connect_clicked(move |_| {
            popover_ref.popdown();
            if let Some(window) = window.upgrade() {
                window.pick_files();
            }
        });
        for (icon, title, name) in [
            ("image-x-generic-symbolic", "GIF", GIF_PAGE),
            ("face-smile-symbolic", "Sticker", STICKER_PAGE),
        ] {
            let (window, stack, popover) = (self.downgrade(), stack.clone(), popover.clone());
            entry(icon, title).connect_clicked(move |_| {
                // The same reasons as for a file: nothing can be sent now.
                match window.upgrade().and_then(|window| window.cannot_attach()) {
                    Some(reason) => {
                        popover.popdown();
                        if let Some(window) = window.upgrade() {
                            window.toast(reason);
                        }
                    }
                    None => stack.set_visible_child_name(name),
                }
            });
        }
        page
    }

    /// A page with a back button and a title row; `extra` follows the back
    /// button in the header.
    fn picker_page(
        stack: &gtk::Stack,
        title: &impl glib::object::IsA<gtk::Widget>,
    ) -> (gtk::Box, gtk::Box) {
        let page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .width_request(PAGE_SIZE.0)
            .height_request(PAGE_SIZE.1)
            .build();
        let header = gtk::Box::builder().spacing(6).build();
        let back = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Back")
            .build();
        super::a11y::label(&back, "Back to the menu");
        back.add_css_class("flat");
        let stack = stack.clone();
        back.connect_clicked(move |_| stack.set_visible_child_name(MENU));
        header.append(&back);
        title.set_hexpand(true);
        header.append(title);
        page.append(&header);
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .vexpand(true)
            .build();
        page.append(&body);
        (page, body)
    }

    fn gif_page(
        &self,
        popover: &gtk::Popover,
        stack: &gtk::Stack,
    ) -> (gtk::Box, impl Fn() + 'static) {
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search GIFs")
            .search_delay(300)
            .build();
        let (page, body) = Self::picker_page(stack, &search);
        let status = gtk::Label::builder()
            .vexpand(true)
            .valign(gtk::Align::Center)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        status.add_css_class("dim-label");
        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .min_children_per_line(GIF_COLUMNS)
            .max_children_per_line(GIF_COLUMNS)
            .homogeneous(true)
            .valign(gtk::Align::Start)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&grid)
            .visible(false)
            .build();
        body.append(&status);
        body.append(&scroller);

        let base = gifs::base_url(std::env::var(gifs::BASE_ENV).ok().as_deref());
        // Only the newest search may fill the grid.
        let latest = Rc::new(Cell::new(0u64));
        let run = {
            let (window, popover, status, grid, scroller) = (
                self.downgrade(),
                popover.clone(),
                status.clone(),
                grid.clone(),
                scroller.clone(),
            );
            Rc::new(move |query: String| {
                let id = latest.get() + 1;
                latest.set(id);
                grid.remove_all();
                scroller.set_visible(false);
                status.set_label("Loading…");
                status.set_visible(true);
                let (window, popover, status, grid, scroller, latest, base) = (
                    window.clone(),
                    popover.clone(),
                    status.clone(),
                    grid.clone(),
                    scroller.clone(),
                    latest.clone(),
                    base.clone(),
                );
                glib::spawn_future_local(async move {
                    let outcome = runtime::spawn(gifs::search(base, query)).await;
                    if latest.get() != id {
                        return;
                    }
                    let message = match outcome {
                        Ok(Outcome::Results(found)) if !found.is_empty() => {
                            for gif in found {
                                grid.append(&gif_button(&window, &popover, gif));
                            }
                            status.set_visible(false);
                            scroller.set_visible(true);
                            return;
                        }
                        Ok(Outcome::Results(_)) => "No GIFs found.".to_string(),
                        Ok(Outcome::NotConfigured) => {
                            "GIF search is not set up on this server.".to_string()
                        }
                        Ok(Outcome::Unavailable(message)) => message,
                        Err(_) => gifs::UNAVAILABLE.to_string(),
                    };
                    status.set_label(&message);
                });
            })
        };
        let query = {
            let search = search.clone();
            move || search.text().to_string()
        };
        let again = run.clone();
        search.connect_search_changed(move |search| again(search.text().to_string()));
        // Trending when the page opens, or the search left from before.
        let start = move || run(query());
        (page, start)
    }

    fn sticker_page(
        &self,
        popover: &gtk::Popover,
        stack: &gtk::Stack,
    ) -> (gtk::Box, impl Fn() + 'static) {
        let title = gtk::Label::builder().label("Stickers").xalign(0.0).build();
        title.add_css_class("heading");
        let (page, body) = Self::picker_page(stack, &title);
        let status = gtk::Label::builder()
            .vexpand(true)
            .valign(gtk::Align::Center)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        status.add_css_class("dim-label");
        let packs = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&packs)
            .visible(false)
            .build();
        body.append(&status);
        body.append(&scroller);

        let (window, popover) = (self.downgrade(), popover.clone());
        let latest = Rc::new(Cell::new(0u64));
        let start = move || {
            let id = latest.get() + 1;
            latest.set(id);
            while let Some(child) = packs.first_child() {
                packs.remove(&child);
            }
            scroller.set_visible(false);
            status.set_label("Loading…");
            status.set_visible(true);
            let Some(session) = window
                .upgrade()
                .and_then(|window| window.imp().session.borrow().clone())
            else {
                return;
            };
            let (window, popover, status, packs, scroller, latest) = (
                window.clone(),
                popover.clone(),
                status.clone(),
                packs.clone(),
                scroller.clone(),
                latest.clone(),
            );
            glib::spawn_future_local(async move {
                let loaded = session.sticker_packs().await;
                if latest.get() != id {
                    return;
                }
                let Some(owner) = window.upgrade() else {
                    return;
                };
                match loaded {
                    Ok(found) if !found.is_empty() => {
                        for pack in found {
                            packs.append(&pack_section(&owner, &session, &popover, pack));
                        }
                        status.set_visible(false);
                        scroller.set_visible(true);
                    }
                    Ok(_) => status.set_label(
                        "You have no stickers yet.\nMake a sticker pack in Waddle on the web\nor on iOS and it shows up here.",
                    ),
                    Err(message) => status.set_label(&message),
                }
            });
        };
        (page, start)
    }

    /// Send a message built by a picker, as the composer's own send does:
    /// echoed at once, flagged if it fails, quoting the reply in progress.
    fn send_prepared(
        &self,
        body: String,
        files: Vec<WaddleSharedFile>,
        sticker: Option<(Sticker, WaddleStickerRef)>,
    ) {
        let imp = self.imp();
        let (Some(conversation), Some(session)) =
            (self.current_conversation(), imp.session.borrow().clone())
        else {
            return;
        };
        if let Some(reason) = self.cannot_attach() {
            return self.toast(reason);
        }
        let quote = match imp.compose.replace(Compose::Message) {
            Compose::Reply(quote) => Some(quote),
            Compose::Message | Compose::Edit { .. } => None,
        };
        imp.composer_context.set_visible(false);
        let origin = uuid::Uuid::new_v4().to_string();
        imp.outbound
            .borrow_mut()
            .insert(origin.clone(), conversation.clone());
        let reference_id = (conversation.kind == Kind::Direct).then(|| origin.clone());
        let attachments = sticker
            .iter()
            .map(|(sticker, _)| sticker.attachment())
            .collect();
        if let Some((_, reference)) = &sticker {
            // A direct message is known by the id we chose, so its echo
            // can already be drawn as a sticker.
            media_index::record(
                &conversation.jid,
                reference_id.as_deref(),
                Some(MessageMedia {
                    sticker: true,
                    pack: Mark::from_wire(reference),
                    ..MessageMedia::default()
                }),
            );
        }
        let echo = LocalEcho {
            origin_id: origin.clone(),
            sender: session.me.nick.clone(),
            body: body.clone(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            reference_id,
            reply_to: quote.as_ref().map(|quote| ReplyTo {
                id: quote.id.clone(),
                sender: Some(quote.sender.clone()),
            }),
            attachments,
        };
        self.edit_conversation(&conversation.key(), |timeline| {
            timeline.add_local_echo(echo)
        });
        self.scroll_to_end();

        let window = self.clone();
        let generation = imp.generation.get();
        glib::spawn_future_local(async move {
            let sent = session
                .send(
                    &conversation.jid,
                    conversation.kind,
                    Draft {
                        body,
                        origin_id: origin.clone(),
                        quote,
                        thread: conversation.thread.clone(),
                        files,
                        mentionable: Vec::new(),
                        markup: Vec::new(),
                        sticker: sticker.map(|(_, reference)| reference),
                    },
                )
                .await;
            if window.imp().generation.get() != generation {
                return;
            }
            if !sent {
                window.imp().outbound.borrow_mut().remove(&origin);
                window.edit_conversation(&conversation.key(), |timeline| {
                    timeline.set_delivery(&origin, Delivery::Failed)
                });
                window.toast("The message could not be sent.");
            }
        });
    }
}

/// One GIF in the grid: its small rendition, animated; a click sends it.
fn gif_button(
    window: &glib::WeakRef<WaddleWindow>,
    popover: &gtk::Popover,
    gif: Gif,
) -> gtk::Button {
    let picture = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Cover)
        .overflow(gtk::Overflow::Hidden)
        .width_request(150)
        .height_request(100)
        .alternative_text(if gif.title.is_empty() {
            "GIF"
        } else {
            &gif.title
        })
        .build();
    picture.add_css_class("media-placeholder");
    let weak = picture.downgrade();
    let preview = gif.preview.clone();
    glib::spawn_future_local(async move {
        let download = runtime::spawn(media::download(preview));
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
                picture.remove_css_class("media-placeholder");
                media::play(&picture, &animation);
            }
        }
    });
    let button = gtk::Button::builder()
        .child(&picture)
        .tooltip_text(&gif.title)
        .build();
    super::a11y::label(
        &button,
        &if gif.title.is_empty() {
            "Send this GIF".to_string()
        } else {
            format!("Send GIF: {}", gif.title)
        },
    );
    button.add_css_class("flat");
    let (window, popover) = (window.clone(), popover.clone());
    button.connect_clicked(move |_| {
        popover.popdown();
        if let Some(window) = window.upgrade() {
            window.send_prepared(gif.original.to_string(), Vec::new(), None);
        }
    });
    button
}

/// A pack's name and its stickers; a click sends one.
fn pack_section(
    window: &WaddleWindow,
    session: &Session,
    popover: &gtk::Popover,
    pack: Pack,
) -> gtk::Box {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    let title = gtk::Label::builder()
        .label(pack.title())
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    title.add_css_class("caption-heading");
    title.add_css_class("dim-label");
    section.append(&title);
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(4)
        .homogeneous(true)
        .valign(gtk::Align::Start)
        .build();
    for sticker in &pack.stickers {
        let button = gtk::Button::builder()
            .child(&sticker_picture(window, session, sticker, 72))
            .tooltip_text(sticker.text())
            .build();
        super::a11y::label(&button, &format!("Send sticker: {}", sticker.text()));
        button.add_css_class("flat");
        let (weak, popover, sticker, reference) = (
            window.downgrade(),
            popover.clone(),
            sticker.clone(),
            pack.reference(),
        );
        button.connect_clicked(move |_| {
            popover.popdown();
            if let Some(window) = weak.upgrade() {
                window.send_prepared(
                    sticker.text(),
                    vec![sticker.shared_file()],
                    Some((sticker.clone(), reference.clone())),
                );
            }
        });
        grid.append(&button);
    }
    section.append(&grid);
    section
}

/// A sticker's picture at `size`, filled in once it has been fetched. The
/// fetch goes through the same checked transfer path as a shared file.
pub(super) fn sticker_picture(
    window: &WaddleWindow,
    session: &Session,
    sticker: &Sticker,
    size: i32,
) -> gtk::Picture {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .width_request(size)
        .height_request(size)
        .alternative_text(sticker.text())
        .build();
    let (weak, session, attachment) = (picture.downgrade(), session.clone(), sticker.attachment());
    let generation = window.imp().generation.get();
    let window = window.downgrade();
    glib::spawn_future_local(async move {
        let Ok(bytes) = session.fetch(attachment).await else {
            return;
        };
        let Ok(decoded) = super::images::decode_off_main(bytes).await else {
            return;
        };
        let current = window
            .upgrade()
            .is_some_and(|window| window.imp().generation.get() == generation);
        if let (true, Some(picture)) = (current, weak.upgrade()) {
            picture.set_paintable(Some(&decoded.texture()));
        }
    });
    picture
}
