//! Conversations (rooms and DMs), the timeline and the composer.
//!
//! Each conversation's messages live in a `waddle_client_session::Timeline`;
//! this module applies the diffs it returns to a `gio::ListStore` behind a
//! virtualised `ListView`, so only changed rows are touched. Message
//! actions (react, reply, edit, delete) are in `ui/actions.rs`.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use waddle_client_session::{Delivery, Reaction, Timeline, TimelineDiff, TimelineItem};

use super::{avatar, WaddleWindow};
use crate::auth::store::StoredAccount;
use crate::session::{self, Arrival, Conversation, Kind, Session, SessionEvent};

/// One row in the list: the core's item plus where it sits.
#[derive(Clone)]
pub(super) struct Row {
    pub item: TimelineItem,
    pub layout: Layout,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Layout {
    /// Starts a group: shows the avatar, sender and time.
    pub header: bool,
    /// Starts a local day: shows the date above.
    pub day: bool,
}

impl Layout {
    /// Until `refresh_headers` places the row.
    const UNPLACED: Self = Self {
        header: true,
        day: false,
    };

    fn of(timeline: &Timeline, at: usize, utc_offset_ms: i64) -> Self {
        let items = timeline.items();
        let local_day = |item: &TimelineItem| {
            (item.timestamp_ms + utc_offset_ms).div_euclid(24 * 60 * 60 * 1000)
        };
        Self {
            header: !timeline.continues_group(at, utc_offset_ms),
            day: match (
                at.checked_sub(1).and_then(|at| items.get(at)),
                items.get(at),
            ) {
                (Some(previous), Some(item)) => local_day(previous) != local_day(item),
                (None, Some(_)) => true,
                _ => false,
            },
        }
    }
}

/// How long to wait for `Connected` before treating a connect as failed.
const CONNECT_BUDGET_SECS: u32 = 20;
/// How long to gather messages read in the open conversation before
/// telling the server (XEP-0430 mark-read).
const READ_DEBOUNCE_SECS: u32 = 2;

impl WaddleWindow {
    pub(super) fn setup_timeline(&self) {
        let imp = self.imp();
        let rows = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        let window = self.downgrade();
        factory.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let widget = row_widget();
            attach_action_gestures(&widget, item, &window);
            item.set_child(Some(&widget));
            item.set_activatable(false);
            item.set_selectable(false);
        });
        let window = self.downgrade();
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let (Some(child), Some(object), Some(window)) = (
                item.child(),
                item.item().and_downcast::<glib::BoxedAnyObject>(),
                window.upgrade(),
            ) else {
                return;
            };
            bind_row(&window.downgrade(), &child, &object.borrow::<Row>());
        });
        factory.connect_unbind(|_, item| {
            if let Some(parts) = item
                .downcast_ref::<gtk::ListItem>()
                .and_then(|item| item.child())
                .and_then(|child| row_parts(&child))
            {
                super::media::bind(&parts.media, "", true);
            }
        });
        imp.timeline_view
            .set_model(Some(&gtk::NoSelection::new(Some(rows.clone()))));
        imp.timeline_view.set_factory(Some(&factory));
        imp.rows.replace(Some(rows));

        // Page older history in when the user nears the top.
        let window = self.clone();
        imp.timeline_scroller
            .vadjustment()
            .connect_value_changed(move |adjustment| {
                if adjustment.value() < 200.0 {
                    window.load_older();
                }
            });

        // "Rooms" and "Direct Messages" above the first row of each kind.
        let window = self.downgrade();
        imp.room_list.set_header_func(move |row, before| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let kind_at = |row: &gtk::ListBoxRow| {
                window
                    .conversation_at(row.index())
                    .map(|conversation| conversation.kind)
            };
            let kind = kind_at(row);
            let starts_section = kind.is_some() && before.and_then(kind_at) != kind;
            match kind.filter(|_| starts_section) {
                Some(kind) => row.set_header(Some(&section_header(kind))),
                None => row.set_header(None::<&gtk::Widget>),
            }
        });

        // Coming back to the window reads the open conversation.
        self.connect_is_active_notify(|window| {
            if window.is_active() {
                window.read_current();
            }
        });
        self.setup_composer();
    }

    pub(super) fn start_session(&self, account: &StoredAccount) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get().wrapping_add(1));
        let generation = imp.generation.get();
        imp.account_status.set_title("Connecting…");
        imp.content_stack.set_visible_child_name("empty");

        imp.connected.set(false);
        self.clear_membership();
        let (session, events) = Session::start(account);
        imp.session.replace(Some(session));
        self.watch_connect(generation);

        let window = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(event) = events.recv().await {
                if window.imp().generation.get() != generation {
                    break;
                }
                window.handle(event, generation).await;
            }
        });
    }

    pub(super) async fn stop_session(&self) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get().wrapping_add(1));
        if let Some(session) = imp.session.take() {
            session.disconnect().await;
        }
        imp.connected.set(false);
        self.clear_membership();
        imp.conversations.replace(Vec::new());
        imp.timelines.replace(Default::default());
        imp.unread.replace(Default::default());
        imp.current.replace(None);
        imp.read_pending.replace(Default::default());
        imp.loading_history.replace(Default::default());
        self.cancel_compose();
        if let Some(rows) = imp.rows.borrow().as_ref() {
            rows.remove_all();
        }
        while let Some(row) = imp.room_list.first_child() {
            imp.room_list.remove(&row);
        }
        imp.composer.set_sensitive(false);
    }

    /// Retry when a connect attempt neither connects nor reports anything.
    fn watch_connect(&self, generation: u64) {
        let window = self.clone();
        glib::timeout_add_seconds_local_once(CONNECT_BUDGET_SECS, move || {
            let imp = window.imp();
            if imp.generation.get() == generation && !imp.connected.get() {
                window.schedule_reconnect(generation);
            }
        });
    }

    fn schedule_reconnect(&self, generation: u64) {
        let imp = self.imp();
        let attempt = imp.reconnect_attempt.get();
        imp.reconnect_attempt.set(attempt.saturating_add(1));
        let delay = session::reconnect_delay(attempt, glib::random_double());
        imp.account_status.set_title("Reconnecting…");
        imp.composer.set_sensitive(false);
        let window = self.clone();
        glib::timeout_add_local_once(delay, move || {
            let imp = window.imp();
            if imp.generation.get() != generation || imp.connected.get() {
                return;
            }
            if let Some(session) = imp.session.borrow().as_ref() {
                session.connect();
            }
            window.watch_connect(generation);
        });
    }

    async fn handle(&self, event: SessionEvent, generation: u64) {
        let imp = self.imp();
        match event {
            SessionEvent::Connected => {
                imp.connected.set(true);
                imp.reconnect_attempt.set(0);
                self.on_connected().await;
            }
            SessionEvent::Disconnected => {
                imp.connected.set(false);
                self.clear_membership();
                self.schedule_reconnect(generation);
            }
            SessionEvent::AuthenticationFailed => {
                self.toast("Your session has expired. Sign in again.");
                self.sign_out_now();
            }
            SessionEvent::Message(message) => {
                let Some(me) = imp
                    .session
                    .borrow()
                    .as_ref()
                    .map(|session| session.me.clone())
                else {
                    return;
                };
                if let Some(arrival) = session::arrival(*message, &me) {
                    self.on_arrival(arrival);
                }
            }
            SessionEvent::Inbox(entry) => {
                if entry.thread_id.is_some() {
                    return;
                }
                if entry.kind == "direct" {
                    self.ensure_direct(&entry.partner);
                }
                if !self.is_reading(&entry.partner) {
                    imp.unread.borrow_mut().insert(entry.partner, entry.unread);
                    self.render_conversations();
                }
            }
            SessionEvent::Presence(presence) => self.on_room_presence(*presence),
            SessionEvent::Rejected { id, from, to } => self.on_message_rejected(id, from, to),
            SessionEvent::DeliveryAcked(id) => self.set_delivery(&id, Delivery::Sent),
            SessionEvent::DeliveryFailed(id) => self.set_delivery(&id, Delivery::Failed),
        }
    }

    fn on_arrival(&self, arrival: Arrival) {
        let imp = self.imp();
        let Arrival {
            conversation,
            kind,
            message,
            mentions_me,
            delayed,
        } = arrival;
        if kind == Kind::Direct {
            self.ensure_direct(&conversation);
        }
        let notice = (!message.own && !is_fastening(&message))
            .then(|| (message.sender.clone(), message.body.clone()))
            .and_then(|(sender, body)| Some((sender, body?)));
        let diffs = imp
            .timelines
            .borrow_mut()
            .entry(conversation.clone())
            .or_default()
            .apply_live(message);
        let added = diffs
            .iter()
            .any(|diff| matches!(diff, TimelineDiff::Append { .. }));

        if imp.current.borrow().as_deref() == Some(conversation.as_str()) {
            let stick = self.near_bottom();
            self.apply(&diffs);
            if stick {
                self.scroll_to_end();
            }
        }
        // A message held while away is already in the inbox's unread
        // count, and too old to announce.
        let Some((sender, body)) = notice.filter(|_| added && !delayed) else {
            return;
        };
        if self.is_reading(&conversation) {
            self.queue_read(&conversation);
            return;
        }
        // Unread even when open, if the user is not looking: coming back
        // then marks it read on the server too.
        *imp.unread
            .borrow_mut()
            .entry(conversation.clone())
            .or_default() += 1;
        self.render_conversations();
        if kind == Kind::Direct || mentions_me {
            self.notify(&conversation, kind, &sender, &body);
        }
    }

    async fn on_connected(&self) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        imp.account_status.set_title("Choose a conversation");
        session.announce();
        let rooms = session.rooms().await;
        let inbox = session.inbox().await;
        for room in rooms.iter().filter(|room| room.autojoin) {
            self.ensure_room_joined(room);
        }
        let mut conversations = rooms;
        for direct in session::direct_from(&inbox) {
            if !conversations.iter().any(|known| known.jid == direct.jid) {
                conversations.push(direct);
            }
        }
        // Keep DMs that arrived live but are not in the inbox yet.
        for known in imp.conversations.borrow().iter() {
            if known.kind == Kind::Direct && !conversations.iter().any(|c| c.jid == known.jid) {
                conversations.push(known.clone());
            }
        }
        imp.conversations.replace(conversations);
        {
            let mut unread = imp.unread.borrow_mut();
            for entry in inbox.iter().filter(|entry| entry.thread_id.is_none()) {
                unread.insert(entry.partner.clone(), entry.unread);
            }
        }
        self.render_conversations();
        self.refresh_composer();

        // Reopen what was on screen (or asked for from a notification), or
        // the first room on first connect.
        let target = imp
            .pending_open
            .take()
            .or_else(|| imp.current.borrow().clone())
            .or_else(|| {
                imp.conversations
                    .borrow()
                    .first()
                    .map(|conversation| conversation.jid.clone())
            });
        if let Some(jid) = target {
            self.open_conversation(&jid);
        }
    }

    /// Add a DM conversation the sidebar does not show yet.
    fn ensure_direct(&self, peer: &str) {
        let imp = self.imp();
        if imp
            .conversations
            .borrow()
            .iter()
            .any(|known| known.jid == peer)
        {
            return;
        }
        if let Some(direct) = session::direct_conversation(peer) {
            imp.conversations.borrow_mut().push(direct);
            self.render_conversations();
        }
    }

    fn render_conversations(&self) {
        let imp = self.imp();
        while let Some(row) = imp.room_list.first_child() {
            imp.room_list.remove(&row);
        }
        let current = imp.current.borrow().clone();
        let unread = imp.unread.borrow();
        for conversation in imp.conversations.borrow().iter() {
            let unread = unread.get(&conversation.jid).copied().unwrap_or(0);
            let row = conversation_row(conversation, unread);
            imp.room_list.append(&row);
            if current.as_deref() == Some(conversation.jid.as_str()) {
                imp.room_list.select_row(Some(&row));
            }
        }
    }

    pub(super) fn conversation_at(&self, index: i32) -> Option<Conversation> {
        let index = usize::try_from(index).ok()?;
        self.imp().conversations.borrow().get(index).cloned()
    }

    pub(super) fn current_conversation(&self) -> Option<Conversation> {
        let imp = self.imp();
        let current = imp.current.borrow().clone()?;
        imp.conversations
            .borrow()
            .iter()
            .find(|conversation| conversation.jid == current)
            .cloned()
    }

    /// Open a room or DM, or remember it for when the session has loaded.
    pub fn open_conversation(&self, jid: &str) {
        let imp = self.imp();
        let Some(conversation) = imp
            .conversations
            .borrow()
            .iter()
            .find(|conversation| conversation.jid == jid)
            .cloned()
        else {
            imp.pending_open.replace(Some(jid.to_string()));
            return;
        };
        let switching = imp.current.borrow().as_deref() != Some(jid);
        imp.current.replace(Some(conversation.jid.clone()));
        if switching {
            self.cancel_compose();
        }
        let title = sidebar_title(&conversation);
        imp.room_title.set_title(&title);
        imp.room_page.set_title(&conversation.name);
        imp.composer_entry
            .set_placeholder_text(Some(&format!("Message {title}")));
        imp.content_stack.set_visible_child_name("timeline");
        self.ensure_room_joined(&conversation);
        self.refresh_composer();
        imp.split_view.set_show_content(true);
        self.read_current();
        self.render_conversations();

        let needs_history = imp
            .timelines
            .borrow()
            .get(&conversation.jid)
            .is_none_or(|timeline| timeline.older_cursor().is_none());
        self.rebuild_rows();
        self.scroll_to_end();
        if needs_history {
            self.load_history(conversation, None);
        }
    }

    /// Whether the user is looking at this conversation right now.
    fn is_reading(&self, jid: &str) -> bool {
        self.is_active() && self.is_visible() && self.imp().current.borrow().as_deref() == Some(jid)
    }

    /// Clear the open conversation's unread state, here and on the server.
    fn read_current(&self) {
        let imp = self.imp();
        let Some(jid) = imp.current.borrow().clone() else {
            return;
        };
        if !self.is_active() {
            return;
        }
        if let Some(app) = self.application() {
            app.withdraw_notification(&notification_id(&jid));
        }
        let had_unread = imp
            .unread
            .borrow_mut()
            .remove(&jid)
            .is_some_and(|count| count > 0);
        if had_unread {
            self.queue_read(&jid);
            self.render_conversations();
        }
    }

    /// Tell the server this conversation is read, a moment from now, once
    /// for however many messages arrive meanwhile.
    fn queue_read(&self, jid: &str) {
        let imp = self.imp();
        if !imp.read_pending.borrow_mut().insert(jid.to_string()) {
            return;
        }
        let window = self.clone();
        glib::timeout_add_seconds_local_once(READ_DEBOUNCE_SECS, move || {
            let imp = window.imp();
            let pending: Vec<String> = imp.read_pending.borrow_mut().drain().collect();
            let Some(session) = imp.session.borrow().clone() else {
                return;
            };
            for jid in pending {
                session.mark_read(&jid);
            }
        });
    }

    fn notify(&self, jid: &str, kind: Kind, sender: &str, body: &str) {
        let Some(app) = self.application() else {
            return;
        };
        let title = match kind {
            Kind::Direct => sender.to_string(),
            Kind::Room => {
                let room = self
                    .imp()
                    .conversations
                    .borrow()
                    .iter()
                    .find(|conversation| conversation.jid == jid)
                    .map(|conversation| conversation.name.clone())
                    .unwrap_or_default();
                format!("{sender} in #{room}")
            }
        };
        let notification = gio::Notification::new(&title);
        notification.set_body(Some(body));
        notification
            .set_default_action_and_target_value("app.open-conversation", Some(&jid.to_variant()));
        app.send_notification(Some(&notification_id(jid)), &notification);
    }

    fn load_older(&self) {
        let imp = self.imp();
        let Some(conversation) = self.current_conversation() else {
            return;
        };
        let cursor = {
            let timelines = imp.timelines.borrow();
            let Some(timeline) = timelines.get(&conversation.jid) else {
                return;
            };
            if timeline.history_complete() {
                return;
            }
            timeline.older_cursor().map(str::to_string)
        };
        if cursor.is_some() {
            self.load_history(conversation, cursor);
        }
    }

    fn load_history(&self, conversation: Conversation, before: Option<String>) {
        let imp = self.imp();
        if !imp
            .loading_history
            .borrow_mut()
            .insert(conversation.jid.clone())
        {
            return;
        }
        let Some(session) = imp.session.borrow().clone() else {
            imp.loading_history.borrow_mut().remove(&conversation.jid);
            return;
        };
        let initial = before.is_none();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let (messages, first_id, complete) = session
                .history(&conversation.jid, conversation.kind, before)
                .await;
            let imp = window.imp();
            imp.loading_history.borrow_mut().remove(&conversation.jid);
            let diffs = imp
                .timelines
                .borrow_mut()
                .entry(conversation.jid.clone())
                .or_default()
                .apply_history_page(messages, first_id, complete);
            if imp.current.borrow().as_deref() != Some(conversation.jid.as_str()) {
                return;
            }
            let pushed = diffs.iter().find_map(|diff| match diff {
                TimelineDiff::PushFront { items } => Some(items.len()),
                _ => None,
            });
            window.apply(&diffs);
            if initial {
                window.scroll_to_end();
            } else if let Some(pushed) = pushed {
                // Keep the row the user was reading in place.
                window.scroll_to(pushed);
            }
        });
    }

    fn set_delivery(&self, id: &str, delivery: Delivery) {
        let imp = self.imp();
        let current = imp.current.borrow().clone();
        let mut current_diffs = Vec::new();
        for (jid, timeline) in imp.timelines.borrow_mut().iter_mut() {
            let diffs = timeline.set_delivery(id, delivery);
            if current.as_deref() == Some(jid.as_str()) {
                current_diffs.extend(diffs);
            }
        }
        self.apply(&current_diffs);
    }

    fn sign_out_now(&self) {
        if let Some(action) = self.lookup_action("sign-out") {
            action.activate(None);
        }
    }

    /// Run `edit` on this conversation's timeline, and show its diffs if
    /// the conversation is open. Async results name the conversation they
    /// started in, so a switch meanwhile cannot misdirect them.
    pub(super) fn edit_conversation(
        &self,
        jid: &str,
        edit: impl FnOnce(&mut Timeline) -> Vec<TimelineDiff>,
    ) {
        let imp = self.imp();
        let diffs = edit(
            imp.timelines
                .borrow_mut()
                .entry(jid.to_string())
                .or_default(),
        );
        if imp.current.borrow().as_deref() == Some(jid) {
            self.apply(&diffs);
        }
    }

    /// Bring the list store in step with the open conversation's timeline.
    pub(super) fn apply(&self, diffs: &[TimelineDiff]) {
        let imp = self.imp();
        let Some(rows) = imp.rows.borrow().clone() else {
            return;
        };
        let mut touched: Vec<usize> = Vec::new();
        for diff in diffs {
            match diff {
                TimelineDiff::PushFront { items } => {
                    let objects: Vec<glib::BoxedAnyObject> = items
                        .iter()
                        .map(|item| boxed(item.clone(), Layout::UNPLACED))
                        .collect();
                    rows.splice(0, 0, &objects);
                    touched.iter_mut().for_each(|at| *at += items.len());
                    touched.extend(0..=items.len());
                }
                TimelineDiff::Append { items } => {
                    let start = rows.n_items() as usize;
                    let objects: Vec<glib::BoxedAnyObject> = items
                        .iter()
                        .map(|item| boxed(item.clone(), Layout::UNPLACED))
                        .collect();
                    rows.extend_from_slice(&objects);
                    touched.extend(start..start + items.len());
                }
                TimelineDiff::Set { index, item } => {
                    let at = *index as usize;
                    rows.splice(*index, 1, &[boxed((**item).clone(), Layout::UNPLACED)]);
                    touched.extend([at, at + 1]);
                }
            }
        }
        self.refresh_headers(&rows, touched);
    }

    /// Recompute the group header for these rows from the core.
    fn refresh_headers(&self, rows: &gio::ListStore, mut touched: Vec<usize>) {
        let imp = self.imp();
        let Some(jid) = imp.current.borrow().clone() else {
            return;
        };
        let timelines = imp.timelines.borrow();
        let Some(timeline) = timelines.get(&jid) else {
            return;
        };
        let offset = utc_offset_ms();
        touched.sort_unstable();
        touched.dedup();
        for at in touched {
            let Some(item) = timeline.items().get(at) else {
                continue;
            };
            let layout = Layout::of(timeline, at, offset);
            let Ok(position) = u32::try_from(at) else {
                continue;
            };
            let current = rows
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
                .map(|object| object.borrow::<Row>().layout);
            if current != Some(layout) {
                rows.splice(position, 1, &[boxed(item.clone(), layout)]);
            }
        }
    }

    fn rebuild_rows(&self) {
        let imp = self.imp();
        let Some(rows) = imp.rows.borrow().clone() else {
            return;
        };
        rows.remove_all();
        let Some(jid) = imp.current.borrow().clone() else {
            return;
        };
        let timelines = imp.timelines.borrow();
        let empty = Timeline::new();
        let timeline = timelines.get(&jid).unwrap_or(&empty);
        let offset = utc_offset_ms();
        let objects: Vec<glib::BoxedAnyObject> = timeline
            .items()
            .iter()
            .enumerate()
            .map(|(at, item)| boxed(item.clone(), Layout::of(timeline, at, offset)))
            .collect();
        rows.extend_from_slice(&objects);
    }

    fn near_bottom(&self) -> bool {
        let adjustment = self.imp().timeline_scroller.vadjustment();
        adjustment.upper() - adjustment.page_size() - adjustment.value() < 120.0
    }

    pub(super) fn scroll_to_end(&self) {
        let imp = self.imp();
        let count = imp
            .rows
            .borrow()
            .as_ref()
            .map_or(0, |rows| rows.n_items() as usize);
        if count > 0 {
            self.scroll_to(count - 1);
        }
    }

    fn scroll_to(&self, index: usize) {
        let Ok(index) = u32::try_from(index) else {
            return;
        };
        let view = self.imp().timeline_view.clone();
        // After the list has laid out the new rows.
        glib::idle_add_local_once(move || {
            view.scroll_to(index, gtk::ListScrollFlags::NONE, None);
        });
    }
}

