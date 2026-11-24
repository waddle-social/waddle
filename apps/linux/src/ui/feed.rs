//! The Feed destination: the community feed (XEP-0472) with the stories
//! rail above it (see `ui/stories.rs`).
//!
//! What a post is, how they sort and what may be posted is decided in
//! `feed.rs`; this draws it and wires the sidebar row, the composer and
//! refresh to the window. There are no live updates: refresh to see new
//! posts.

use std::collections::HashMap;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};

use super::stories::StoriesState;
use super::{avatar, media, WaddleWindow};
use crate::feed::{self, Post, PostDraft};
use crate::message_text;
use crate::nick_color;
use crate::session::Outgoing;
use crate::stories;

/// A feed older than this is fetched again when the Feed is opened.
const STALE_MS: i64 = 60_000;

/// The widgets of the Feed page.
#[derive(Clone)]
struct Page {
    refresh: gtk::Button,
    rail: gtk::Box,
    note: gtk::Label,
    body: gtk::TextView,
    link: gtk::Entry,
    attach: gtk::Button,
    attached: gtk::Box,
    attached_name: gtk::Label,
    post: gtk::Button,
    posts: gtk::Box,
}

/// The window's Feed state.
#[derive(Default)]
pub struct FeedState {
    open: bool,
    row: Option<adw::ActionRow>,
    page: Option<Page>,
    posts: Vec<Post>,
    /// Display names from profiles, by author address.
    pub(super) names: HashMap<String, String>,
    loading: bool,
    loaded: bool,
    failed: Option<String>,
    loaded_at_ms: Option<i64>,
    /// Bumped by every refresh and on sign-out; older answers are dropped.
    serial: u64,
    /// The picture chosen for the post being written.
    attachment: Option<Outgoing>,
    posting: bool,
    pub(super) stories: StoriesState,
}

/// Milliseconds since the Unix epoch.
pub(super) fn now_ms() -> i64 {
    glib::real_time() / 1000
}

/// Read a picture the user chose, for a post or a story: its type and size
/// are checked before it is read, and it is shared as it is.
pub(super) async fn read_picture(file: &gio::File) -> Result<Outgoing, String> {
    let name = file
        .basename()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "picture".to_string());
    let size = file
        .query_info_future(
            gio::FILE_ATTRIBUTE_STANDARD_SIZE,
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
        .map_err(|_| format!("{name} could not be read."))?
        .size();
    let size = u64::try_from(size).unwrap_or(u64::MAX);
    let guess_type = |bytes: Option<&[u8]>| {
        let (guess, _) = gio::content_type_guess(Some(name.as_str()), bytes);
        gio::content_type_get_mime_type(&guess)
            .map(|mime| mime.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string())
    };
    // Refuse a wrong type or size before reading a large file into memory.
    stories::check_media(&guess_type(None), size).map_err(|error| error.message().to_string())?;
    let (bytes, _) = file
        .load_contents_future()
        .await
        .map_err(|_| format!("{name} could not be read."))?;
    let media_type = guess_type(Some(&bytes[..]));
    stories::check_media(&media_type, bytes.len() as u64)
        .map_err(|error| error.message().to_string())?;
    Ok(Outgoing {
        name: crate::attachments::safe_file_name(&name),
        media_type,
        bytes: bytes.to_vec(),
    })
}

/// A file dialog that offers pictures.
pub(super) fn picture_dialog(title: &str) -> gtk::FileDialog {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Pictures"));
    for media_type in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
        filter.add_mime_type(media_type);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    gtk::FileDialog::builder()
        .title(title)
        .filters(&filters)
        .default_filter(&filter)
        .build()
}

impl WaddleWindow {
    /// The sidebar's Feed row and the page it shows.
    pub(super) fn setup_feed(&self) {
        let imp = self.imp();
        let row = adw::ActionRow::builder()
            .title("Feed")
            .title_lines(1)
            .activatable(true)
            .build();
        row.add_css_class("conversation");
        let icon = gtk::Image::from_icon_name("view-list-symbolic");
        icon.set_valign(gtk::Align::Center);
        row.add_prefix(&icon);
        imp.inbox_list.append(&row);

        let (page, root) = self.build_feed_page();
        imp.content_stack.add_named(&root, Some("feed"));
        let mut state = imp.feed.borrow_mut();
        state.row = Some(row);
        state.page = Some(page);
    }

