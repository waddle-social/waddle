//! Pins, archive search, jumping to a message and the hover toolbar.
//!
//! The rules live outside the widgets: `message_actions.rs` decides what a
//! message offers, `pins.rs` keeps the pinned list, `jump.rs` plans a jump.
//! This module wires them to the header buttons, the search bar and the
//! rows.

use std::collections::{HashMap, HashSet};

use adw::prelude::{
    AccessibleExtManual, ActionRowExt, AdwDialogExt, AlertDialogExt, AlertDialogExtManual, BoxExt,
    ButtonExt, Cast, CastNone, EditableExt, ListBoxRowExt, ObjectExt, PopoverExt, StaticType,
    ToggleButtonExt, WidgetExt,
};
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gdk, glib};
use waddle_client_session::TimelineItem;
use waddle_xmpp_client_ffi::WaddlePresence;

use super::conversation::{boxed, utc_offset_ms, Layout};
use super::WaddleWindow;
use crate::jump::{self, Step, JUMP_PAGES};
use crate::message_actions::{
    self, Context, MessageAction, PinPermission, Standing, TOOLBAR_REACTIONS,
};
use crate::message_text::preview;
use crate::pins::{Pin, PinBook};
use crate::session::{Conversation, Kind};
use crate::session_tools::{PinUpdate, SearchHit};

/// How long a jumped-to message stays marked.
const HIGHLIGHT_SECS: u32 = 3;

struct Jump {
    key: String,
    target: String,
    pages_left: u8,
}

/// The window's pin, search and jump state.
#[derive(Default)]
pub struct ToolsState {
    /// By conversation JID: a room's, or the DM peer's.
    pins: HashMap<String, PinBook>,
    /// Conversations with a pin fetch in flight.
    fetching: HashSet<String>,
    /// Conversations whose pin fetch failed.
    failed: HashSet<String>,
    permissions: HashMap<String, PinPermission>,
    standing: HashMap<String, Standing>,
    jump: Option<Jump>,
    highlight: Option<String>,
    /// The newest search; an older answer is dropped.
    search_seq: u64,
    /// The conversation the open search belongs to.
    search_for: Option<String>,
    pins_list: Option<gtk::ListBox>,
}

fn when(ms: i64) -> String {
    glib::DateTime::from_unix_local(ms.div_euclid(1000))
        .ok()
        .and_then(|time| time.format("%b %-d, %H:%M").ok())
        .map(|text| text.to_string())
        .unwrap_or_default()
}

fn when_rfc3339(text: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|time| when(time.timestamp_millis()))
        .unwrap_or_default()
}

fn result_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .title_lines(2)
        .subtitle_lines(1)
        .use_markup(false)
        .activatable(true)
        .build();
    row.add_css_class("message-result");
    row
}

fn note_row(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    label.add_css_class("dim-label");
    label
}