/// The GNotification id for a conversation, so a newer message replaces
/// the older notification and opening the conversation withdraws it.
pub fn notification_id(jid: &str) -> String {
    format!("conversation:{jid}")
}

fn is_fastening(message: &waddle_client_session::IncomingMessage) -> bool {
    message.reaction.is_some()
        || message.replaces_id.is_some()
        || message.retracts_id.is_some()
        || message.moderates_id.is_some()
}

fn sidebar_title(conversation: &Conversation) -> String {
    match conversation.kind {
        Kind::Room => format!("# {}", conversation.name),
        Kind::Direct => conversation.name.clone(),
    }
}

/// A room's `#` tile or a DM's avatar, the name, and any unread count.
fn conversation_row(conversation: &Conversation, unread: u32) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(conversation.name.as_str())
        .title_lines(1)
        .activatable(true)
        .build();
    row.add_css_class("conversation");
    let icon: gtk::Widget = match conversation.kind {
        Kind::Room => {
            let tile = gtk::Label::builder()
                .label("#")
                .width_request(28)
                .height_request(28)
                .build();
            tile.add_css_class("room-tile");
            tile.upcast()
        }
        Kind::Direct => {
            let avatar = avatar::avatar(28);
            avatar::set_name(&avatar, &conversation.name);
            avatar.upcast()
        }
    };
    icon.set_valign(gtk::Align::Center);
    row.add_prefix(&icon);
    if unread > 0 {
        let badge = gtk::Label::builder()
            .label(if unread > 99 {
                "99+".to_string()
            } else {
                unread.to_string()
            })
            .valign(gtk::Align::Center)
            .build();
        badge.add_css_class("unread-badge");
        badge.add_css_class("numeric");
        row.add_suffix(&badge);
        row.add_css_class("unread");
    }
    row
}

