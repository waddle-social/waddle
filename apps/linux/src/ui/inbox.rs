//! The Inbox destination: unread cards for rooms, DMs and threads.
//!
//! What to show and when to fetch is decided in `inbox.rs`; this draws it
//! and wires the sidebar row, the cards and Mark Read to the window.

use std::collections::{HashMap, HashSet};

use adw::prelude::{
    ActionRowExt, BoxExt, ButtonExt, Cast, NavigationPageExt, ObjectExt, WidgetExt,
};
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;

use super::{avatar, WaddleWindow};
use crate::inbox::{self, badge_text, CardPlan, InboxMessage, Loaded, Meta, SectionPlan, Sections};
use crate::session::{Conversation, Kind};
use crate::threads;

/// The window's overview state.
#[derive(Default)]
pub struct InboxState {
    open: bool,
    /// Newest inbox row of each conversation, by cache key.
    meta: HashMap<String, Meta>,
    /// Arrival times (epoch seconds) of live messages, by cache key.
    activity: HashMap<String, i64>,
    /// Conversations with an unread mention of this account.
    mentions: HashSet<String>,
    sections: Sections,
    badge: Option<gtk::Label>,
    row: Option<adw::ActionRow>,
    /// What the cards last showed, so an unchanged plan is not redrawn.
    drawn: Option<(Vec<CardPlan>, u64)>,
}

impl WaddleWindow {
    /// The sidebar's Inbox row, above the rooms.
    pub(super) fn setup_inbox(&self) {
        let imp = self.imp();
        let row = adw::ActionRow::builder()
            .title("Inbox")
            .title_lines(1)
            .activatable(true)
            .build();
        row.add_css_class("conversation");
        let icon = gtk::Image::from_icon_name("mail-unread-symbolic");
        icon.set_valign(gtk::Align::Center);
        row.add_prefix(&icon);
        let badge = gtk::Label::builder()
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        badge.add_css_class("unread-badge");
        badge.add_css_class("numeric");
        row.add_suffix(&badge);
        imp.inbox_list.append(&row);
        let mut state = imp.inbox.borrow_mut();
        state.badge = Some(badge);
        state.row = Some(row);
    }

    #[must_use]
    pub(super) fn inbox_open(&self) -> bool {
        self.imp().inbox.borrow().open
    }

    /// Forget the account's overview, on sign-out.
    pub(super) fn reset_inbox(&self) {
        let imp = self.imp();
        {
            let mut state = imp.inbox.borrow_mut();
            let (badge, row) = (state.badge.take(), state.row.take());
            *state = InboxState {
                badge,
                row,
                ..InboxState::default()
            };
        }
        if imp.inbox.borrow().open {
            imp.content_stack.set_visible_child_name("empty");
        }
        self.leave_inbox();
        self.refresh_inbox_badge(0);
        while let Some(child) = imp.inbox_cards.first_child() {
            imp.inbox_cards.remove(&child);
        }
    }

    /// Remember an inbox row's recency (a snapshot or a push).
    pub(super) fn inbox_note(&self, entry: &waddle_xmpp_client_ffi::WaddleInboxEntry) {
        let thread = threads::Thread::from_wire(entry.thread_id.clone(), None);
        let key = threads::conversation_key(&entry.partner, thread.as_ref());
        let mut state = self.imp().inbox.borrow_mut();
        if entry.unread == 0 {
            state.mentions.remove(&key);
        }
        state.meta.insert(
            key,
            Meta {
                last_updated: entry.last_updated,
                last_stanza_id: entry.last_stanza_id.clone(),
            },
        );
    }

    /// A live message was counted unread in `key`.
    pub(super) fn inbox_arrival(&self, key: &str, mentions_me: bool) {
        let mut state = self.imp().inbox.borrow_mut();
        state
            .activity
            .insert(key.to_string(), glib::real_time() / 1_000_000);
        if mentions_me {
            state.mentions.insert(key.to_string());
        }
    }

