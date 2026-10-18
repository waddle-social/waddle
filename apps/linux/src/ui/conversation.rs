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

use super::WaddleWindow;
use crate::auth::store::StoredAccount;
use crate::session::{self, Arrival, Conversation, Kind, Session, SessionEvent};

/// One row in the list: the core's item plus whether it starts a group.
#[derive(Clone)]
pub(super) struct Row {
    pub item: TimelineItem,
    pub header: bool,
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
            bind_row(&window, &child, &object.borrow::<Row>());
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
            session.join(room);
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
        imp.composer.set_sensitive(imp.current.borrow().is_some());

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
            let row = adw::ActionRow::builder()
                .title(sidebar_title(conversation))
                .activatable(true)
                .build();
            if let Some(count) = unread.get(&conversation.jid).filter(|count| **count > 0) {
                let badge = gtk::Label::new(Some(&count.to_string()));
                badge.add_css_class("numeric");
                badge.add_css_class("dim-label");
                row.add_suffix(&badge);
                row.add_css_class("heading");
            }
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
        imp.composer.set_sensitive(imp.connected.get());
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
                    let objects: Vec<glib::BoxedAnyObject> =
                        items.iter().map(|item| boxed(item.clone(), true)).collect();
                    rows.splice(0, 0, &objects);
                    touched.iter_mut().for_each(|at| *at += items.len());
                    touched.extend(0..=items.len());
                }
                TimelineDiff::Append { items } => {
                    let start = rows.n_items() as usize;
                    let objects: Vec<glib::BoxedAnyObject> =
                        items.iter().map(|item| boxed(item.clone(), true)).collect();
                    rows.extend_from_slice(&objects);
                    touched.extend(start..start + items.len());
                }
                TimelineDiff::Set { index, item } => {
                    let at = *index as usize;
                    rows.splice(*index, 1, &[boxed((**item).clone(), true)]);
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
            let header = !timeline.continues_group(at, offset);
            let Ok(position) = u32::try_from(at) else {
                continue;
            };
            let current = rows
                .item(position)
                .and_downcast::<glib::BoxedAnyObject>()
                .map(|object| object.borrow::<Row>().header);
            if current != Some(header) {
                rows.splice(position, 1, &[boxed(item.clone(), header)]);
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
            .map(|(at, item)| boxed(item.clone(), !timeline.continues_group(at, offset)))
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

fn boxed(item: TimelineItem, header: bool) -> glib::BoxedAnyObject {
    glib::BoxedAnyObject::new(Row { item, header })
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

/// Header (sender + time), reply quote, body, reaction chips. The header
/// hides for a message that continues the previous one's group.
fn row_widget() -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .margin_start(16)
        .margin_end(16)
        .margin_top(2)
        .margin_bottom(2)
        .build();
    let header = gtk::Box::builder().spacing(8).margin_top(8).build();
    let sender = gtk::Label::builder().xalign(0.0).build();
    sender.add_css_class("heading");
    let time = gtk::Label::builder().xalign(0.0).build();
    time.add_css_class("dim-label");
    time.add_css_class("caption");
    time.add_css_class("numeric");
    header.append(&sender);
    header.append(&time);
    let reply = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    reply.add_css_class("dim-label");
    reply.add_css_class("caption");
    let body = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .build();
    let reactions = gtk::Box::builder().spacing(4).margin_top(2).build();
    row.append(&header);
    row.append(&reply);
    row.append(&body);
    row.append(&reactions);
    row
}

struct RowParts {
    header: gtk::Box,
    sender: gtk::Label,
    time: gtk::Label,
    reply: gtk::Label,
    body: gtk::Label,
    reactions: gtk::Box,
}

fn row_parts(widget: &gtk::Widget) -> Option<RowParts> {
    let header = widget.first_child().and_downcast::<gtk::Box>()?;
    let reply = header.next_sibling().and_downcast::<gtk::Label>()?;
    let body = reply.next_sibling().and_downcast::<gtk::Label>()?;
    let reactions = body.next_sibling().and_downcast::<gtk::Box>()?;
    Some(RowParts {
        sender: header.first_child().and_downcast::<gtk::Label>()?,
        time: header.last_child().and_downcast::<gtk::Label>()?,
        header,
        reply,
        body,
        reactions,
    })
}

fn bind_row(window: &WaddleWindow, widget: &gtk::Widget, row: &Row) {
    let Some(parts) = row_parts(widget) else {
        return;
    };
    let item = &row.item;
    parts.header.set_visible(row.header);
    parts.sender.set_label(&item.sender);
    parts.time.set_label(&format_time(item.timestamp_ms));

    match &item.reply {
        Some(reply) => {
            let who = reply.sender.as_deref().unwrap_or("a message");
            let what = if reply.target_retracted {
                "Message deleted".to_string()
            } else {
                reply
                    .snippet
                    .clone()
                    .unwrap_or_else(|| "Original message not loaded".to_string())
            };
            parts.reply.set_label(&format!("↪ {who}: {what}"));
            parts.reply.set_visible(true);
        }
        None => parts.reply.set_visible(false),
    }

    let body = &parts.body;
    for class in ["dim-label", "error"] {
        body.remove_css_class(class);
    }
    bind_reactions(window, &parts.reactions, item);
    if item.retracted {
        body.set_markup("<i>Message deleted</i>");
        body.add_css_class("dim-label");
        return;
    }
    let mut text = item.body.clone();
    if item.edited {
        text.push_str(" (edited)");
    }
    match item.delivery {
        Delivery::Sending => {
            body.set_label(&text);
            body.add_css_class("dim-label");
        }
        Delivery::Failed => {
            body.set_label(&format!("{text}  · not sent"));
            body.add_css_class("error");
        }
        Delivery::Sent | Delivery::Received => body.set_label(&text),
    }
}

/// One toggle chip per emoji; clicking toggles the user's own reaction.
fn bind_reactions(window: &WaddleWindow, container: &gtk::Box, item: &TimelineItem) {
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
        chip.add_css_class("pill");
        chip.add_css_class("small");
        let (owner, target, emoji) = (window.downgrade(), reference.clone(), emoji.clone());
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