fn section_header(kind: Kind) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(match kind {
            Kind::Room => "Rooms",
            Kind::Direct => "Direct Messages",
        })
        .xalign(0.0)
        .margin_start(12)
        .margin_top(12)
        .margin_bottom(6)
        .build();
    label.add_css_class("heading");
    label.add_css_class("dim-label");
    label
}

fn boxed(item: TimelineItem, layout: Layout) -> glib::BoxedAnyObject {
    glib::BoxedAnyObject::new(Row { item, layout })
}

fn utc_offset_ms() -> i64 {
    glib::DateTime::now_local()
        .map(|now| now.utc_offset().as_milliseconds())
        .unwrap_or(0)
}

/// Right-click and long-press open the message's actions.
fn attach_action_gestures(
    widget: &gtk::Box,
    item: &gtk::ListItem,
    window: &glib::WeakRef<WaddleWindow>,
) {
    // Capture phase: the selectable body label would otherwise claim a
    // right-click for its own Cut/Copy/Paste menu.
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let (list_item, owner) = (item.downgrade(), window.clone());
    click.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let (Some(item), Some(window)) = (list_item.upgrade(), owner.upgrade()) {
            window.show_message_actions(&item, x, y);
        }
    });
    widget.add_controller(click);

    let press = gtk::GestureLongPress::builder()
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let (list_item, owner) = (item.downgrade(), window.clone());
    press.connect_pressed(move |gesture, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let (Some(item), Some(window)) = (list_item.upgrade(), owner.upgrade()) {
            window.show_message_actions(&item, x, y);
        }
    });
    widget.add_controller(press);
}