impl WaddleWindow {
    pub(super) fn setup_message_tools(&self) {
        let imp = self.imp();

        // The pinned list in the header button's popover.
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.add_css_class("navigation-sidebar");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(380)
            .min_content_width(320)
            .build();
        let popover = gtk::Popover::builder().child(&scroller).build();
        let window = self.downgrade();
        popover.connect_show(move |_| {
            if let Some(window) = window.upgrade() {
                window.refresh_pins_for_popover();
                window.rebuild_pins_list();
            }
        });
        imp.pins_button.set_popover(Some(&popover));
        imp.tools.borrow_mut().pins_list = Some(list);

        // Search.
        let window = self.downgrade();
        imp.search_entry.connect_search_changed(move |_| {
            if let Some(window) = window.upgrade() {
                window.run_search();
            }
        });
        let window = self.downgrade();
        imp.search_entry.connect_activate(move |_| {
            if let Some(window) = window.upgrade() {
                window.run_search();
            }
        });
        let window = self.downgrade();
        imp.search_bar
            .connect_search_mode_enabled_notify(move |bar| {
                let Some(window) = window.upgrade() else {
                    return;
                };
                if bar.is_search_mode() {
                    window.imp().search_entry.grab_focus();
                } else {
                    window.clear_search();
                }
            });

        // Ctrl+F searches the open conversation.
        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Local);
        let window = self.downgrade();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>f"),
            Some(gtk::CallbackAction::new(move |_, _| {
                let Some(window) = window.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let button = window.imp().search_button.clone();
                if button.is_visible() {
                    button.set_active(true);
                    window.imp().search_entry.grab_focus();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            })),
        ));
        self.add_controller(shortcuts);
    }

    /// Forget everything about pins, permissions and jumps: the session
    /// is gone or its connection dropped.
    pub(super) fn reset_message_tools(&self) {
        let mut tools = self.imp().tools.borrow_mut();
        tools.pins.clear();
        tools.fetching.clear();
        tools.failed.clear();
        tools.permissions.clear();
        tools.standing.clear();
        tools.jump = None;
        tools.highlight = None;
        tools.search_seq = tools.search_seq.wrapping_add(1);
    }

    /// Show the header buttons for the open conversation and fetch its pins.
    pub(super) fn refresh_message_tools(&self) {
        let imp = self.imp();
        let conversation = self.current_conversation();
        let usable = conversation
            .as_ref()
            .is_some_and(|conversation| conversation.thread.is_none());
        imp.pins_button.set_visible(usable);
        imp.search_button.set_visible(usable);
        self.refresh_call_button();
        let key = conversation.as_ref().map(Conversation::key);
        let moved = imp.tools.borrow().search_for != key;
        if moved || !usable {
            imp.search_button.set_active(false);
            self.clear_search();
            imp.tools.borrow_mut().search_for = key;
        }
        if let Some(conversation) = conversation.filter(|_| usable) {
            self.load_pins(&conversation.jid, conversation.kind, false);
        }
    }

    // ── Standing and permission ─────────────────────────────────────────

    /// The account's own room presence says what it may do there, and that
    /// the room can be asked for its pins.
    pub(super) fn note_presence(&self, presence: &WaddlePresence) {
        use waddle_xmpp_client_ffi::{WaddleMucAffiliation, WaddleMucRole};
        if !presence.muc_status_codes.contains(&110) {
            return;
        }
        let Some(from) = presence
            .from
            .as_deref()
            .and_then(|from| from.parse::<jid::Jid>().ok())
        else {
            return;
        };
        let room = from.to_bare().to_string();
        let imp = self.imp();
        if !matches!(presence.presence_type.as_str(), "available" | "") {
            imp.tools.borrow_mut().standing.remove(&room);
            return;
        }
        let standing = Standing {
            owner_or_admin: matches!(
                presence.muc_affiliation,
                Some(WaddleMucAffiliation::Owner | WaddleMucAffiliation::Admin)
            ),
            moderator: presence.muc_role == Some(WaddleMucRole::Moderator),
        };
        imp.tools
            .borrow_mut()
            .standing
            .insert(room.clone(), standing);
        self.load_pin_permission(&room);
        self.load_pins(&room, Kind::Room, false);
    }

    fn load_pin_permission(&self, room: &str) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let (window, room) = (self.clone(), room.to_string());
        let generation = imp.generation.get();
        glib::spawn_future_local(async move {
            let permission = session.pin_permission(&room).await;
            if window.imp().generation.get() != generation {
                return;
            }
            if let Some(permission) = permission {
                window
                    .imp()
                    .tools
                    .borrow_mut()
                    .permissions
                    .insert(room, permission);
            }
        });
    }

    /// What the account may do to messages in `conversation`.
    fn action_context(&self, conversation: &Conversation) -> Context {
        let room = conversation.kind == Kind::Room;
        let tools = self.imp().tools.borrow();
        let standing = tools
            .standing
            .get(&conversation.jid)
            .copied()
            .unwrap_or_default();
        let permission = tools.permissions.get(&conversation.jid).copied();
        drop(tools);
        let ready = self.can_send(conversation);
        Context {
            in_thread: conversation.thread.is_some(),
            can_pin: ready && message_actions::may_pin(room, permission, standing),
            can_moderate: ready && message_actions::may_moderate(room, standing),
        }
    }

    /// The actions for a message of the open conversation.
    pub(super) fn actions_for(&self, item: &TimelineItem) -> Vec<MessageAction> {
        let Some(conversation) = self.current_conversation() else {
            return Vec::new();
        };
        let pinned = self.is_pinned(item);
        message_actions::actions(item, self.action_context(&conversation), pinned)
    }

    // ── Pins ─────────────────────────────────────────────────────────────

    pub(super) fn is_pinned(&self, item: &TimelineItem) -> bool {
        let Some(conversation) = self.current_conversation() else {
            return false;
        };
        self.imp()
            .tools
            .borrow()
            .pins
            .get(&conversation.jid)
            .is_some_and(|book| book.is_pinned(item))
    }

    /// Whether the row was just jumped to.
    pub(super) fn is_highlighted(&self, item: &TimelineItem) -> bool {
        let tools = self.imp().tools.borrow();
        let Some(target) = tools.highlight.as_deref() else {
            return false;
        };
        item.key.as_str() == target || item.reference_id.as_deref() == Some(target)
    }

    /// Fetch the pinned list unless it is held, or in flight.
    fn load_pins(&self, jid: &str, kind: Kind, force: bool) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let ready = imp.connected.get()
            && (kind == Kind::Direct || imp.tools.borrow().standing.contains_key(jid));
        {
            let mut tools = imp.tools.borrow_mut();
            let held = tools.pins.get(jid).is_some_and(PinBook::hydrated);
            if !ready || tools.fetching.contains(jid) || (held && !force) {
                return;
            }
            tools.fetching.insert(jid.to_string());
            tools.failed.remove(jid);
        }
        let (window, jid) = (self.clone(), jid.to_string());
        let generation = imp.generation.get();
        glib::spawn_future_local(async move {
            let result = session.pins(&jid).await;
            let imp = window.imp();
            if imp.generation.get() != generation {
                return;
            }
            let before = window.pinned_targets(&jid);
            let mut refetch = false;
            {
                let mut tools = imp.tools.borrow_mut();
                tools.fetching.remove(&jid);
                match result {
                    Ok(entries) => {
                        refetch = tools.pins.entry(jid.clone()).or_default().hydrate(entries);
                    }
                    Err(_) => {
                        tools.failed.insert(jid.clone());
                    }
                }
            }
            window.after_pins_changed(&jid, before);
            if refetch {
                window.load_pins(&jid, kind, true);
            }
        });
    }

    fn pinned_targets(&self, jid: &str) -> Vec<String> {
        self.imp()
            .tools
            .borrow()
            .pins
            .get(jid)
            .map(|book| {
                book.entries()
                    .iter()
                    .map(|pin| pin.target.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Rebind the rows whose pinned state changed and refresh the list.
    fn after_pins_changed(&self, jid: &str, before: Vec<String>) {
        let after = self.pinned_targets(jid);
        let changed: Vec<String> = before
            .iter()
            .filter(|target| !after.contains(target))
            .chain(after.iter().filter(|target| !before.contains(target)))
            .cloned()
            .collect();
        if self.imp().current.borrow().as_deref() == Some(jid) {
            self.refresh_rows(&changed);
        }
        self.rebuild_pins_list();
    }

    /// A pin or unpin broadcast.
    pub(super) fn on_pin_update(&self, update: PinUpdate) {
        let PinUpdate {
            conversation,
            change,
        } = update;
        let before = self.pinned_targets(&conversation);
        let refetch = self
            .imp()
            .tools
            .borrow_mut()
            .pins
            .entry(conversation.clone())
            .or_default()
            .apply(change);
        self.after_pins_changed(&conversation, before);
        if refetch {
            let kind = self
                .imp()
                .conversations
                .borrow()
                .iter()
                .find(|known| known.jid == conversation && known.thread.is_none())
                .map(|known| known.kind);
            if let Some(kind) = kind {
                self.load_pins(&conversation, kind, true);
            }
        }
    }

    /// Opening the list asks again when the last fetch failed.
    fn refresh_pins_for_popover(&self) {
        let Some(conversation) = self.current_conversation() else {
            return;
        };
        let failed = self.imp().tools.borrow().failed.contains(&conversation.jid);
        if failed {
            self.load_pins(&conversation.jid, conversation.kind, true);
        }
    }

    fn rebuild_pins_list(&self) {
        let imp = self.imp();
        let Some(list) = imp.tools.borrow().pins_list.clone() else {
            return;
        };
        list.remove_all();
        let Some(conversation) = self.current_conversation() else {
            return;
        };
        let (entries, loading, failed, can_unpin) = {
            let tools = imp.tools.borrow();
            let book = tools.pins.get(&conversation.jid);
            (
                book.map(|book| book.entries().to_vec()).unwrap_or_default(),
                !book.is_some_and(PinBook::hydrated) && !tools.failed.contains(&conversation.jid),
                tools.failed.contains(&conversation.jid),
                false,
            )
        };
        let can_unpin = can_unpin || self.action_context(&conversation).can_pin;
        if entries.is_empty() {
            let text = if failed {
                "The pinned messages could not be loaded."
            } else if loading {
                "Loading…"
            } else {
                "No pinned messages"
            };
            list.append(&note_row(text));
            if let Some(row) = list.row_at_index(0) {
                row.set_activatable(false);
                row.set_selectable(false);
            }
            return;
        }
        for pin in entries {
            list.append(&self.pin_row(&pin, can_unpin, &conversation));
        }
    }

    fn pin_row(&self, pin: &Pin, can_unpin: bool, conversation: &Conversation) -> adw::ActionRow {
        let author = pin.author();
        let row = result_row(
            &preview(author, &pin.text),
            &format!("{author} · {}", when_rfc3339(&pin.message_at)),
        );
        let (window, target) = (self.clone(), pin.target.clone());
        row.connect_activated(move |row| {
            if let Some(popover) = row
                .ancestor(gtk::Popover::static_type())
                .and_downcast::<gtk::Popover>()
            {
                popover.popdown();
            }
            window.jump_to(&target);
        });
        if can_unpin {
            let button = gtk::Button::builder()
                .icon_name("view-pin-symbolic")
                .tooltip_text("Unpin")
                .valign(gtk::Align::Center)
                .build();
            button.add_css_class("flat");
            button.update_property(&[gtk::accessible::Property::Label("Unpin message")]);
            let (window, target, conversation) =
                (self.clone(), pin.target.clone(), conversation.clone());
            button
                .connect_clicked(move |_| window.set_pinned(&conversation, target.clone(), false));
            row.add_suffix(&button);
        }
        row
    }

    /// Pin or unpin; the room's or peer's broadcast updates the list.
    pub(super) fn set_pinned(&self, conversation: &Conversation, target: String, pinned: bool) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let (window, conversation) = (self.clone(), conversation.clone());
        let generation = imp.generation.get();
        glib::spawn_future_local(async move {
            let sent = session
                .set_pinned(&conversation.jid, conversation.kind, target, pinned)
                .await;
            if sent || window.imp().generation.get() != generation {
                return;
            }
            window.toast(if pinned {
                "The message could not be pinned."
            } else {
                "The message could not be unpinned."
            });
        });
    }

    // ── Moderation ───────────────────────────────────────────────────────

    pub(super) fn confirm_remove(&self, item: &TimelineItem) {
        let Some(target) = message_actions::pin_target(item)
            .map(str::to_string)
            .or_else(|| item.reference_id.clone())
        else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some("Remove Message?"),
            Some("It is removed for everyone in the room, and the room keeps a record that a moderator removed it."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let window = self.clone();
        dialog.connect_response(Some("remove"), move |_, _| window.remove(target.clone()));
        dialog.present(Some(self));
    }

    fn remove(&self, target: String) {
        let imp = self.imp();
        let (Some(conversation), Some(session)) =
            (self.current_conversation(), imp.session.borrow().clone())
        else {
            return;
        };
        let (window, generation) = (self.clone(), imp.generation.get());
        glib::spawn_future_local(async move {
            if !session.remove_message(&conversation.jid, target).await
                && window.imp().generation.get() == generation
            {
                window.toast("The message could not be removed.");
            }
        });
    }

    // ── Search ───────────────────────────────────────────────────────────

    fn clear_search(&self) {
        let imp = self.imp();
        {
            let mut tools = imp.tools.borrow_mut();
            tools.search_seq = tools.search_seq.wrapping_add(1);
        }
        imp.search_entry.set_text("");
        imp.search_results.remove_all();
        imp.search_results_scroller.set_visible(false);
    }

    fn run_search(&self) {
        let imp = self.imp();
        let query = imp.search_entry.text().trim().to_string();
        let seq = {
            let mut tools = imp.tools.borrow_mut();
            tools.search_seq = tools.search_seq.wrapping_add(1);
            tools.search_seq
        };
        imp.search_results.remove_all();
        if query.is_empty() {
            imp.search_results_scroller.set_visible(false);
            return;
        }
        let (Some(conversation), Some(session)) =
            (self.current_conversation(), imp.session.borrow().clone())
        else {
            return;
        };
        imp.search_results.append(&note_row("Searching…"));
        imp.search_results_scroller.set_visible(true);
        let (window, generation) = (self.clone(), imp.generation.get());
        glib::spawn_future_local(async move {
            let result = session
                .search(&conversation.jid, conversation.kind, query)
                .await;
            let imp = window.imp();
            if imp.generation.get() != generation || imp.tools.borrow().search_seq != seq {
                return;
            }
            imp.search_results.remove_all();
            match result {
                Ok(hits) if hits.is_empty() => imp.search_results.append(&note_row("No results")),
                Ok(hits) => {
                    for hit in hits {
                        imp.search_results.append(&window.hit_row(&hit));
                    }
                }
                Err(message) => imp.search_results.append(&note_row(&message)),
            }
        });
    }

    fn hit_row(&self, hit: &SearchHit) -> adw::ActionRow {
        let row = result_row(
            &preview(&hit.sender, &hit.body),
            &format!("{} · {}", hit.sender, when(hit.timestamp_ms)),
        );
        let (window, target) = (self.clone(), hit.stanza_id.clone());
        row.connect_activated(move |_| {
            window.imp().search_button.set_active(false);
            window.jump_to(&target);
        });
        row
    }

    // ── Jumping ──────────────────────────────────────────────────────────

    /// Scroll to the message with this archive id, paging older history in
    /// until it is loaded.
    pub(super) fn jump_to(&self, target: &str) {
        let Some(conversation) = self.current_conversation() else {
            return;
        };
        self.imp().tools.borrow_mut().jump = Some(Jump {
            key: conversation.key(),
            target: target.to_string(),
            pages_left: JUMP_PAGES,
        });
        self.advance_jump();
    }

    pub(super) fn cancel_jump(&self) {
        self.imp().tools.borrow_mut().jump = None;
    }

    /// Take the next step of a pending jump; called when a history page
    /// has been applied.
    pub(super) fn advance_jump(&self) {
        let imp = self.imp();
        let (key, target, pages_left) = {
            let tools = imp.tools.borrow();
            let Some(jump) = tools.jump.as_ref() else {
                return;
            };
            (jump.key.clone(), jump.target.clone(), jump.pages_left)
        };
        if imp.current.borrow().as_deref() != Some(key.as_str()) {
            return self.cancel_jump();
        }
        if imp.loading_history.borrow().contains(&key) {
            return;
        }
        let step = {
            let timelines = imp.timelines.borrow();
            let Some(timeline) = timelines.get(&key) else {
                drop(timelines);
                return self.cancel_jump();
            };
            jump::next_step(
                timeline.items(),
                &target,
                timeline.history_complete(),
                pages_left,
            )
        };
        match step {
            Step::Found(at) => {
                self.cancel_jump();
                self.scroll_to(at);
                self.highlight(&target);
            }
            Step::LoadOlder => {
                if let Some(jump) = imp.tools.borrow_mut().jump.as_mut() {
                    jump.pages_left = jump.pages_left.saturating_sub(1);
                }
                self.load_older();
            }
            Step::Missing => {
                self.cancel_jump();
                self.toast("That message is not in the conversation's loaded history.");
            }
        }
    }

    fn highlight(&self, target: &str) {
        let imp = self.imp();
        imp.tools.borrow_mut().highlight = Some(target.to_string());
        self.refresh_rows(&[target.to_string()]);
        let (window, target) = (self.downgrade(), target.to_string());
        glib::timeout_add_seconds_local_once(HIGHLIGHT_SECS, move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let cleared = {
                let mut tools = window.imp().tools.borrow_mut();
                let same = tools.highlight.as_deref() == Some(target.as_str());
                if same {
                    tools.highlight = None;
                }
                same
            };
            if cleared {
                window.refresh_rows(&[target]);
            }
        });
    }

    /// Bind the rows of these archive ids again.
    fn refresh_rows(&self, targets: &[String]) {
        let imp = self.imp();
        let (Some(rows), Some(key)) = (imp.rows.borrow().clone(), imp.current.borrow().clone())
        else {
            return;
        };
        let replacements: Vec<(u32, glib::BoxedAnyObject)> = {
            let timelines = imp.timelines.borrow();
            let Some(timeline) = timelines.get(&key) else {
                return;
            };
            let offset = utc_offset_ms();
            targets
                .iter()
                .filter_map(|target| {
                    let at = jump::position(timeline.items(), target)?;
                    let layout = Layout::of(timeline, at, offset);
                    Some((
                        u32::try_from(at).ok()?,
                        boxed(timeline.items()[at].clone(), layout),
                    ))
                })
                .collect()
        };
        for (position, object) in replacements {
            rows.splice(position, 1, &[object]);
        }
    }

    // ── Emoji picker ─────────────────────────────────────────────────────

    /// The full emoji chooser for a reaction on `target`, pointing at
    /// `rect` of `anchor`.
    pub(super) fn pick_reaction(&self, anchor: &gtk::Widget, rect: gdk::Rectangle, target: String) {
        let chooser = gtk::EmojiChooser::new();
        chooser.set_parent(anchor);
        chooser.set_pointing_to(Some(&rect));
        let window = self.clone();
        chooser.connect_emoji_picked(move |_, emoji| window.toggle_reaction(&target, emoji));
        chooser.connect_closed(|chooser| {
            let chooser = chooser.clone();
            glib::idle_add_local_once(move || chooser.unparent());
        });
        chooser.popup();
    }
}

// ── The hover toolbar ────────────────────────────────────────────────────

/// An empty toolbar for a row; `bind_toolbar` fills it.
pub(super) fn new_toolbar() -> gtk::Box {
    let toolbar = gtk::Box::builder()
        .spacing(2)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_end(8)
        .visible(false)
        .build();
    toolbar.add_css_class("message-toolbar");
    toolbar
}

/// Fill the toolbar from the same list the message menu uses. `row` is the
/// row's widget, which any popover opens on.
pub(super) fn bind_toolbar(
    window: &glib::WeakRef<WaddleWindow>,
    toolbar: &gtk::Box,
    row: &gtk::Widget,
    item: &TimelineItem,
) {
    while let Some(child) = toolbar.first_child() {
        toolbar.remove(&child);
    }
    let Some(owner) = window.upgrade() else {
        toolbar.set_visible(false);
        return;
    };
    let list = owner.actions_for(item);
    toolbar.set_visible(!list.is_empty());
    if list.is_empty() {
        return;
    }
    let rect_of = |widget: &gtk::Widget, row: &gtk::Widget| {
        widget
            .compute_bounds(row)
            .map(|bounds| {
                gdk::Rectangle::new(
                    bounds.x() as i32,
                    bounds.y() as i32,
                    bounds.width() as i32,
                    bounds.height() as i32,
                )
            })
            .unwrap_or_else(|| gdk::Rectangle::new(0, 0, 1, 1))
    };

    if let Some(reference) = item.reference_id.clone() {
        let own = owner
            .current_conversation()
            .and_then(|conversation| {
                owner
                    .imp()
                    .timelines
                    .borrow()
                    .get(&conversation.key())
                    .map(|timeline| timeline.own_reactions(&reference))
            })
            .unwrap_or_default();
        for emoji in message_actions::QUICK_REACTIONS
            .iter()
            .take(TOOLBAR_REACTIONS)
        {
            let button = gtk::ToggleButton::builder()
                .label(*emoji)
                .active(own.iter().any(|held| held == emoji))
                .tooltip_text(format!("React with {emoji}"))
                .build();
            button.add_css_class("flat");
            let (window, target) = (window.clone(), reference.clone());
            button.connect_clicked(move |_| {
                if let Some(window) = window.upgrade() {
                    window.toggle_reaction(&target, emoji);
                }
            });
            toolbar.append(&button);
        }
    }
    for action in list.iter().copied().filter(|action| action.in_toolbar()) {
        let (icon, label) = action.presentation();
        let button = gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(label)
            .build();
        button.add_css_class("flat");
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        let (window, item, row) = (window.clone(), item.clone(), row.clone());
        button.connect_clicked(move |button| {
            if let Some(window) = window.upgrade() {
                let rect = rect_of(button.upcast_ref(), &row);
                window.perform_action(action, &item, &row, rect);
            }
        });
        toolbar.append(&button);
    }
    let more = gtk::Button::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("More Actions")
        .build();
    more.add_css_class("flat");
    more.update_property(&[gtk::accessible::Property::Label("More actions")]);
    let (window, item, row) = (window.clone(), item.clone(), row.clone());
    more.connect_clicked(move |button| {
        if let Some(window) = window.upgrade() {
            let rect = rect_of(button.upcast_ref(), &row);
            window.present_menu(&row, rect, item.clone());
        }
    });
    toolbar.append(&more);
}

/// Menu key and Shift+F10 on a focused row open its actions.
pub(super) fn attach_menu_keys(
    widget: &gtk::Widget,
    item: &gtk::ListItem,
    window: &glib::WeakRef<WaddleWindow>,
) {
    let shortcuts = gtk::ShortcutController::new();
    shortcuts.set_scope(gtk::ShortcutScope::Local);
    let (item, window) = (item.downgrade(), window.clone());
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Menu|<Shift>F10"),
        Some(gtk::CallbackAction::new(move |_, _| {
            if let (Some(item), Some(window)) = (item.upgrade(), window.upgrade()) {
                window.show_message_actions(&item, 24.0, 16.0);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        })),
    ));
    widget.add_controller(shortcuts);
}