    /// Whether the sidebar's activated row is the Feed's.
    pub(super) fn is_feed_row(&self, row: &gtk::ListBoxRow) -> bool {
        self.imp()
            .feed
            .borrow()
            .row
            .as_ref()
            .is_some_and(|feed| feed.upcast_ref::<gtk::ListBoxRow>() == row)
    }

    #[must_use]
    pub(super) fn feed_open(&self) -> bool {
        self.imp().feed.borrow().open
    }

    /// Forget the account's feed and stories, on sign-out.
    pub(super) fn reset_feed(&self) {
        let imp = self.imp();
        self.close_story_viewer();
        {
            let mut state = imp.feed.borrow_mut();
            let (row, page, serial) = (state.row.take(), state.page.take(), state.serial);
            *state = FeedState {
                row,
                page,
                serial: serial.wrapping_add(1),
                open: state.open,
                ..FeedState::default()
            };
        }
        if self.feed_open() {
            imp.content_stack.set_visible_child_name("empty");
        }
        self.leave_feed();
        if let Some(page) = self.feed_page() {
            page.body.buffer().set_text("");
            page.link.set_text("");
            for container in [&page.posts, &page.rail] {
                while let Some(child) = container.first_child() {
                    container.remove(&child);
                }
            }
        }
        self.refresh_feed_chrome();
    }

    fn feed_page(&self) -> Option<Page> {
        self.imp().feed.borrow().page.clone()
    }

    pub(super) fn show_feed(&self) {
        let imp = self.imp();
        self.leave_inbox();
        imp.feed.borrow_mut().open = true;
        // Nothing is being read while the feed is on screen.
        imp.current.replace(None);
        self.render_conversations();
        if let Some(row) = imp.feed.borrow().row.as_ref() {
            imp.inbox_list.select_row(Some(row));
        }
        imp.room_title.set_title("Feed");
        imp.room_page.set_title("Feed");
        imp.thread_back.set_visible(false);
        imp.load_earlier.set_visible(false);
        imp.content_stack.set_visible_child_name("feed");
        if let Some(composer) = imp.composer.parent() {
            composer.set_visible(false);
        }
        self.cancel_compose();
        self.refresh_composer();
        self.refresh_notify_button();
        self.refresh_message_tools();
        self.refresh_thread_context();
        imp.split_view.set_show_content(true);
        self.draw_feed();
        let stale = {
            let state = imp.feed.borrow();
            !state.loaded
                || state
                    .loaded_at_ms
                    .is_none_or(|at| now_ms().saturating_sub(at) > STALE_MS)
        };
        if stale {
            self.refresh_feed();
        }
    }

    /// A conversation or the inbox is opening: the feed gives way to it.
    pub(super) fn leave_feed(&self) {
        let imp = self.imp();
        let was_open = std::mem::take(&mut imp.feed.borrow_mut().open);
        if !was_open {
            return;
        }
        self.close_story_viewer();
        imp.inbox_list.unselect_all();
        if let Some(composer) = imp.composer.parent() {
            composer.set_visible(true);
        }
    }

    fn feed_is_current(&self, serial: u64, generation: u64) -> bool {
        let imp = self.imp();
        imp.generation.get() == generation && imp.feed.borrow().serial == serial
    }

    /// Fetch the feed, the stories and what has been seen.
    pub(super) fn refresh_feed(&self) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        if !imp.connected.get() {
            imp.feed.borrow_mut().failed = Some("Waiting for the connection…".to_string());
            self.draw_feed();
            return;
        }
        let (serial, generation) = {
            let mut state = imp.feed.borrow_mut();
            if state.loading {
                return;
            }
            state.loading = true;
            state.failed = None;
            state.serial = state.serial.wrapping_add(1);
            (state.serial, imp.generation.get())
        };
        self.refresh_feed_chrome();
        self.draw_feed();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let now = now_ms();
            let feed = session.feed().await;
            if !window.feed_is_current(serial, generation) {
                return;
            }
            {
                let mut state = window.imp().feed.borrow_mut();
                match feed {
                    Ok(posts) => state.posts = posts,
                    Err(message) => state.failed = Some(message),
                }
            }
            window.draw_feed();