/// Avatar gutter beside sender and time, reply quote, body, images and
/// reaction chips, under the date when the row starts a day. A row that
/// continues its group shows its time in the gutter on hover instead.
fn row_widget() -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let day = gtk::Box::builder()
        .spacing(12)
        .margin_top(16)
        .margin_bottom(8)
        .build();
    day.add_css_class("day-divider");
    let day_label = gtk::Label::new(None);
    day_label.add_css_class("caption-heading");
    day_label.add_css_class("dim-label");
    for (at, widget) in [
        gtk::Separator::new(gtk::Orientation::Horizontal).upcast::<gtk::Widget>(),
        day_label.upcast(),
        gtk::Separator::new(gtk::Orientation::Horizontal).upcast(),
    ]
    .into_iter()
    .enumerate()
    {
        widget.set_valign(gtk::Align::Center);
        widget.set_hexpand(at != 1);
        day.append(&widget);
    }

    let row = gtk::Box::builder().spacing(12).build();
    row.add_css_class("waddle-message");
    let gutter = gtk::Stack::builder()
        .width_request(GUTTER)
        .vhomogeneous(false)
        .build();
    let avatar = avatar::avatar(GUTTER);
    avatar.set_valign(gtk::Align::Start);
    gutter.add_named(&avatar, Some("avatar"));
    let gutter_time = gtk::Label::builder()
        .valign(gtk::Align::Start)
        .margin_top(3)
        .build();
    for class in ["gutter-time", "caption", "numeric", "dim-label"] {
        gutter_time.add_css_class(class);
    }
    gutter.add_named(&gutter_time, Some("time"));

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .hexpand(true)
        .build();
    let header = gtk::Box::builder().spacing(8).build();
    let sender = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    sender.add_css_class("sender");
    let time = gtk::Label::builder()
        .xalign(0.0)
        .valign(gtk::Align::Baseline)
        .build();
    for class in ["caption", "numeric", "dim-label"] {
        time.add_css_class(class);
    }
    header.append(&sender);
    header.append(&time);

    let reply = gtk::Box::builder()
        .spacing(6)
        .halign(gtk::Align::Start)
        .build();
    reply.add_css_class("reply-quote");
    let reply_icon = gtk::Image::from_icon_name("mail-reply-sender-symbolic");
    reply_icon.add_css_class("dim-label");
    let reply_label = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    reply_label.add_css_class("caption");
    reply.append(&reply_icon);
    reply.append(&reply_label);

    let body = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .build();
    body.add_css_class("body");
    let media = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let reactions = gtk::Box::builder().spacing(6).margin_top(2).build();
    reactions.add_css_class("reactions");

    content.append(&header);
    content.append(&reply);
    content.append(&body);
    content.append(&media);
    content.append(&reactions);
    row.append(&gutter);
    row.append(&content);
    outer.append(&day);
    outer.append(&row);
    outer
}