    /// The cards for what is unread now.
    fn inbox_plan(&self) -> Vec<CardPlan> {
        let imp = self.imp();
        let unread = imp.unread.borrow();
        let mut state = imp.inbox.borrow_mut();
        state
            .mentions
            .retain(|key| unread.get(key).is_some_and(|count| *count > 0));
        inbox::plan(
            &imp.conversations.borrow(),
            &unread,
            &state.mentions,
            &state.meta,
            &state.activity,
        )
    }

    /// Unread counts changed: update the badge, and the cards if shown.
    pub(super) fn inbox_changed(&self) {
        let plan = self.inbox_plan();
        self.refresh_inbox_badge(inbox::total_unread(&plan));
        if self.inbox_open() {
            self.refresh_inbox_view_with(plan);
        }
    }

    fn refresh_inbox_badge(&self, total: u32) {
        let state = self.imp().inbox.borrow();
        if let Some(badge) = state.badge.as_ref() {
            badge.set_visible(total > 0);
            badge.set_label(&badge_text(total));
        }
        if let Some(row) = state.row.as_ref() {
            row.set_tooltip_text(Some(&match total {
                0 => "No unread messages".to_string(),
                1 => "1 unread message".to_string(),
                total => format!("{total} unread messages"),
            }));
        }
    }

    pub(super) fn show_inbox(&self) {
        let imp = self.imp();
        {
            let mut state = imp.inbox.borrow_mut();
            state.open = true;
            state.drawn = None;
        }
        // Nothing is being read while the overview is on screen.
        imp.current.replace(None);
        self.render_conversations();
        if let Some(row) = imp.inbox.borrow().row.as_ref() {
            imp.inbox_list.select_row(Some(row));
        }
        imp.room_title.set_title("Inbox");
        imp.room_page.set_title("Inbox");
        imp.thread_back.set_visible(false);
        imp.load_earlier.set_visible(false);
        imp.content_stack.set_visible_child_name("inbox");
        if let Some(composer) = imp.composer.parent() {
            composer.set_visible(false);
        }
        self.cancel_compose();
        self.refresh_composer();
        self.refresh_notify_button();
        self.refresh_message_tools();
        self.refresh_thread_context();
        imp.split_view.set_show_content(true);
        self.inbox_changed();
    }

    /// A conversation is opening: the overview gives way to it.
    pub(super) fn leave_inbox(&self) {
        let imp = self.imp();
        let was_open = std::mem::take(&mut imp.inbox.borrow_mut().open);
        if !was_open {
            return;
        }
        imp.inbox_list.unselect_all();
        if let Some(composer) = imp.composer.parent() {
            composer.set_visible(true);
        }
    }

    fn open_inbox_target(&self, key: &str, target: Option<&str>) {
        self.open_conversation(key);
        if let Some(target) = target {
            if self.current_conversation().is_some_and(|c| c.key() == key) {
                self.jump_to(target);
            }
        }
    }

    pub(super) fn mark_inbox_read(&self, conversations: &[Conversation]) {
        for conversation in conversations {
            self.mark_conversation_read(&conversation.key());
        }
    }

    pub(super) fn mark_all_inbox_read(&self) {
        let plan = self.inbox_plan();
        let conversations: Vec<Conversation> =
            plan.iter().flat_map(CardPlan::conversations).collect();
        self.mark_inbox_read(&conversations);
    }

    fn refresh_inbox_view_with(&self, plan: Vec<CardPlan>) {
        let imp = self.imp();
        let due: Vec<SectionPlan> = {
            let mut state = imp.inbox.borrow_mut();
            let wanted: HashSet<String> = plan
                .iter()
                .flat_map(CardPlan::sections)
                .map(|section| section.conversation.key())
                .collect();
            state.sections.retain(&wanted);
            if imp.connected.get() {
                state
                    .sections
                    .due(plan.iter().flat_map(CardPlan::sections))
                    .into_iter()
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            }
        };
        self.fetch_inbox_sections(due);
        let version = imp.inbox.borrow().sections.version();
        let unchanged = imp
            .inbox
            .borrow()
            .drawn
            .as_ref()
            .is_some_and(|(drawn, drawn_version)| *drawn == plan && *drawn_version == version);
        if unchanged {
            return;
        }
        self.draw_inbox(&plan);
        imp.inbox.borrow_mut().drawn = Some((plan, version));
    }

