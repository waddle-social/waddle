// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Application state and business logic for the Waddle TUI.

use std::collections::HashMap;
use std::sync::Arc;

use xmpp_parsers::jid::BareJid;

use crate::embed::{EmbedProcessor, NoopEmbedProcessor};
use crate::stanza::RawEmbed;

/// Maximum messages to retain per room/conversation.
const MAX_MESSAGES_PER_ROOM: usize = 1000;

/// Connection state for the XMPP client
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Error(String),
    /// Reconnecting with countdown.
    Reconnecting {
        attempt: u32,
        countdown_secs: f64,
    },
}

impl ConnectionState {
    pub fn display(&self) -> String {
        match self {
            ConnectionState::Disconnected => "Disconnected".to_string(),
            ConnectionState::Connecting => "Connecting...".to_string(),
            ConnectionState::Connected => "Connected".to_string(),
            ConnectionState::Error(_) => "Error".to_string(),
            ConnectionState::Reconnecting {
                attempt,
                countdown_secs,
            } => format!("Retry #{} in {:.0}s", attempt, countdown_secs),
        }
    }

    pub fn indicator(&self) -> &str {
        match self {
            ConnectionState::Disconnected => "○",
            ConnectionState::Connecting | ConnectionState::Reconnecting { .. } => "◐",
            ConnectionState::Connected => "●",
            ConnectionState::Error(_) => "✕",
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionState::Connected)
    }
}

/// Which panel currently has focus
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Sidebar,
    Messages,
    Input,
}

impl Focus {
    pub fn next(self) -> Self {
        match self {
            Focus::Sidebar => Focus::Messages,
            Focus::Messages => Focus::Input,
            Focus::Input => Focus::Sidebar,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Focus::Sidebar => Focus::Input,
            Focus::Messages => Focus::Sidebar,
            Focus::Input => Focus::Messages,
        }
    }
}

/// Represents a selectable item in the sidebar
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarItem {
    WaddleHeader,
    Waddle { id: String, name: String },
    ChannelHeader,
    Channel { id: String, name: String },
    DmHeader,
    DirectMessage { id: String, name: String },
}

impl SidebarItem {
    pub fn is_header(&self) -> bool {
        matches!(
            self,
            SidebarItem::WaddleHeader | SidebarItem::ChannelHeader | SidebarItem::DmHeader
        )
    }

    pub fn display_name(&self) -> &str {
        match self {
            SidebarItem::WaddleHeader => "🐧 Waddles",
            SidebarItem::Waddle { name, .. } => name,
            SidebarItem::ChannelHeader => "📢 Channels",
            SidebarItem::Channel { name, .. } => name,
            SidebarItem::DmHeader => "💬 Direct Messages",
            SidebarItem::DirectMessage { name, .. } => name,
        }
    }
}

/// A message in the message view.
#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Raw embed payloads from the XMPP stanza (processed by plugins).
    pub embeds: Vec<RawEmbed>,
}

/// Presence state for a contact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactPresence {
    pub available: bool,
    pub show: Option<String>,
}

impl ContactPresence {
    /// Presence indicator for sidebar display.
    pub fn indicator(&self) -> &str {
        if !self.available {
            return "○";
        }
        match self.show.as_deref() {
            Some("Away") | Some("Xa") => "◐",
            Some("Dnd") => "⊘",
            _ => "●",
        }
    }
}

/// Main application state
pub struct App {
    pub should_quit: bool,
    pub focus: Focus,
    pub sidebar_items: Vec<SidebarItem>,
    pub sidebar_selected: usize,
    /// Per-room/conversation message storage.
    room_messages: HashMap<String, Vec<Message>>,
    pub message_scroll: usize,
    pub input_buffer: String,
    pub input_cursor: usize,
    pub current_view_name: String,
    /// Key into room_messages for the active view.
    current_view_key: Option<String>,
    pub connection_state: ConnectionState,
    pub current_room_jid: Option<BareJid>,
    pub current_dm_jid: Option<BareJid>,
    pub joined_rooms: std::collections::HashSet<BareJid>,
    pub own_jid: Option<BareJid>,
    pub nickname: String,
    /// Contact presence states.
    pub presence_map: HashMap<BareJid, ContactPresence>,
    /// Roster contacts (JID -> display name).
    pub roster: HashMap<BareJid, Option<String>>,
    /// Whether MAM history is currently loading.
    pub mam_loading: bool,
    /// Unread message counts per view key.
    pub unread_counts: HashMap<String, usize>,
    /// Embed processor (plugin pipeline).
    embed_processor: Arc<dyn EmbedProcessor>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("focus", &self.focus)
            .field("connection_state", &self.connection_state)
            .field("current_view_name", &self.current_view_name)
            .field("nickname", &self.nickname)
            .finish_non_exhaustive()
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let sidebar_items = vec![
            SidebarItem::WaddleHeader,
            SidebarItem::ChannelHeader,
            SidebarItem::DmHeader,
        ];