            let fetched = session.stories(now).await;
            if !window.feed_is_current(serial, generation) {
                return;
            }
            {
                let mut state = window.imp().feed.borrow_mut();
                match fetched {
                    Ok(items) => {
                        state.stories.items = items;
                        state.stories.failed = false;
                    }
                    Err(_) => state.stories.failed = true,
                }
            }
            window.draw_story_rail();
            window.sync_story_reads().await;
            if !window.feed_is_current(serial, generation) {
                return;
            }
            window.look_up_names(&session, serial, generation).await;
            if !window.feed_is_current(serial, generation) {
                return;
            }
            {
                let mut state = window.imp().feed.borrow_mut();
                state.loading = false;
                state.loaded = true;
                state.loaded_at_ms = Some(now_ms());
            }
            window.refresh_feed_chrome();
            window.draw_feed();
        });
    }

    /// Profile names for the authors of posts and stories not yet known.
    async fn look_up_names(&self, session: &crate::session::Session, serial: u64, generation: u64) {
        let wanted: Vec<String> = {
            let state = self.imp().feed.borrow();
            let mut authors: Vec<String> = Vec::new();
            let all = state
                .posts
                .iter()
                .filter_map(|post| post.author.clone())
                .chain(
                    state
                        .stories
                        .items
                        .iter()
                        .filter_map(|story| story.author.clone()),
                );
            for author in all {
                if !state.names.contains_key(&author) && !authors.contains(&author) {
                    authors.push(author);
                }
            }
            authors.truncate(40);
            authors
        };
        if wanted.is_empty() {
            return;
        }
        let names = session.author_names(wanted).await;
        if !self.feed_is_current(serial, generation) || names.is_empty() {
            return;
        }
        self.imp().feed.borrow_mut().names.extend(names);
        self.draw_story_rail();
        self.draw_feed();
    }

    /// The refresh button and the busy note follow what is loading.
    fn refresh_feed_chrome(&self) {
        let Some(page) = self.feed_page() else {
            return;
        };
        let loading = self.imp().feed.borrow().loading;
        page.refresh.set_sensitive(!loading);
    }

    /// Redraw the note and the posts.
    pub(super) fn draw_feed(&self) {
        let Some(page) = self.feed_page() else {
            return;
        };
        let (posts, names, loading, loaded, failed) = {
            let state = self.imp().feed.borrow();
            (
                state.posts.clone(),
                state.names.clone(),
                state.loading,
                state.loaded,
                state.failed.clone(),
            )
        };
        let note = match (&failed, loading && posts.is_empty()) {
            (Some(message), _) => Some(format!("The feed could not be refreshed. {message}")),
            (None, true) => Some("Loading…".to_string()),
            (None, false) => None,
        };
        page.note.set_visible(note.is_some());
        page.note.set_label(note.as_deref().unwrap_or(""));
        while let Some(child) = page.posts.first_child() {
            page.posts.remove(&child);
        }
        if posts.is_empty() && loaded && failed.is_none() {
            let empty = adw::StatusPage::builder()
                .icon_name("view-list-symbolic")
                .title("No Posts Yet")
                .description("Share something with the community.")
                .build();
            page.posts.append(&empty);
            return;
        }
        let now = now_ms();
        for post in &posts {
            page.posts.append(&self.post_card(post, &names, now));
        }
    }

    fn post_card(&self, post: &Post, names: &HashMap<String, String>, now: i64) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("card");
        card.add_css_class("feed-post");

        let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let author = post.author.as_deref();
        let name = author.map_or_else(
            || "Someone".to_string(),
            |author| feed::author_name(author, names.get(author).map(String::as_str)),
        );
        let picture = avatar::avatar(40);
        avatar::set_name(&picture, &name);
        if let Some(author) = author {
            self.watch_avatar(&picture, author);
        }
        head.append(&picture);
        let who = gtk::Box::new(gtk::Orientation::Vertical, 0);
        who.set_valign(gtk::Align::Center);
        who.set_hexpand(true);
        let color = nick_color::hex(author.unwrap_or(&name));
        let name_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .use_markup(true)
            .label(format!(
                "<span foreground=\"{color}\" weight=\"bold\">{}</span>",
                glib::markup_escape_text(&name)
            ))
            .build();
        who.append(&name_label);
        let mut meta: Vec<String> = Vec::new();
        if let Some(source) = post.source {
            meta.push(source.label().to_string());
        }
        if let Some(at) = post.published_ms {
            meta.push(feed::ago(now, at));
        }
        let meta_label = gtk::Label::builder()
            .xalign(0.0)
            .label(meta.join(" · "))
            .visible(!meta.is_empty())
            .build();
        meta_label.add_css_class("dim-label");
        meta_label.add_css_class("caption");
        who.append(&meta_label);
        head.append(&who);
        if let Some(author) = author {
            head.set_cursor_from_name(Some("pointer"));
            let click = gtk::GestureClick::new();
            let (window, author, name) = (self.downgrade(), author.to_string(), name.clone());
            click.connect_released(move |_, _, _, _| {
                let Some(window) = window.upgrade() else {
                    return;
                };
                if Some(author.to_lowercase()) == window.own_jid().map(|own| own.to_lowercase()) {
                    window.open_profile();
                } else {
                    window.open_person(Some(author.clone()), name.clone());
                }
            });
            head.add_controller(click);
        }
        card.append(&head);

        if let Some(title) = &post.title {
            let title = gtk::Label::builder()
                .xalign(0.0)
                .wrap(true)
                .label(title)
                .build();
            title.add_css_class("heading");
            card.append(&title);
        }
        let body = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .use_markup(true)
            .label(message_text::body_markup(&post.body))
            .build();
        card.append(&body);

        if let Some(link) = &post.link {
            if media::is_media_only(link.as_str()) {
                let pictures = gtk::Box::new(gtk::Orientation::Vertical, 4);
                media::bind(&pictures, link.as_str(), false);
                card.append(&pictures);
            } else {
                let escaped = glib::markup_escape_text(link.as_str());
                let label = gtk::Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .use_markup(true)
                    .label(format!("<a href=\"{escaped}\">{escaped}</a>"))
                    .build();
                card.append(&label);
            }
        }
        card
    }

    fn build_feed_page(&self) -> (Page, gtk::Box) {
        let head_title = gtk::Label::builder()
            .label("Community")
            .hexpand(true)
            .xalign(0.0)
            .build();
        head_title.add_css_class("title-2");
        let refresh = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh the Feed")
            .valign(gtk::Align::Center)
            .build();
        refresh.add_css_class("flat");
        refresh.add_css_class("circular");

        let rail = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let note = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .build();
        note.add_css_class("dim-label");

        // The composer card.
        let body = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(10)
            .right_margin(10)
            .build();
        body.set_tooltip_text(Some("What do you want to share with the community?"));
        let body_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(84)
            .max_content_height(220)
            .child(&body)
            .build();
        body_scroller.add_css_class("card");
        let link = gtk::Entry::builder()
            .placeholder_text("Link (optional)")
            .input_purpose(gtk::InputPurpose::Url)
            .build();
        let attach = gtk::Button::builder()
            .icon_name("image-x-generic-symbolic")
            .tooltip_text("Add a Picture")
            .valign(gtk::Align::Center)
            .build();
        attach.add_css_class("flat");
        let attached_name = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        attached_name.add_css_class("caption");
        let remove = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Remove the Picture")
            .valign(gtk::Align::Center)
            .build();
        remove.add_css_class("flat");
        remove.add_css_class("circular");
        let attached = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        attached.set_visible(false);
        attached.set_hexpand(true);
        attached.append(&attached_name);
        attached.append(&remove);
        let post = gtk::Button::builder()
            .label("Post")
            .valign(gtk::Align::Center)
            .build();
        post.add_css_class("suggested-action");
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.append(&attach);
        actions.append(&attached);
        actions.append(&post);
        let posts = gtk::Box::new(gtk::Orientation::Vertical, 12);

        let page = Page {
            refresh,
            rail,
            note,
            body,
            link,
            attach,
            attached,
            attached_name,
            post,
            posts,
        };

        let window = self.downgrade();
        page.refresh.connect_clicked({
            let window = window.clone();
            move |_| {
                if let Some(window) = window.upgrade() {
                    window.refresh_feed();
                }
            }
        });
        page.attach.connect_clicked({
            let window = window.clone();
            move |_| {
                if let Some(window) = window.upgrade() {
                    window.choose_post_picture();
                }
            }
        });
        remove.connect_clicked({
            let window = window.clone();
            move |_| {
                if let Some(window) = window.upgrade() {
                    window.imp().feed.borrow_mut().attachment = None;
                    window.draw_attachment();
                }
            }
        });
        page.post.connect_clicked({
            let window = window.clone();
            move |_| {
                if let Some(window) = window.upgrade() {
                    window.post_to_feed();
                }
            }
        });
        page.link.connect_activate({
            move |_| {
                if let Some(window) = window.upgrade() {
                    window.post_to_feed();
                }
            }
        });

        let composer = gtk::Box::new(gtk::Orientation::Vertical, 8);
        composer.append(&body_scroller);
        composer.append(&page.link);
        composer.append(&actions);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
        column.set_margin_start(16);
        column.set_margin_end(16);
        column.set_margin_top(12);
        column.set_margin_bottom(16);
        let rail_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&page.rail)
            .build();
        column.append(&rail_scroller);
        column.append(&composer);
        column.append(&page.note);
        column.append(&page.posts);

        let clamp = adw::Clamp::builder()
            .maximum_size(720)
            .child(&column)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.set_margin_start(16);
        header.set_margin_end(16);
        header.set_margin_top(12);
        header.append(&head_title);
        header.append(&page.refresh);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&header);
        root.append(&scroller);
        (page, root)
    }

    fn choose_post_picture(&self) {
        if self.imp().feed.borrow().posting {
            return;
        }
        let dialog = picture_dialog("Add a Picture");
        let window = self.clone();
        glib::spawn_future_local(async move {
            let Ok(file) = dialog.open_future(Some(&window)).await else {
                return;
            };
            match read_picture(&file).await {
                Ok(picture) => {
                    window.imp().feed.borrow_mut().attachment = Some(picture);
                    window.draw_attachment();
                }
                Err(message) => window.toast(&message),
            }
        });
    }

    fn draw_attachment(&self) {
        let Some(page) = self.feed_page() else {
            return;
        };
        let state = self.imp().feed.borrow();
        match &state.attachment {
            Some(picture) => {
                page.attached_name.set_label(&format!(
                    "{} · {}",
                    picture.name,
                    crate::attachments::format_size(picture.bytes.len() as u64)
                ));
                page.attached.set_visible(true);
            }
            None => page.attached.set_visible(false),
        }
    }

    fn post_to_feed(&self) {
        let imp = self.imp();
        let (Some(page), Some(session)) = (self.feed_page(), imp.session.borrow().clone()) else {
            return;
        };
        if imp.feed.borrow().posting {
            return;
        }
        if !imp.connected.get() {
            return self.toast("Wait until you are connected to post.");
        }
        let buffer = page.body.buffer();
        let body = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        let link = page.link.text().to_string();
        let has_picture = imp.feed.borrow().attachment.is_some();
        // Check what was typed before anything is uploaded.
        let probe = has_picture.then_some("https://example.invalid/picture");
        if let Err(error) = PostDraft::new(&body, &link, probe) {
            return self.toast(error.message());
        }
        let picture = imp.feed.borrow_mut().attachment.take();
        imp.feed.borrow_mut().posting = true;
        page.post.set_sensitive(false);
        page.post.set_label(if picture.is_some() {
            "Uploading…"
        } else {
            "Posting…"
        });
        let generation = imp.generation.get();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let result = async {
                let url = match &picture {
                    Some(file) => Some(
                        session
                            .upload_public(Outgoing {
                                name: file.name.clone(),
                                media_type: file.media_type.clone(),
                                bytes: file.bytes.clone(),
                            })
                            .await
                            .map_err(|error| {
                                format!("The picture could not be uploaded: {error}.")
                            })?,
                    ),
                    None => None,
                };
                let draft = PostDraft::new(&body, &link, url.as_deref())
                    .map_err(|error| error.message().to_string())?;
                session.post_to_feed(draft).await
            }
            .await;
            let imp = window.imp();
            if imp.generation.get() != generation {
                return;
            }
            imp.feed.borrow_mut().posting = false;
            if let Some(page) = window.feed_page() {
                page.post.set_sensitive(true);
                page.post.set_label("Post");
            }
            match result {
                Ok(post) => {
                    feed::prepend(&mut imp.feed.borrow_mut().posts, post);
                    if let Some(page) = window.feed_page() {
                        page.body.buffer().set_text("");
                        page.link.set_text("");
                    }
                    window.draw_attachment();
                    window.draw_feed();
                }
                Err(message) => {
                    // Give the picture back, unless another was chosen.
                    {
                        let mut state = imp.feed.borrow_mut();
                        if state.attachment.is_none() {
                            state.attachment = picture;
                        }
                    }
                    window.draw_attachment();
                    window.toast(&message);
                }
            }
        });
    }

    /// The feed's rail is drawn by `ui/stories.rs`.
    pub(super) fn story_rail(&self) -> Option<gtk::Box> {
        self.feed_page().map(|page| page.rail)
    }
}