/// The avatar column's width.
const GUTTER: i32 = 36;

struct RowParts {
    day: gtk::Box,
    day_label: gtk::Label,
    row: gtk::Box,
    gutter: gtk::Stack,
    avatar: adw::Avatar,
    gutter_time: gtk::Label,
    header: gtk::Box,
    sender: gtk::Label,
    time: gtk::Label,
    reply: gtk::Box,
    reply_label: gtk::Label,
    body: gtk::Label,
    media: gtk::Box,
    reactions: gtk::Box,
}

fn row_parts(widget: &gtk::Widget) -> Option<RowParts> {
    let day = widget.first_child().and_downcast::<gtk::Box>()?;
    let row = day.next_sibling().and_downcast::<gtk::Box>()?;
    let gutter = row.first_child().and_downcast::<gtk::Stack>()?;
    let content = gutter.next_sibling()?;
    let header = content.first_child().and_downcast::<gtk::Box>()?;
    let reply = header.next_sibling().and_downcast::<gtk::Box>()?;
    let body = reply.next_sibling().and_downcast::<gtk::Label>()?;
    let media = body.next_sibling().and_downcast::<gtk::Box>()?;
    let reactions = media.next_sibling().and_downcast::<gtk::Box>()?;
    Some(RowParts {
        day_label: day.first_child()?.next_sibling().and_downcast()?,
        avatar: gutter.child_by_name("avatar").and_downcast()?,
        gutter_time: gutter.child_by_name("time").and_downcast()?,
        sender: header.first_child().and_downcast()?,
        time: header.last_child().and_downcast()?,
        reply_label: reply.last_child().and_downcast()?,
        day,
        row,
        gutter,
        header,
        reply,
        body,
        media,
        reactions,
    })
}

