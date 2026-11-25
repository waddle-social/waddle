//! Stories (XEP-0501) in the Feed: the rail of who has one up, the viewer
//! that steps through them on a timer with emoji reactions (XEP-0470), and
//! the dialog that shares a picture as a story.
//!
//! Which stories show, in what order, how the viewer steps and how seen
//! stories are remembered is decided in `stories.rs`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gdk, glib};

use super::feed::{now_ms, picture_dialog, read_picture};
use super::images::{self, Lru};
use super::{avatar, WaddleWindow};
use crate::feed as feed_logic;
use crate::nick_color;
use crate::session::{Outgoing, Session};
use crate::stories::{
    self, Finish, MediaKind, Playthrough, ReactionWrites, Reactions, Reads, Step, Story,
    QUICK_EMOJI, REACTION_LIMIT,
};

/// How often the viewer's clock and bars move.
const TICK: Duration = Duration::from_millis(50);
/// Pictures kept decoded while one viewing runs.
const KEPT_PICTURES: usize = 6;

/// The window's stories state.
#[derive(Default)]
pub struct StoriesState {
    pub(super) items: Vec<Story>,
    pub(super) reads: Reads,
    pub(super) failed: bool,
    reactions: HashMap<String, Reactions>,
    /// Stories whose reactions were asked for.
    asked: HashSet<String>,
    writes: ReactionWrites,
    syncing: bool,
    resync: bool,
    viewer: Option<Viewer>,
}

impl StoriesState {
    /// The author stamped on a listed story: its reactions live on that
    /// account's PEP service.
    fn author_of(&self, id: &str) -> Option<String> {
        self.items
            .iter()
            .find(|story| story.id == id)
            .and_then(|story| story.author.clone())
    }
}

/// An open viewer.
struct Viewer {
    dialog: adw::Dialog,
    watch: Playthrough,
    bars: gtk::Box,
    avatar: adw::Avatar,
    who: gtk::Label,
    when: gtk::Label,
    pause: gtk::ToggleButton,
    stack: gtk::Stack,
    picture: gtk::Picture,
    error: gtk::Label,
    video: gtk::Button,
    caption: gtk::Label,
    reactions: gtk::FlowBox,
    note: gtk::Label,
    tick: Option<glib::SourceId>,
    last_tick: Instant,
    /// The shown story's picture is on screen, so its clock runs.
    showing: bool,
    pictures: Lru<gdk::Texture>,
}