    fn refresh_inbox_view(&self) {
        let plan = self.inbox_plan();
        self.refresh_inbox_view_with(plan);
    }

    fn fetch_inbox_sections(&self, due: Vec<SectionPlan>) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get();
        for section in due {
            let (window, session) = (self.clone(), session.clone());
            glib::spawn_future_local(async move {
                let loaded = match session.inbox_section(&section).await {
                    Ok(rows) => Loaded::Ready(rows),
                    Err(_) => Loaded::Failed,
                };
                if window.imp().generation.get() != generation {
                    return;
                }
                let key = section.conversation.key();
                window
                    .imp()
                    .inbox
                    .borrow_mut()
                    .sections
                    .finish(&key, &section.signature(), loaded);
                if window.inbox_open() {
                    window.refresh_inbox_view();
                }
            });
        }
    }

    fn draw_inbox(&self, plan: &[CardPlan]) {
        let imp = self.imp();
        let cards = &imp.inbox_cards;
        while let Some(child) = cards.first_child() {
            cards.remove(&child);
        }
        imp.inbox_mark_all.set_sensitive(!plan.is_empty());
        if plan.is_empty() {
            let empty = adw::StatusPage::builder()
                .icon_name("emblem-ok-symbolic")
                .title("All Caught Up")
                .description("There is nothing unread.")
                .vexpand(true)
                .build();
            cards.append(&empty);
            return;
        }
        for card in plan {
            cards.append(&self.card_widget(card));
        }
    }

    fn card_widget(&self, card: &CardPlan) -> gtk::Box {
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 4);
        outer.add_css_class("card");
        outer.add_css_class("inbox-card");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let title = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        title.append(&card_icon(card));
        let name = gtk::Label::builder()
            .label(match card.kind {
                Kind::Room => format!("# {}", card.title),
                Kind::Direct => card.title.clone(),
            })
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        name.add_css_class("heading");
        title.append(&name);
        let open = gtk::Button::builder().child(&title).hexpand(true).build();
        open.add_css_class("flat");
        open.set_tooltip_text(Some("Open this conversation"));
        let (window, key) = (self.downgrade(), main_key(card));
        open.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.open_inbox_target(&key, None);
            }
        });
        header.append(&open);
        if card.mention() {
            header.append(&mention_badge());
        }
        header.append(&count_badge(card.unread()));
        let mark = gtk::Button::builder().label("Mark Read").build();
        mark.add_css_class("flat");
        mark.set_valign(gtk::Align::Center);
        let (window, conversations) = (self.downgrade(), card.conversations());
        mark.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.mark_inbox_read(&conversations);
            }
        });
        header.append(&mark);
        outer.append(&header);

        if let Some(feed) = card.feed.as_ref() {
            outer.append(&self.section_widget(feed, None));
        }
        for thread in &card.threads {
            outer.append(&self.section_widget(thread, Some(&thread.conversation.name)));
        }
        outer
    }

    /// A feed's or a thread's unread messages (a thread under its title).
    fn section_widget(&self, section: &SectionPlan, thread_title: Option<&str>) -> gtk::Box {
        let key = section.conversation.key();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 2);
        if let Some(title) = thread_title {
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let label = gtk::Label::builder()
                .label(format!("Thread: {title}"))
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            label.add_css_class("dim-label");
            label.add_css_class("heading");
            heading.append(&label);
            if section.mention {
                heading.append(&mention_badge());
            }
            heading.append(&count_badge(section.unread));
            let open = gtk::Button::builder().child(&heading).build();
            open.add_css_class("flat");
            open.add_css_class("inbox-thread-heading");
            open.set_tooltip_text(Some("Open this thread"));
            let (window, open_key) = (self.downgrade(), key.clone());
            open.connect_clicked(move |_| {
                if let Some(window) = window.upgrade() {
                    window.open_inbox_target(&open_key, None);
                }
            });
            body.append(&open);
        }
        let loaded = {
            let state = self.imp().inbox.borrow();
            state.sections.get(&key, &section.signature()).cloned()
        };
        match loaded {
            None => {
                let spinner = adw::Spinner::builder()
                    .width_request(16)
                    .height_request(16)
                    .halign(gtk::Align::Start)
                    .margin_start(10)
                    .margin_top(6)
                    .margin_bottom(6)
                    .build();
                body.append(&spinner);
            }
            Some(Loaded::Failed) => {
                let failed = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                failed.set_margin_start(10);
                let note = gtk::Label::builder()
                    .label("The unread messages could not be loaded.")
                    .xalign(0.0)
                    .build();
                note.add_css_class("dim-label");
                failed.append(&note);
                let retry = gtk::Button::builder().label("Retry").build();
                retry.add_css_class("flat");
                let (window, retry_key) = (self.downgrade(), key.clone());
                retry.connect_clicked(move |_| {
                    if let Some(window) = window.upgrade() {
                        window.imp().inbox.borrow_mut().sections.forget(&retry_key);
                        window.refresh_inbox_view();
                    }
                });
                failed.append(&retry);
                body.append(&failed);
            }
            Some(Loaded::Ready(rows)) if rows.is_empty() => {
                let note = gtk::Label::builder()
                    .label("Open the conversation to catch up.")
                    .xalign(0.0)
                    .margin_start(10)
                    .build();
                note.add_css_class("dim-label");
                body.append(&note);
            }
            Some(Loaded::Ready(rows)) => {
                for row in &rows {
                    body.append(&self.message_widget(&key, row));
                }
            }
        }
        body
    }

    fn message_widget(&self, key: &str, row: &InboxMessage) -> gtk::Button {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let sender = gtk::Label::builder()
            .label(&row.sender)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        sender.add_css_class("heading");
        top.append(&sender);
        if row.mentions_me {
            top.append(&mention_badge());
        }
        let time = gtk::Label::builder()
            .label(when(row.timestamp_ms))
            .hexpand(true)
            .xalign(0.0)
            .build();
        time.add_css_class("dim-label");
        time.add_css_class("caption");
        top.append(&time);
        content.append(&top);
        let text = gtk::Label::builder()
            .label(&row.body)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(3)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        content.append(&text);
        let button = gtk::Button::builder().child(&content).build();
        button.add_css_class("flat");
        button.add_css_class("inbox-message");
        button.set_tooltip_text(Some("Open this message"));
        let (window, key, target) = (self.downgrade(), key.to_string(), row.target.clone());
        button.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.open_inbox_target(&key, Some(&target));
            }
        });
        button
    }
}