fn bind_row(window: &glib::WeakRef<WaddleWindow>, widget: &gtk::Widget, row: &Row) {
    let Some(parts) = row_parts(widget) else {
        return;
    };
    let item = &row.item;
    let Layout { header, day } = row.layout;
    parts.day.set_visible(day);
    if day {
        parts.day_label.set_label(&format_day(item.timestamp_ms));
    }
    let time = format_time(item.timestamp_ms);
    parts.header.set_visible(header);
    parts.row.set_css_classes(if header {
        &["waddle-message", "starts-group"]
    } else {
        &["waddle-message"]
    });
    parts
        .gutter
        .set_visible_child_name(if header { "avatar" } else { "time" });
    avatar::set_name(&parts.avatar, &item.sender);
    parts.gutter_time.set_label(&time);
    parts.sender.set_label(&item.sender);
    parts.time.set_label(&time);

    match &item.reply {
        Some(reply) => {
            let who = reply.sender.as_deref().unwrap_or("a message");
            let what = if reply.target_retracted {
                "Message deleted".to_string()
            } else {
                match reply.snippet.as_deref() {
                    Some(snippet) if super::media::is_media_only(snippet) => "Photo".to_string(),
                    Some(snippet) => snippet.to_string(),
                    None => "Original message not loaded".to_string(),
                }
            };
            parts.reply_label.set_markup(&format!(
                "<b>{}</b>  <i>{}</i>",
                glib::markup_escape_text(who),
                glib::markup_escape_text(&what)
            ));
            parts.reply.set_visible(true);
        }
        None => parts.reply.set_visible(false),
    }

    let body = &parts.body;
    for class in ["dim-label", "error"] {
        body.remove_css_class(class);
    }
    parts.row.remove_css_class("sending");
    super::media::bind(&parts.media, &item.body, item.retracted);
    bind_reactions(window, &parts.reactions, item);
    if item.retracted {
        body.set_markup("<i>Message deleted</i>");
        body.add_css_class("dim-label");
        body.set_visible(true);
        return;
    }
    let mut markup = if super::media::is_media_only(&item.body) {
        String::new()
    } else {
        body_markup(&item.body)
    };
    let mut note = |text: &str| {
        if !markup.is_empty() {
            markup.push(' ');
        }
        markup.push_str(&format!("<span size=\"small\" alpha=\"60%\">{text}</span>"));
    };
    if item.edited {
        note("(edited)");
    }
    match item.delivery {
        Delivery::Sending => parts.row.add_css_class("sending"),
        Delivery::Failed => {
            note("· Not sent");
            body.add_css_class("error");
        }
        Delivery::Sent | Delivery::Received => {}
    }
    body.set_visible(!markup.is_empty());
    body.set_markup(&markup);
}