impl WaddleWindow {
    /// Remember a story as seen here and on the account's other devices:
    /// fetch what the server holds, merge, and publish only when this
    /// device adds to it, never over a copy that could not be fetched.
    pub(super) async fn sync_story_reads(&self) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        {
            let mut feed = imp.feed.borrow_mut();
            let state = &mut feed.stories;
            if state.syncing {
                state.resync = true;
                return;
            }
            state.syncing = true;
        }
        let generation = imp.generation.get();
        loop {
            imp.feed.borrow_mut().stories.resync = false;
            let Ok(remote) = session.story_reads().await else {
                break;
            };
            if imp.generation.get() != generation {
                return;
            }
            let now = now_ms();
            let publish = {
                let mut feed = imp.feed.borrow_mut();
                let reads = &mut feed.stories.reads;
                reads.merge(&remote);
                reads.adds_to(&remote, now).then(|| reads.to_wire(now))
            };
            self.draw_story_rail();
            if let Some(wire) = publish {
                if let Err(message) = session.publish_story_reads(wire).await {
                    tracing::debug!(error = %message, "could not store the seen stories");
                }
                if imp.generation.get() != generation {
                    return;
                }
            }
            if !imp.feed.borrow().stories.resync {
                break;
            }
        }
        imp.feed.borrow_mut().stories.syncing = false;
    }

    /// Redraw the rail: add a story, then one ring per author.
    pub(super) fn draw_story_rail(&self) {
        let Some(rail) = self.story_rail() else {
            return;
        };
        while let Some(child) = rail.first_child() {
            rail.remove(&child);
        }
        let (rings, names, failed) = {
            let feed = self.imp().feed.borrow();
            let own = self.own_jid().unwrap_or_default();
            (
                stories::rings(&feed.stories.items, &feed.stories.reads, &own),
                feed.names.clone(),
                feed.stories.failed,
            )
        };
        let window = self.downgrade();

        let add = gtk::Button::new();
        add.add_css_class("flat");
        add.set_tooltip_text(Some("Share a Story"));
        super::a11y::label(&add, "Share a story");
        let circle = gtk::Box::new(gtk::Orientation::Vertical, 0);
        circle.set_halign(gtk::Align::Center);
        circle.set_valign(gtk::Align::Center);
        circle.set_size_request(58, 58);
        circle.add_css_class("story-add");
        let plus = gtk::Image::from_icon_name("list-add-symbolic");
        plus.set_vexpand(true);
        plus.set_pixel_size(24);
        circle.append(&plus);
        add.set_child(Some(&item_column(&circle, "Add Story")));
        add.connect_clicked({
            let window = window.clone();
            move |_| {
                if let Some(window) = window.upgrade() {
                    window.open_story_composer();
                }
            }
        });
        rail.append(&add);

        for ring in &rings {
            let name = if ring.own {
                "Your story".to_string()
            } else {
                feed_logic::author_name(&ring.author, names.get(&ring.author).map(String::as_str))
            };
            let picture = avatar::avatar(52);
            avatar::set_name(&picture, &name);
            self.watch_avatar(&picture, &ring.author);
            let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
            frame.set_halign(gtk::Align::Center);
            frame.add_css_class("story-ring");
            frame.add_css_class(if ring.unseen {
                "story-unseen"
            } else {
                "story-seen"
            });
            frame.append(&picture);
            let button = gtk::Button::new();
            button.add_css_class("flat");
            let phrase = super::a11y::story_phrase(&name, ring.stories.len(), ring.unseen);
            button.set_tooltip_text(Some(&phrase));
            super::a11y::label(&button, &phrase);
            button.set_child(Some(&item_column(&frame, &name)));
            let author = ring.author.clone();
            button.connect_clicked({
                let window = window.clone();
                move |_| {
                    if let Some(window) = window.upgrade() {
                        window.open_story_viewer(&author);
                    }
                }
            });
            rail.append(&button);
        }
        if failed && rings.is_empty() {
            let label = gtk::Label::new(Some("Stories could not be loaded."));
            label.add_css_class("dim-label");
            label.set_valign(gtk::Align::Center);
            rail.append(&label);
        }
    }

    // MARK: Viewer

    /// Watch `author`'s stories, then those of the rings after theirs.
    fn open_story_viewer(&self, author: &str) {
        if self.imp().feed.borrow().stories.viewer.is_some() {
            return;
        }
        let watch = {
            let feed = self.imp().feed.borrow();
            let own = self.own_jid().unwrap_or_default();
            let rings = stories::rings(&feed.stories.items, &feed.stories.reads, &own);
            let Some(start) = rings
                .iter()
                .position(|ring| ring.author.eq_ignore_ascii_case(author))
            else {
                return;
            };
            Playthrough::open(&rings, start, &feed.stories.reads)
        };
        let Some(watch) = watch else {
            return;
        };

        let dialog = adw::Dialog::builder()
            .title("Story")
            .content_width(440)
            .content_height(700)
            .build();
        let bars = gtk::Box::builder()
            .spacing(4)
            .homogeneous(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .build();
        let picture_avatar = avatar::avatar(36);
        let who = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .use_markup(true)
            .build();
        let when = gtk::Label::builder().xalign(0.0).build();
        when.add_css_class("dim-label");
        when.add_css_class("caption");
        let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
        names.set_hexpand(true);
        names.set_valign(gtk::Align::Center);
        names.append(&who);
        names.append(&when);
        let pause = gtk::ToggleButton::builder()
            .icon_name("media-playback-pause-symbolic")
            .tooltip_text("Pause (Space)")
            .valign(gtk::Align::Center)
            .build();
        super::a11y::label(&pause, "Pause story");
        pause.add_css_class("flat");
        pause.add_css_class("circular");
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Close")
            .valign(gtk::Align::Center)
            .build();
        super::a11y::label(&close, "Close story viewer");
        close.add_css_class("flat");
        close.add_css_class("circular");
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        head.set_margin_start(12);
        head.set_margin_end(12);
        head.append(&picture_avatar);
        head.append(&names);
        head.append(&pause);
        head.append(&close);

        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .alternative_text("Story picture")
            .build();
        let loading = adw::Spinner::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .width_request(32)
            .height_request(32)
            .build();
        let error = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        error.add_css_class("dim-label");
        let video = gtk::Button::builder().label("Open Video").build();
        video.add_css_class("pill");
        video.add_css_class("suggested-action");
        let video_note = gtk::Label::new(Some(
            "This story is a video. Waddle shows pictures here; open the video to watch it.",
        ));
        video_note.set_wrap(true);
        video_note.set_justify(gtk::Justification::Center);
        let video_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        video_page.set_halign(gtk::Align::Center);
        video_page.set_valign(gtk::Align::Center);
        video_page.set_margin_start(24);
        video_page.set_margin_end(24);
        video_page.append(&video_note);
        video_page.append(&video);
        let stack = gtk::Stack::new();
        stack.add_named(&picture, Some("picture"));
        stack.add_named(&loading, Some("loading"));
        stack.add_named(&error, Some("error"));
        stack.add_named(&video_page, Some("video"));
        stack.set_vexpand(true);

        let previous = edge_button(
            "go-previous-symbolic",
            "Previous Story (Left)",
            gtk::Align::Start,
        );
        let next = edge_button("go-next-symbolic", "Next Story (Right)", gtk::Align::End);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        overlay.add_overlay(&previous);
        overlay.add_overlay(&next);
        overlay.set_vexpand(true);

        let caption = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .xalign(0.0)
            .selectable(true)
            .margin_start(16)
            .margin_end(16)
            .build();
        let reactions = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(8)
            .hexpand(true)
            .homogeneous(false)
            .column_spacing(4)
            .row_spacing(4)
            .build();
        let chooser = gtk::EmojiChooser::new();
        let more = gtk::MenuButton::builder()
            .icon_name("face-smile-symbolic")
            .tooltip_text("More Reactions")
            .valign(gtk::Align::Start)
            .popover(&chooser)
            .build();
        super::a11y::label(&more, "More reactions");
        more.add_css_class("flat");
        more.add_css_class("circular");
        let reaction_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        reaction_row.set_margin_start(12);
        reaction_row.set_margin_end(12);
        reaction_row.append(&reactions);
        reaction_row.append(&more);
        let note = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .margin_start(16)
            .margin_end(16)
            .margin_bottom(8)
            .build();
        note.add_css_class("error");
        note.add_css_class("caption");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        content.append(&bars);
        content.append(&head);
        content.append(&overlay);
        content.append(&caption);
        content.append(&reaction_row);
        content.append(&note);
        content.set_margin_bottom(8);
        dialog.set_child(Some(&content));

        let window = self.downgrade();
        let act = |step: fn(&WaddleWindow)| {
            let window = window.clone();
            move |_: &_| {
                if let Some(window) = window.upgrade() {
                    step(&window);
                }
            }
        };
        previous.connect_clicked(act(|w| w.story_step(-1)));
        next.connect_clicked(act(|w| w.story_step(1)));
        close.connect_clicked({
            let dialog = dialog.clone();
            move |_| {
                dialog.close();
            }
        });
        pause.connect_toggled({
            let window = window.clone();
            move |button| {
                if let Some(window) = window.upgrade() {
                    window.story_pause(button.is_active());
                }
            }
        });
        video.connect_clicked(act(|w| w.open_story_video()));
        chooser.connect_emoji_picked({
            let window = window.clone();
            move |_, emoji| {
                if let Some(window) = window.upgrade() {
                    window.react_to_story(emoji);
                }
            }
        });
        // Left and right step, Space pauses; the middle of the picture
        // pauses and its sides step, as on the phones.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let window = window.clone();
            move |_, key, _, _| {
                let Some(window) = window.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                match key {
                    gdk::Key::Left => window.story_step(-1),
                    gdk::Key::Right => window.story_step(1),
                    gdk::Key::space => window.toggle_story_pause(),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        });
        dialog.add_controller(keys);
        let click = gtk::GestureClick::new();
        click.connect_released({
            let window = window.clone();
            let stack = stack.clone();
            move |_, _, x, _| {
                let (Some(window), width) = (window.upgrade(), f64::from(stack.width())) else {
                    return;
                };
                if width <= 0.0 {
                    return;
                }
                match x / width {
                    f if f < 0.33 => window.story_step(-1),
                    f if f > 0.67 => window.story_step(1),
                    _ => window.toggle_story_pause(),
                }
            }
        });
        stack.add_controller(click);
        dialog.connect_closed({
            let window = window.clone();
            move |_| {
                if let Some(window) = window.upgrade() {
                    window.story_viewer_closed();
                }
            }
        });

        let tick = glib::timeout_add_local(TICK, {
            let window = window.clone();
            move || match window.upgrade() {
                Some(window) => window.story_tick(),
                None => glib::ControlFlow::Break,
            }
        });
        self.imp().feed.borrow_mut().stories.viewer = Some(Viewer {
            dialog: dialog.clone(),
            watch,
            bars,
            avatar: picture_avatar,
            who,
            when,
            pause,
            stack,
            picture,
            error,
            video,
            caption,
            reactions,
            note,
            tick: Some(tick),
            last_tick: Instant::now(),
            showing: false,
            pictures: Lru::new(KEPT_PICTURES),
        });
        dialog.present(Some(self));
        self.show_story();
    }

    /// Close the viewer if it is open.
    pub(super) fn close_story_viewer(&self) {
        let dialog = self
            .imp()
            .feed
            .borrow()
            .stories
            .viewer
            .as_ref()
            .map(|viewer| viewer.dialog.clone());
        if let Some(dialog) = dialog {
            dialog.close();
        }
    }

    fn story_viewer_closed(&self) {
        let viewer = self.imp().feed.borrow_mut().stories.viewer.take();
        if let Some(tick) = viewer.and_then(|mut viewer| viewer.tick.take()) {
            tick.remove();
        }
        // What was seen moves the rings.
        self.draw_story_rail();
    }

    /// Show the story the viewer is on: who, when, caption, bars,
    /// reactions and the picture. Seeing it marks it read.
    fn show_story(&self) {
        let imp = self.imp();
        let now = now_ms();
        let (story, range, fills, session) = {
            let mut feed = imp.feed.borrow_mut();
            let Some(viewer) = feed.stories.viewer.as_mut() else {
                return;
            };
            viewer.showing = false;
            viewer.note.set_visible(false);
            let story = viewer.watch.current().clone();
            let range = viewer.watch.same_author();
            let fills: Vec<f64> = range.clone().map(|at| viewer.watch.fill(at)).collect();
            (story, range, fills, imp.session.borrow().clone())
        };
        let own = self.own_jid().unwrap_or_default();
        let author = story.author.clone().unwrap_or_default();
        let name = {
            let feed = imp.feed.borrow();
            if author.eq_ignore_ascii_case(&own) {
                "Your story".to_string()
            } else {
                feed_logic::author_name(&author, feed.names.get(&author).map(String::as_str))
            }
        };
        {
            let feed = imp.feed.borrow();
            let Some(viewer) = feed.stories.viewer.as_ref() else {
                return;
            };
            avatar::set_name(&viewer.avatar, &name);
            if !author.is_empty() {
                self.watch_avatar(&viewer.avatar, &author);
            }
            viewer.who.set_label(&format!(
                "<span foreground=\"{}\" weight=\"bold\">{}</span>",
                nick_color::hex(&author),
                glib::markup_escape_text(&name)
            ));
            viewer.when.set_label(
                &story
                    .posted_ms
                    .map(|at| feed_logic::ago(now, at))
                    .unwrap_or_default(),
            );
            viewer
                .caption
                .set_label(story.body.as_deref().unwrap_or(""));
            viewer.caption.set_visible(story.body.is_some());
            while let Some(child) = viewer.bars.first_child() {
                viewer.bars.remove(&child);
            }
            for fill in &fills {
                let bar = gtk::ProgressBar::new();
                bar.set_fraction(*fill);
                bar.set_valign(gtk::Align::Center);
                viewer.bars.append(&bar);
            }
            let _ = &range;
        }
        self.draw_story_reactions();

        // Seeing a story marks it read, here and on the other devices.
        let news = imp.feed.borrow_mut().stories.reads.mark(&story.id, now);
        if news {
            let window = self.clone();
            glib::spawn_future_local(async move { window.sync_story_reads().await });
        }
        if let Some(session) = &session {
            self.ask_story_reactions(session, &story.id);
        }

        match story.kind() {
            MediaKind::Video => {
                if let Some(viewer) = imp.feed.borrow().stories.viewer.as_ref() {
                    let openable = feed_logic::safe_link(&story.media_url).is_some();
                    viewer.video.set_visible(openable);
                    viewer.stack.set_visible_child_name("video");
                }
            }
            MediaKind::Picture => self.load_story_picture(&story),
        }
    }

    fn load_story_picture(&self, story: &Story) {
        let imp = self.imp();
        let cached = {
            let mut feed = imp.feed.borrow_mut();
            let Some(viewer) = feed.stories.viewer.as_mut() else {
                return;
            };
            match viewer.pictures.get(&story.id).cloned() {
                Some(texture) => {
                    viewer.picture.set_paintable(Some(&texture));
                    viewer.stack.set_visible_child_name("picture");
                    viewer.showing = true;
                    viewer.last_tick = Instant::now();
                    true
                }
                None => {
                    viewer.stack.set_visible_child_name("loading");
                    false
                }
            }
        };
        if cached {
            return;
        }
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let (window, id, url) = (self.clone(), story.id.clone(), story.media_url.clone());
        let generation = imp.generation.get();
        glib::spawn_future_local(async move {
            let result = match session.fetch_public(url).await {
                Ok(bytes) => images::decode_off_main(bytes)
                    .await
                    .map(|decoded| decoded.texture())
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            if window.imp().generation.get() != generation {
                return;
            }
            let mut feed = window.imp().feed.borrow_mut();
            let Some(viewer) = feed.stories.viewer.as_mut() else {
                return;
            };
            if let Ok(texture) = &result {
                viewer.pictures.insert(id.clone(), texture.clone());
            }
            if viewer.watch.current().id != id {
                return;
            }
            match result {
                Ok(texture) => {
                    viewer.picture.set_paintable(Some(&texture));
                    viewer.stack.set_visible_child_name("picture");
                    viewer.showing = true;
                    viewer.last_tick = Instant::now();
                }
                Err(message) => {
                    viewer.error.set_label(&format!(
                        "This picture could not be shown. {}",
                        capitalise(&message)
                    ));
                    viewer.stack.set_visible_child_name("error");
                }
            }
        });
    }

    fn story_step(&self, offset: isize) {
        let step = {
            let mut feed = self.imp().feed.borrow_mut();
            let Some(viewer) = feed.stories.viewer.as_mut() else {
                return;
            };
            viewer.watch.step(offset)
        };
        self.after_story_step(step);
    }

    fn after_story_step(&self, step: Step) {
        match step {
            Step::Moved => self.show_story(),
            Step::Finished => self.close_story_viewer(),
            Step::Unchanged => {
                // Back from the first story starts it again.
                if let Some(viewer) = self.imp().feed.borrow().stories.viewer.as_ref() {
                    self.fill_story_bars(viewer);
                }
            }
        }
    }

    fn story_pause(&self, paused: bool) {
        if let Some(viewer) = self.imp().feed.borrow_mut().stories.viewer.as_mut() {
            viewer.watch.set_paused(paused);
            viewer.pause.set_icon_name(if paused {
                "media-playback-start-symbolic"
            } else {
                "media-playback-pause-symbolic"
            });
            viewer.pause.set_tooltip_text(Some(if paused {
                "Play (Space)"
            } else {
                "Pause (Space)"
            }));
        }
    }

    fn toggle_story_pause(&self) {
        let button = self
            .imp()
            .feed
            .borrow()
            .stories
            .viewer
            .as_ref()
            .map(|viewer| viewer.pause.clone());
        if let Some(button) = button {
            button.set_active(!button.is_active());
        }
    }

    fn fill_story_bars(&self, viewer: &Viewer) {
        let range = viewer.watch.same_author();
        let mut bar = viewer.bars.first_child();
        for at in range {
            let Some(widget) = bar else {
                break;
            };
            if let Some(progress) = widget.downcast_ref::<gtk::ProgressBar>() {
                progress.set_fraction(viewer.watch.fill(at));
            }
            bar = widget.next_sibling();
        }
    }

    fn story_tick(&self) -> glib::ControlFlow {
        let step = {
            let mut feed = self.imp().feed.borrow_mut();
            let Some(viewer) = feed.stories.viewer.as_mut() else {
                return glib::ControlFlow::Break;
            };
            let delta = i64::try_from(viewer.last_tick.elapsed().as_millis()).unwrap_or(0);
            viewer.last_tick = Instant::now();
            if !viewer.showing {
                return glib::ControlFlow::Continue;
            }
            let step = viewer.watch.tick(delta);
            if step.is_none() {
                self.fill_story_bars(viewer);
            }
            step
        };
        if let Some(step) = step {
            self.after_story_step(step);
        }
        glib::ControlFlow::Continue
    }

    fn open_story_video(&self) {
        let url = self
            .imp()
            .feed
            .borrow()
            .stories
            .viewer
            .as_ref()
            .map(|viewer| viewer.watch.current().media_url.clone());
        let Some(url) = url.as_deref().and_then(feed_logic::safe_link) else {
            return;
        };
        gtk::UriLauncher::new(url.as_str()).launch(
            Some(self),
            None::<&gtk::gio::Cancellable>,
            |result| {
                if let Err(error) = result {
                    tracing::warn!(%error, "could not open the story video");
                }
            },
        );
    }

    // MARK: Reactions

    /// Ask for the reactions on a story the first time it is shown.
    fn ask_story_reactions(&self, session: &Session, id: &str) {
        let imp = self.imp();
        if !imp.feed.borrow_mut().stories.asked.insert(id.to_string()) {
            return;
        }
        let author = imp.feed.borrow().stories.author_of(id);
        let (window, session, id) = (self.clone(), session.clone(), id.to_string());
        let generation = imp.generation.get();
        glib::spawn_future_local(async move {
            let result = session.story_reactions(author, id.clone()).await;
            if window.imp().generation.get() != generation {
                return;
            }
            {
                let mut feed = window.imp().feed.borrow_mut();
                let state = &mut feed.stories;
                match result {
                    // An answer must not replace a change still on its way.
                    Ok(reactions) if !state.writes.pending(&id) => {
                        state.reactions.insert(id.clone(), reactions);
                    }
                    Ok(_) => {}
                    // Asked again the next time it is shown.
                    Err(_) => {
                        state.asked.remove(&id);
                    }
                }
            }
            window.draw_story_reactions();
        });
    }

    fn draw_story_reactions(&self) {
        let feed = self.imp().feed.borrow();
        let Some(viewer) = feed.stories.viewer.as_ref() else {
            return;
        };
        let id = viewer.watch.current().id.clone();
        let reactions = feed.stories.reactions.get(&id).cloned().unwrap_or_default();
        while let Some(child) = viewer.reactions.first_child() {
            viewer.reactions.remove(&child);
        }
        let mut shown: Vec<&str> = QUICK_EMOJI.to_vec();
        for emoji in reactions
            .counts
            .iter()
            .map(|(emoji, _)| emoji)
            .chain(reactions.mine.iter())
        {
            if !shown.contains(&emoji.as_str()) {
                shown.push(emoji);
            }
        }
        let window = self.downgrade();
        for emoji in shown {
            let count = reactions.count_of(emoji);
            let label = if count > 0 {
                format!("{emoji} {count}")
            } else {
                emoji.to_string()
            };
            let button = gtk::ToggleButton::with_label(&label);
            button.add_css_class("reaction");
            button.set_active(reactions.mine.iter().any(|mine| mine == emoji));
            button.set_tooltip_text(Some("React"));
            let (window, emoji) = (window.clone(), emoji.to_string());
            button.connect_clicked(move |_| {
                if let Some(window) = window.upgrade() {
                    window.react_to_story(&emoji);
                }
            });
            viewer.reactions.append(&button);
        }
    }

    /// Add this account's reaction to the story shown, or take it back.
    /// Shown at once; written one at a time per story, each with all of the
    /// account's emoji, so quick taps end as the last one left them.
    fn react_to_story(&self, emoji: &str) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let (id, start) = {
            let mut feed = imp.feed.borrow_mut();
            let state = &mut feed.stories;
            let Some(viewer) = state.viewer.as_ref() else {
                return;
            };
            let id = viewer.watch.current().id.clone();
            let before = state.reactions.get(&id).cloned().unwrap_or_default();
            let Some(next) = before.toggled(emoji) else {
                viewer
                    .note
                    .set_label(&format!("You can react with up to {REACTION_LIMIT} emoji."));
                viewer.note.set_visible(true);
                drop(feed);
                self.draw_story_reactions();
                return;
            };
            viewer.note.set_visible(false);
            let start = state.writes.want(&id, &before, next.mine.clone());
            state.reactions.insert(id.clone(), next);
            (id, start)
        };
        self.draw_story_reactions();
        if !start {
            return;
        }
        let (window, generation) = (self.clone(), imp.generation.get());
        let author = imp.feed.borrow().stories.author_of(&id);
        glib::spawn_future_local(async move {
            loop {
                let Some(sent) = window.imp().feed.borrow_mut().stories.writes.next(&id) else {
                    break;
                };
                let result = session
                    .set_story_reactions(author.clone(), id.clone(), sent.clone())
                    .await;
                if window.imp().generation.get() != generation {
                    return;
                }
                let finish = window.imp().feed.borrow_mut().stories.writes.finished(
                    &id,
                    &sent,
                    result.is_ok(),
                );
                match finish {
                    Finish::WriteAgain => {}
                    Finish::Done => break,
                    Finish::Revert(confirmed) => {
                        window
                            .imp()
                            .feed
                            .borrow_mut()
                            .stories
                            .reactions
                            .insert(id.clone(), confirmed);
                        if let (Err(message), Some(viewer)) =
                            (result, window.imp().feed.borrow().stories.viewer.as_ref())
                        {
                            viewer
                                .note
                                .set_label(&format!("The reaction was not saved. {message}"));
                            viewer.note.set_visible(true);
                        }
                        window.draw_story_reactions();
                        break;
                    }
                }
            }
        });
    }

    // MARK: Sharing a story

    /// The dialog that shares a picture and a caption as a story.
    pub(super) fn open_story_composer(&self) {
        let imp = self.imp();
        if imp.session.borrow().is_none() {
            return;
        }
        let dialog = adw::Dialog::builder()
            .title("New Story")
            .content_width(420)
            .build();
        let toasts = adw::ToastOverlay::new();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());

        let preview = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .height_request(260)
            .visible(false)
            .alternative_text("Preview of the story")
            .build();
        let choose = gtk::Button::builder().label("Choose Picture…").build();
        choose.add_css_class("pill");
        choose.set_halign(gtk::Align::Center);
        let caption = gtk::Entry::builder()
            .placeholder_text("Add a caption (optional)")
            .build();
        let note = gtk::Label::builder()
            .label("Everyone in the community can see a story. It stays up for a day.")
            .wrap(true)
            .xalign(0.0)
            .build();
        note.add_css_class("dim-label");
        note.add_css_class("caption");
        let share = gtk::Button::builder()
            .label("Share Story")
            .sensitive(false)
            .build();
        share.add_css_class("suggested-action");
        share.add_css_class("pill");
        share.set_halign(gtk::Align::Center);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.set_margin_start(16);
        body.set_margin_end(16);
        body.set_margin_top(12);
        body.set_margin_bottom(16);
        for widget in [
            preview.upcast_ref::<gtk::Widget>(),
            choose.upcast_ref(),
            caption.upcast_ref(),
            note.upcast_ref(),
            share.upcast_ref(),
        ] {
            body.append(widget);
        }
        toolbar.set_content(Some(&body));
        toasts.set_child(Some(&toolbar));
        dialog.set_child(Some(&toasts));

        let chosen: Rc<RefCell<Option<Outgoing>>> = Rc::new(RefCell::new(None));
        let sending = Rc::new(std::cell::Cell::new(false));
        let window = self.downgrade();

        choose.connect_clicked({
            let (chosen, dialog, toasts, preview, share, window) = (
                chosen.clone(),
                dialog.clone(),
                toasts.clone(),
                preview.clone(),
                share.clone(),
                window.clone(),
            );
            move |_| {
                let Some(window) = window.upgrade() else {
                    return;
                };
                let (chosen, dialog, toasts, preview, share) = (
                    chosen.clone(),
                    dialog.clone(),
                    toasts.clone(),
                    preview.clone(),
                    share.clone(),
                );
                let chooser = picture_dialog("Choose a Picture");
                glib::spawn_future_local(async move {
                    let Ok(file) = chooser.open_future(Some(&window)).await else {
                        return;
                    };
                    let picture = match read_picture(&file).await {
                        Ok(picture) => picture,
                        Err(message) => return toasts.add_toast(adw::Toast::new(&message)),
                    };
                    match images::decode_off_main(picture.bytes.clone()).await {
                        Ok(decoded) => {
                            preview.set_paintable(Some(&decoded.texture()));
                            preview.set_visible(true);
                        }
                        Err(error) => {
                            return toasts.add_toast(adw::Toast::new(&format!(
                                "That picture cannot be shown: {error}."
                            )));
                        }
                    }
                    chosen.replace(Some(picture));
                    share.set_sensitive(true);
                    dialog.set_focus(Some(&share));
                });
            }
        });
        share.connect_clicked({
            let (chosen, dialog, toasts, choose, caption, sending) = (
                chosen.clone(),
                dialog.clone(),
                toasts.clone(),
                choose.clone(),
                caption.clone(),
                sending.clone(),
            );
            move |share| {
                let Some(window) = window.upgrade() else {
                    return;
                };
                let Some(session) = window.imp().session.borrow().clone() else {
                    return;
                };
                if sending.get() || !window.imp().connected.get() {
                    if !sending.get() {
                        toasts.add_toast(adw::Toast::new("Wait until you are connected."));
                    }
                    return;
                }
                let Some(picture) = chosen.borrow_mut().take() else {
                    return;
                };
                sending.set(true);
                share.set_sensitive(false);
                share.set_label("Sharing…");
                choose.set_sensitive(false);
                let text = stories::caption(&caption.text());
                let generation = window.imp().generation.get();
                let (chosen, dialog, toasts, choose, sending, share) = (
                    chosen.clone(),
                    dialog.clone(),
                    toasts.clone(),
                    choose.clone(),
                    sending.clone(),
                    share.clone(),
                );
                glib::spawn_future_local(async move {
                    let media_type = picture.media_type.clone();
                    let result = async {
                        let url = session
                            .upload_public(Outgoing {
                                name: picture.name.clone(),
                                media_type: picture.media_type.clone(),
                                bytes: picture.bytes.clone(),
                            })
                            .await
                            .map_err(|error| {
                                format!("The picture could not be uploaded: {error}.")
                            })?;
                        session.publish_story(url, media_type, text).await
                    }
                    .await;
                    if window.imp().generation.get() != generation {
                        return;
                    }
                    sending.set(false);
                    match result {
                        Ok(story) => {
                            window.story_shared(story);
                            dialog.close();
                        }
                        Err(message) => {
                            // Keep the picture for another try.
                            chosen.replace(Some(picture));
                            toasts.add_toast(adw::Toast::new(&message));
                            choose.set_sensitive(true);
                            share.set_sensitive(true);
                            share.set_label("Share Story");
                        }
                    }
                });
            }
        });
        dialog.present(Some(self));
    }

    /// A story this account just published: it shows first, already seen.
    fn story_shared(&self, story: Story) {
        {
            let mut feed = self.imp().feed.borrow_mut();
            feed.stories.reads.mark(&story.id, now_ms());
            stories::prepend(&mut feed.stories.items, story);
        }
        self.draw_story_rail();
        self.toast("Your story is up.");
        let window = self.clone();
        glib::spawn_future_local(async move { window.sync_story_reads().await });
    }
}

/// A rail item: the picture above its name.
fn item_column(top: &impl IsA<gtk::Widget>, name: &str) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    column.set_halign(gtk::Align::Center);
    column.append(top);
    let label = gtk::Label::builder()
        .label(name)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(9)
        .width_chars(9)
        .build();
    label.add_css_class("caption");
    column.append(&label);
    column
}

/// A round button on the edge of the picture.
fn edge_button(icon: &str, tooltip: &str, side: gtk::Align) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .halign(side)
        .valign(gtk::Align::Center)
        .margin_start(8)
        .margin_end(8)
        .build();
    super::a11y::label(&button, tooltip);
    button.add_css_class("osd");
    button.add_css_class("circular");
    button
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_start_with_a_capital() {
        assert_eq!(
            capitalise("the image could not be read"),
            "The image could not be read"
        );
        assert_eq!(capitalise(""), "");
    }
}