        Self {
            should_quit: false,
            focus: Focus::Sidebar,
            sidebar_items,
            sidebar_selected: 0,
            room_messages: HashMap::new(),
            message_scroll: 0,
            input_buffer: String::new(),
            input_cursor: 0,
            current_view_name: "Welcome".into(),
            current_view_key: None,
            connection_state: ConnectionState::Disconnected,
            current_room_jid: None,
            current_dm_jid: None,
            joined_rooms: std::collections::HashSet::new(),
            own_jid: None,
            nickname: "user".into(),
            presence_map: HashMap::new(),
            roster: HashMap::new(),
            mam_loading: false,
            unread_counts: HashMap::new(),
            embed_processor: Arc::new(NoopEmbedProcessor),
        }
    }

    /// Set a custom embed processor (for plugin integration).
    pub fn set_embed_processor(&mut self, processor: Arc<dyn EmbedProcessor>) {
        self.embed_processor = processor;
    }

    /// Get messages for the current active view.
    pub fn messages(&self) -> &[Message] {
        self.current_view_key
            .as_ref()
            .and_then(|key| self.room_messages.get(key))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Add a message to a specific room/conversation.
    pub fn add_message_to(
        &mut self,
        view_key: &str,
        author: String,
        content: String,
        embeds: Vec<RawEmbed>,
    ) {
        self.add_message_to_with_id(view_key, None, author, content, embeds);
    }

    /// Add a message with an optional ID for deduplication.
    pub fn add_message_to_with_id(
        &mut self,
        view_key: &str,
        id: Option<String>,
        author: String,
        content: String,
        embeds: Vec<RawEmbed>,
    ) {
        let messages = self.room_messages.entry(view_key.to_string()).or_default();

        // Deduplicate by message ID
        if let Some(ref msg_id) = id {
            if !msg_id.is_empty() && messages.iter().any(|m| m.id == *msg_id) {
                return;
            }
        }

        messages.push(Message {
            id: id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            author,
            content,
            timestamp: chrono::Utc::now(),
            embeds,
        });

        // Cap message buffer
        if messages.len() > MAX_MESSAGES_PER_ROOM {
            let excess = messages.len() - MAX_MESSAGES_PER_ROOM;
            messages.drain(..excess);
        }

        // Auto-scroll to newest if viewing this room, otherwise increment unread
        if self.current_view_key.as_deref() == Some(view_key) {
            self.message_scroll = 0;
        } else {
            *self.unread_counts.entry(view_key.to_string()).or_insert(0) += 1;
        }
    }

    /// Prepend a historical message (from MAM) to a room.
    pub fn prepend_message(
        &mut self,
        view_key: &str,
        id: Option<String>,
        author: String,
        content: String,
        embeds: Vec<RawEmbed>,
        timestamp: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        let messages = self.room_messages.entry(view_key.to_string()).or_default();

        // Deduplicate
        if let Some(ref msg_id) = id {
            if !msg_id.is_empty() && messages.iter().any(|m| m.id == *msg_id) {
                return;
            }
        }

        messages.insert(
            0,
            Message {
                id: id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                author,
                content,
                timestamp: timestamp.unwrap_or_else(chrono::Utc::now),
                embeds,
            },
        );

        // Cap from front if needed
        if messages.len() > MAX_MESSAGES_PER_ROOM {
            messages.truncate(MAX_MESSAGES_PER_ROOM);
        }
    }

    /// Convenience: add a message to the current view.
    pub fn add_message(&mut self, author: String, content: String) {
        if let Some(key) = self.current_view_key.clone() {
            self.add_message_to(&key, author, content, vec![]);
        }
    }

    /// Populate sidebar with waddles and channels from the API.
    pub fn set_waddles_and_channels(
        &mut self,
        waddles: Vec<(String, String)>,
        channels: Vec<(String, String)>,
    ) {
        let mut items = vec![SidebarItem::WaddleHeader];
        for (id, name) in waddles {
            items.push(SidebarItem::Waddle { id, name });
        }
        items.push(SidebarItem::ChannelHeader);
        for (id, name) in channels {
            items.push(SidebarItem::Channel {
                id,
                name: format!("#{}", name),
            });
        }
        items.push(SidebarItem::DmHeader);

        // Add roster contacts to DM section
        for (jid, name) in &self.roster {
            let display = name.as_ref().cloned().unwrap_or_else(|| jid.to_string());
            items.push(SidebarItem::DirectMessage {
                id: jid.to_string(),
                name: display,
            });
        }

        self.sidebar_items = items;
        self.sidebar_selected = self
            .sidebar_items
            .iter()
            .position(|item| !item.is_header())
            .unwrap_or(0);
    }

    /// Add a roster contact and refresh sidebar DMs.
    pub fn add_roster_contact(&mut self, jid: BareJid, name: Option<String>) {
        self.roster.insert(jid.clone(), name.clone());

        // Add to sidebar if not already present
        let id = jid.to_string();
        if !self
            .sidebar_items
            .iter()
            .any(|item| matches!(item, SidebarItem::DirectMessage { id: did, .. } if *did == id))
        {
            let display = name.unwrap_or_else(|| jid.to_string());
            self.sidebar_items
                .push(SidebarItem::DirectMessage { id, name: display });
        }
    }

    /// Update contact presence.
    pub fn update_presence(&mut self, jid: BareJid, available: bool, show: Option<String>) {
        self.presence_map
            .insert(jid, ContactPresence { available, show });
    }

    /// Get presence for a contact.
    pub fn get_presence(&self, jid: &BareJid) -> Option<&ContactPresence> {
        self.presence_map.get(jid)
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    pub fn focus_next(&mut self) {
        self.focus = self.focus.next();
    }

    pub fn focus_prev(&mut self) {
        self.focus = self.focus.prev();
    }

    pub fn sidebar_up(&mut self) {
        if self.sidebar_selected > 0 {
            self.sidebar_selected -= 1;
            while self.sidebar_selected > 0 && self.sidebar_items[self.sidebar_selected].is_header()
            {
                self.sidebar_selected -= 1;
            }
            if self.sidebar_items[self.sidebar_selected].is_header() {
                self.sidebar_down();
            }
        }
    }

    pub fn sidebar_down(&mut self) {
        if self.sidebar_selected < self.sidebar_items.len() - 1 {
            self.sidebar_selected += 1;
            while self.sidebar_selected < self.sidebar_items.len() - 1
                && self.sidebar_items[self.sidebar_selected].is_header()
            {
                self.sidebar_selected += 1;
            }
        }
    }

    pub fn sidebar_select(&mut self) -> Option<&SidebarItem> {
        let item = self.sidebar_items.get(self.sidebar_selected)?;
        if item.is_header() {
            return None;
        }
        self.current_view_name = item.display_name().to_string();
        tracing::info!("Selected sidebar item: {:?}", item);
        Some(item)
    }

    pub fn selected_sidebar_item(&self) -> Option<&SidebarItem> {
        self.sidebar_items.get(self.sidebar_selected)
    }

    pub fn scroll_messages_up(&mut self) {
        let msg_count = self.messages().len();
        if self.message_scroll < msg_count.saturating_sub(1) {
            self.message_scroll += 1;
        }
    }

    pub fn scroll_messages_down(&mut self) {
        if self.message_scroll > 0 {
            self.message_scroll -= 1;
        }
    }

    pub fn input_insert(&mut self, c: char) {
        self.input_buffer.insert(self.input_cursor, c);
        self.input_cursor += c.len_utf8();
    }

    pub fn input_backspace(&mut self) {
        if self.input_cursor > 0 {
            let prev_char = self.input_buffer[..self.input_cursor]
                .chars()
                .last()
                .unwrap();
            self.input_cursor -= prev_char.len_utf8();
            self.input_buffer.remove(self.input_cursor);
        }
    }

    pub fn input_delete(&mut self) {
        if self.input_cursor < self.input_buffer.len() {
            self.input_buffer.remove(self.input_cursor);
        }
    }

    pub fn input_cursor_left(&mut self) {
        if self.input_cursor > 0 {
            let prev_char = self.input_buffer[..self.input_cursor]
                .chars()
                .last()
                .unwrap();
            self.input_cursor -= prev_char.len_utf8();
        }
    }

    pub fn input_cursor_right(&mut self) {
        if self.input_cursor < self.input_buffer.len() {
            let next_char = self.input_buffer[self.input_cursor..]
                .chars()
                .next()
                .unwrap();
            self.input_cursor += next_char.len_utf8();
        }
    }

    pub fn input_cursor_home(&mut self) {
        self.input_cursor = 0;
    }

    pub fn input_cursor_end(&mut self) {
        self.input_cursor = self.input_buffer.len();
    }

    pub fn input_submit(&mut self) -> Option<String> {
        if self.input_buffer.is_empty() {
            return None;
        }
        let message = std::mem::take(&mut self.input_buffer);
        self.input_cursor = 0;
        Some(message)
    }

    pub fn set_connection_state(&mut self, state: ConnectionState) {
        tracing::info!("Connection state: {:?}", state);
        self.connection_state = state;
    }

    pub fn room_joined(&mut self, room_jid: BareJid) {
        tracing::info!("Room joined: {}", room_jid);
        self.joined_rooms.insert(room_jid);
    }

    pub fn room_left(&mut self, room_jid: &BareJid) {
        tracing::info!("Room left: {}", room_jid);
        self.joined_rooms.remove(room_jid);
    }

    pub fn is_in_room(&self, room_jid: &BareJid) -> bool {
        self.joined_rooms.contains(room_jid)
    }

    pub fn set_current_room(&mut self, room_jid: Option<BareJid>) {
        self.current_room_jid = room_jid.clone();
        self.current_dm_jid = None;
        if let Some(jid) = room_jid {
            let key = jid.to_string();
            if let Some(node) = jid.node() {
                self.current_view_name = format!("#{}", node);
            } else {
                self.current_view_name = jid.to_string();
            }
            self.unread_counts.remove(&key);
            self.current_view_key = Some(key);
            self.message_scroll = 0;
        } else {
            self.current_view_key = None;
        }
    }

    pub fn set_current_dm(&mut self, dm_jid: Option<BareJid>) {
        self.current_dm_jid = dm_jid.clone();
        self.current_room_jid = None;
        if let Some(jid) = dm_jid {
            let key = jid.to_string();
            if let Some(node) = jid.node() {
                self.current_view_name = format!("@{}", node);
            } else {
                self.current_view_name = jid.to_string();
            }
            self.unread_counts.remove(&key);
            self.current_view_key = Some(key);
            self.message_scroll = 0;
        } else {
            self.current_view_key = None;
        }
    }

    /// Get unread count for a view key.
    pub fn unread_count(&self, view_key: &str) -> usize {
        self.unread_counts.get(view_key).copied().unwrap_or(0)
    }

    /// Get unread count for a channel by name (matches `name@*` in view keys).
    pub fn unread_for_channel(&self, channel_name: &str) -> usize {
        self.unread_counts
            .iter()
            .filter(|(key, _)| {
                // View keys for rooms are "room@muc.domain" — match the local part
                key.split('@').next() == Some(channel_name)
            })
            .map(|(_, count)| count)
            .sum()
    }

    pub fn clear_xmpp_state(&mut self) {
        self.joined_rooms.clear();
        self.current_room_jid = None;
        self.current_dm_jid = None;
        self.connection_state = ConnectionState::Disconnected;
        self.presence_map.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_focus_cycling() {
        let focus = Focus::Sidebar;
        assert_eq!(focus.next(), Focus::Messages);
        assert_eq!(focus.next().next(), Focus::Input);
        assert_eq!(focus.next().next().next(), Focus::Sidebar);
    }

    #[test]
    fn test_per_room_messages() {
        let mut app = App::new();
        app.add_message_to("room1", "alice".into(), "hello".into(), vec![]);
        app.add_message_to("room2", "bob".into(), "world".into(), vec![]);

        // No view selected -> empty
        assert!(app.messages().is_empty());

        // Select room1
        app.current_view_key = Some("room1".into());
        assert_eq!(app.messages().len(), 1);
        assert_eq!(app.messages()[0].author, "alice");

        // Select room2
        app.current_view_key = Some("room2".into());
        assert_eq!(app.messages().len(), 1);
        assert_eq!(app.messages()[0].author, "bob");
    }

    #[test]
    fn test_message_dedup() {
        let mut app = App::new();
        app.add_message_to_with_id("room", Some("id1".into()), "a".into(), "hi".into(), vec![]);
        app.add_message_to_with_id("room", Some("id1".into()), "a".into(), "hi".into(), vec![]);

        app.current_view_key = Some("room".into());
        assert_eq!(app.messages().len(), 1);
    }

    #[test]
    fn test_message_cap() {
        let mut app = App::new();
        for i in 0..1100 {
            app.add_message_to("room", "a".into(), format!("msg {i}"), vec![]);
        }
        app.current_view_key = Some("room".into());
        assert!(app.messages().len() <= MAX_MESSAGES_PER_ROOM);
    }

    #[test]
    fn test_prepend_message() {
        let mut app = App::new();
        app.add_message_to("room", "a".into(), "recent".into(), vec![]);
        app.prepend_message("room", None, "b".into(), "old".into(), vec![], None);

        app.current_view_key = Some("room".into());
        assert_eq!(app.messages()[0].content, "old");
        assert_eq!(app.messages()[1].content, "recent");
    }

    #[test]
    fn test_presence_tracking() {
        let mut app = App::new();
        let jid: BareJid = "alice@example.com".parse().unwrap();
        app.update_presence(jid.clone(), true, None);
        let p = app.get_presence(&jid).unwrap();
        assert!(p.available);
        assert_eq!(p.indicator(), "●");

        app.update_presence(jid.clone(), true, Some("Away".into()));
        assert_eq!(app.get_presence(&jid).unwrap().indicator(), "◐");

        app.update_presence(jid.clone(), false, None);
        assert_eq!(app.get_presence(&jid).unwrap().indicator(), "○");
    }

    #[test]
    fn test_input_operations() {
        let mut app = App::new();
        app.input_insert('H');
        app.input_insert('i');
        assert_eq!(app.input_buffer, "Hi");
        assert_eq!(app.input_cursor, 2);

        app.input_backspace();
        assert_eq!(app.input_buffer, "H");
        assert_eq!(app.input_cursor, 1);
    }

    #[test]
    fn test_connection_state_display() {
        let state = ConnectionState::Reconnecting {
            attempt: 3,
            countdown_secs: 4.5,
        };
        let display = state.display();
        assert!(display.starts_with("Retry #3 in "), "got: {display}");
        assert_eq!(state.indicator(), "◐");
    }

    #[test]
    fn test_unread_counts() {
        let mut app = App::new();
        // Set active view to room1
        app.current_view_key = Some("room1@muc.example.com".into());

        // Message to active view — no unread
        app.add_message_to("room1@muc.example.com", "a".into(), "hi".into(), vec![]);
        assert_eq!(app.unread_count("room1@muc.example.com"), 0);

        // Message to inactive view — unread increments
        app.add_message_to("room2@muc.example.com", "b".into(), "yo".into(), vec![]);
        assert_eq!(app.unread_count("room2@muc.example.com"), 1);
        app.add_message_to("room2@muc.example.com", "b".into(), "hey".into(), vec![]);
        assert_eq!(app.unread_count("room2@muc.example.com"), 2);

        // Channel name lookup
        assert_eq!(app.unread_for_channel("room2"), 2);
        assert_eq!(app.unread_for_channel("room1"), 0);

        // Switching view clears unread
        let jid: BareJid = "room2@muc.example.com".parse().unwrap();
        app.set_current_room(Some(jid));
        assert_eq!(app.unread_count("room2@muc.example.com"), 0);
    }

    #[test]
    fn test_sidebar_navigation() {
        let mut app = App::new();
        let initial = app.sidebar_selected;
        app.sidebar_down();
        assert!(app.sidebar_selected >= initial);
    }
}