/// The body as Pango markup, with its web links clickable.
fn body_markup(body: &str) -> String {
    let mut markup = String::with_capacity(body.len());
    let mut rest = body;
    while !rest.is_empty() {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, after) = rest.split_at(word_end);
        let space_end = after
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(after.len());
        let (space, after) = after.split_at(space_end);
        // Trailing punctuation usually ends the sentence, not the link.
        let link_end = word
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '\'', '"'])
            .len();
        let (link, trailing) = word.split_at(link_end);
        let is_link = (link.starts_with("https://") || link.starts_with("http://"))
            && url::Url::parse(link).is_ok();
        if is_link {
            let escaped = glib::markup_escape_text(link);
            markup.push_str(&format!("<a href=\"{escaped}\">{escaped}</a>"));
            markup.push_str(&glib::markup_escape_text(trailing));
        } else {
            markup.push_str(&glib::markup_escape_text(word));
        }
        markup.push_str(&glib::markup_escape_text(space));
        rest = after;
    }
    markup
}

/// One toggle chip per emoji; clicking toggles the user's own reaction.
fn bind_reactions(window: &glib::WeakRef<WaddleWindow>, container: &gtk::Box, item: &TimelineItem) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let reference = item.reference_id.clone().filter(|_| !item.retracted);
    container.set_visible(reference.is_some() && !item.reactions.is_empty());
    let Some(reference) = reference else {
        return;
    };
    for Reaction { emoji, count, own } in &item.reactions {
        let chip = gtk::ToggleButton::builder()
            .label(format!("{emoji} {count}"))
            .active(*own)
            .build();
        chip.add_css_class("reaction");
        let (owner, target, emoji) = (window.clone(), reference.clone(), emoji.clone());
        chip.connect_clicked(move |_| {
            if let Some(window) = owner.upgrade() {
                window.toggle_reaction(&target, &emoji);
            }
        });
        container.append(&chip);
    }
}

fn format_time(ms: i64) -> String {
    glib::DateTime::from_unix_local(ms.div_euclid(1000))
        .and_then(|at| at.format("%H:%M"))
        .map(|text| text.to_string())
        .unwrap_or_default()
}

/// "Today", "Yesterday", the weekday this week, else the date.
fn format_day(ms: i64) -> String {
    let (Ok(at), Ok(now)) = (
        glib::DateTime::from_unix_local(ms.div_euclid(1000)),
        glib::DateTime::now_local(),
    ) else {
        return String::new();
    };
    let offset = utc_offset_ms();
    let local_day = |ms: i64| (ms + offset).div_euclid(24 * 60 * 60 * 1000);
    let format = match local_day(now.to_unix() * 1000) - local_day(ms) {
        0 => return "Today".to_string(),
        1 => return "Yesterday".to_string(),
        2..=6 => "%A",
        _ if at.year() == now.year() => "%A %-d %B",
        _ => "%-d %B %Y",
    };
    at.format(format)
        .map(|text| text.to_string())
        .unwrap_or_default()
}