/// The key of the card's main conversation (its thread-less entry).
fn main_key(card: &CardPlan) -> String {
    threads::conversation_key(&card.jid, None)
}

fn card_icon(card: &CardPlan) -> gtk::Widget {
    let icon: gtk::Widget = match card.kind {
        Kind::Room => {
            let tile = gtk::Label::builder()
                .label("#")
                .width_request(28)
                .height_request(28)
                .build();
            tile.add_css_class("room-tile");
            tile.add_css_class("inbox-tile");
            tile.upcast()
        }
        Kind::Direct => {
            let avatar = avatar::avatar(28);
            avatar::set_name(&avatar, &card.title);
            avatar.upcast()
        }
    };
    icon.set_valign(gtk::Align::Center);
    icon
}

fn count_badge(count: u32) -> gtk::Label {
    let badge = gtk::Label::builder()
        .label(badge_text(count))
        .valign(gtk::Align::Center)
        .build();
    badge.add_css_class("unread-badge");
    badge.add_css_class("numeric");
    badge.set_tooltip_text(Some("Unread"));
    badge
}

fn mention_badge() -> gtk::Label {
    let badge = gtk::Label::builder()
        .label("@")
        .valign(gtk::Align::Center)
        .build();
    badge.add_css_class("mention-badge");
    badge.set_tooltip_text(Some("You were mentioned"));
    badge
}

fn when(ms: i64) -> String {
    glib::DateTime::from_unix_local(ms.div_euclid(1000))
        .ok()
        .and_then(|time| time.format("%b %-d, %H:%M").ok())
        .map(|text| text.to_string())
        .unwrap_or_default()
}
