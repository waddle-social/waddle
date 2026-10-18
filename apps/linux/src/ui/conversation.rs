//! Rooms, the timeline and the composer.
//!
//! Each room's messages live in a `waddle_client_session::Timeline`; this
//! module applies the diffs it returns to a `gio::ListStore` behind a
//! virtualised `ListView`, so only changed rows are touched.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use waddle_client_session::{Delivery, Timeline, TimelineDiff, TimelineItem};

use super::WaddleWindow;
use crate::auth::store::StoredAccount;
use crate::session::{self, Room, Session, SessionEvent};

/// One row in the list: the core's item plus whether it starts a group.
#[derive(Clone)]
struct Row {
    item: TimelineItem,
    header: bool,
}

/// How long to wait for `Connected` before treating a connect as failed.
const CONNECT_BUDGET_SECS: u32 = 20;

impl WaddleWindow {
    pub(super) fn setup_timeline(&self) {
        let imp = self.imp();
        let rows = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            item.set_child(Some(&row_widget()));
            item.set_activatable(false);
            item.set_selectable(false);
        });
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let (Some(child), Some(object)) = (
                item.child(),
                item.item().and_downcast::<glib::BoxedAnyObject>(),
            ) else {
                return;
            };
            bind_row(&child, &object.borrow::<Row>());
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
        imp.rooms.replace(Vec::new());
        imp.timelines.replace(Default::default());
        imp.unread.replace(Default::default());
        imp.current_room.replace(None);
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
                let nick = imp
                    .session
                    .borrow()
                    .as_ref()
                    .map(|session| session.nick.clone())
                    .unwrap_or_default();
                let Some((room, live)) = session::live_from(*message, &nick) else {
                    return;
                };
                let counts_as_unread = !live.own && live.body.is_some();
                let diffs = imp
                    .timelines
                    .borrow_mut()
                    .entry(room.clone())
                    .or_default()
                    .apply_live(live);
                if imp.current_room.borrow().as_deref() == Some(room.as_str()) {
                    let stick = self.near_bottom();
                    self.apply(&diffs);
                    if stick {
                        self.scroll_to_end();
                    }
                } else if counts_as_unread && !diffs.is_empty() {
                    *imp.unread.borrow_mut().entry(room).or_default() += 1;
                    self.render_rooms();
                }
            }
            SessionEvent::DeliveryAcked(id) => self.set_delivery(&id, Delivery::Sent),
            SessionEvent::DeliveryFailed(id) => self.set_delivery(&id, Delivery::Failed),
        }
    }

    async fn on_connected(&self) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        imp.account_status.set_title("Choose a room");
        let rooms = session.rooms().await;
        for room in rooms.iter().filter(|room| room.autojoin) {
            session.join(room);
        }
        imp.rooms.replace(rooms);
        self.render_rooms();
        imp.composer
            .set_sensitive(imp.current_room.borrow().is_some());

        // Reopen the room on screen, or the first room on first connect.
        let current = imp.current_room.borrow().clone();
        let target = current.or_else(|| imp.rooms.borrow().first().map(|room| room.jid.clone()));
        if let Some(jid) = target {
            self.open_room(&jid);
        }
    }

    fn render_rooms(&self) {
        let imp = self.imp();
        while let Some(row) = imp.room_list.first_child() {
            imp.room_list.remove(&row);
        }
        let current = imp.current_room.borrow().clone();
        let unread = imp.unread.borrow();
        for room in imp.rooms.borrow().iter() {
            let row = adw::ActionRow::builder()
                .title(format!("# {}", room.name))
                .activatable(true)
                .build();
            if let Some(count) = unread.get(&room.jid).filter(|count| **count > 0) {
                let badge = gtk::Label::new(Some(&count.to_string()));
                badge.add_css_class("numeric");
                badge.add_css_class("dim-label");
                row.add_suffix(&badge);
                row.add_css_class("heading");
            }
            imp.room_list.append(&row);
            if current.as_deref() == Some(room.jid.as_str()) {
                imp.room_list.select_row(Some(&row));
            }
        }
    }

    pub(super) fn room_at(&self, index: i32) -> Option<Room> {
        let index = usize::try_from(index).ok()?;
        self.imp().rooms.borrow().get(index).cloned()
    }

    pub(super) fn open_room(&self, jid: &str) {
        let imp = self.imp();
        let Some(room) = imp
            .rooms
            .borrow()
            .iter()
            .find(|room| room.jid == jid)
            .cloned()
        else {
            return;
        };
        imp.current_room.replace(Some(room.jid.clone()));
        imp.unread.borrow_mut().remove(&room.jid);
        imp.room_title.set_title(&format!("# {}", room.name));
        imp.room_page.set_title(&room.name);
        imp.content_stack.set_visible_child_name("timeline");
        imp.composer.set_sensitive(imp.connected.get());
        imp.split_view.set_show_content(true);
        self.render_rooms();

        let needs_history = imp
            .timelines
            .borrow()
            .get(&room.jid)
            .is_none_or(|timeline| timeline.older_cursor().is_none());
        self.rebuild_rows();
        self.scroll_to_end();
        if needs_history {
            self.load_history(room.jid, None);
        }
    }

    fn load_older(&self) {
        let imp = self.imp();
        let Some(room) = imp.current_room.borrow().clone() else {
            return;
        };
        let cursor = {
            let timelines = imp.timelines.borrow();
            let Some(timeline) = timelines.get(&room) else {
                return;
            };
            if timeline.history_complete() {
                return;
            }
            timeline.older_cursor().map(str::to_string)
        };
        if cursor.is_some() {
            self.load_history(room, cursor);
        }
    }

    fn load_history(&self, room: String, before: Option<String>) {
        let imp = self.imp();
        if imp.loading_history.replace(true) {
            return;
        }
        let Some(session) = imp.session.borrow().clone() else {
            imp.loading_history.set(false);
            return;
        };
        let initial = before.is_none();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let (messages, first_id, complete) = session.history(&room, before).await;
            let imp = window.imp();
            imp.loading_history.set(false);
            let diffs = imp
                .timelines
                .borrow_mut()
                .entry(room.clone())
                .or_default()
                .apply_history_page(messages, first_id, complete);
            if imp.current_room.borrow().as_deref() != Some(room.as_str()) {
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

    pub(super) fn send_current(&self) {
        let imp = self.imp();
        let body = imp.composer_entry.text().trim().to_string();
        let (Some(room), Some(session)) = (
            imp.current_room.borrow().clone(),
            imp.session.borrow().clone(),
        ) else {
            return;
        };
        if body.is_empty() {
            return;
        }
        imp.composer_entry.set_text("");
        let origin = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();
        let diffs = imp
            .timelines
            .borrow_mut()
            .entry(room.clone())
            .or_default()
            .add_local_echo(origin.clone(), session.nick.clone(), body.clone(), now);
        self.apply(&diffs);
        self.scroll_to_end();

        let window = self.clone();
        glib::spawn_future_local(async move {
            if !session.send(&room, body, origin.clone()).await {
                window.set_delivery(&origin, Delivery::Failed);
                window.toast("The message could not be sent.");
            }
        });
    }

    fn set_delivery(&self, id: &str, delivery: Delivery) {
        let imp = self.imp();
        let current = imp.current_room.borrow().clone();
        let mut current_diffs = Vec::new();
        for (room, timeline) in imp.timelines.borrow_mut().iter_mut() {
            let diffs = timeline.set_delivery(id, delivery);
            if current.as_deref() == Some(room.as_str()) {
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

    /// Bring the list store in step with the current room's timeline.
    fn apply(&self, diffs: &[TimelineDiff]) {
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
                    rows.splice(*index, 1, &[boxed(item.clone(), true)]);
                    touched.extend([at, at + 1]);
                }
            }
        }
        self.refresh_headers(&rows, touched);
    }

    /// Recompute the group header for these rows from the core.
    fn refresh_headers(&self, rows: &gio::ListStore, mut touched: Vec<usize>) {
        let imp = self.imp();
        let Some(room) = imp.current_room.borrow().clone() else {
            return;
        };
        let timelines = imp.timelines.borrow();
        let Some(timeline) = timelines.get(&room) else {
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
        let Some(room) = imp.current_room.borrow().clone() else {
            return;
        };
        let timelines = imp.timelines.borrow();
        let empty = Timeline::new();
        let timeline = timelines.get(&room).unwrap_or(&empty);
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

    fn scroll_to_end(&self) {
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

fn boxed(item: TimelineItem, header: bool) -> glib::BoxedAnyObject {
    glib::BoxedAnyObject::new(Row { item, header })
}

fn utc_offset_ms() -> i64 {
    glib::DateTime::now_local()
        .map(|now| now.utc_offset().as_milliseconds())
        .unwrap_or(0)
}

/// Header (sender + time) above the body; the header hides for a message
/// that continues the previous one's group.
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
    let body = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .build();
    row.append(&header);
    row.append(&body);
    row
}

fn bind_row(widget: &gtk::Widget, row: &Row) {
    let Some(container) = widget.downcast_ref::<gtk::Box>() else {
        return;
    };
    let (Some(header), Some(body)) = (
        container.first_child().and_downcast::<gtk::Box>(),
        container.last_child().and_downcast::<gtk::Label>(),
    ) else {
        return;
    };
    let (Some(sender), Some(time)) = (
        header.first_child().and_downcast::<gtk::Label>(),
        header.last_child().and_downcast::<gtk::Label>(),
    ) else {
        return;
    };
    let item = &row.item;
    header.set_visible(row.header);
    sender.set_label(&item.sender);
    time.set_label(&format_time(item.timestamp_ms));

    for class in ["dim-label", "error"] {
        body.remove_css_class(class);
    }
    if item.retracted {
        body.set_markup("<i>Message deleted</i>");
        body.add_css_class("dim-label");
        return;
    }
    let mut text = item.body.clone();
    if item.edited {
        text.push_str(" (edited)");
    }
    body.set_label(&text);
    match item.delivery {
        Delivery::Sending => body.add_css_class("dim-label"),
        Delivery::Failed => {
            body.add_css_class("error");
            body.set_label(&format!("{text}  · not sent"));
        }
        Delivery::Sent | Delivery::Received => {}
    }
}

fn format_time(ms: i64) -> String {
    glib::DateTime::from_unix_local(ms.div_euclid(1000))
        .and_then(|at| at.format("%H:%M"))
        .map(|text| text.to_string())
        .unwrap_or_default()
}
