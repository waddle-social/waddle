use std::collections::{HashMap, HashSet};

use waddle_core::event::{ChatMessage, ChatState, PresenceShow, RosterItem};
use waddle_core::i18n::I18n;
use waddle_core::theme::{Theme, ThemeManager};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected { jid: String },
    Syncing,
}

impl ConnectionStatus {
    #[allow(dead_code)]
    pub fn jid(&self) -> Option<&str> {
        match self {
            Self::Connected { jid } => Some(jid),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RosterEntry {
    pub item: RosterItem,
    pub presence: PresenceShow,
    pub unread: u32,
}

#[derive(Debug, Clone)]
pub struct MucRoom {
    pub jid: String,
    pub name: String,
    pub unread: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Conversation {
    pub messages: Vec<ChatMessage>,
    pub remote_chat_state: Option<ChatState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Insert,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Sidebar,
    Conversation,
}

pub struct AppState {
    pub roster: Vec<RosterEntry>,
    pub rooms: Vec<MucRoom>,
    pub conversations: HashMap<String, Conversation>,
    pub delivered_message_ids: HashSet<String>,
    pub active_conversation: Option<String>,
    pub connected_jid: Option<String>,
    pub connection_status: ConnectionStatus,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub focused_panel: Panel,
    pub sidebar_index: usize,
    pub scroll_offset: u16,
    pub i18n: I18n,
    pub theme_manager: ThemeManager,
    pub theme: Theme,
    pub command_feedback: Option<String>,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(i18n: I18n, theme_manager: ThemeManager, theme: Theme) -> Self {
        Self {
            roster: Vec::new(),
            rooms: Vec::new(),
            conversations: HashMap::new(),
            delivered_message_ids: HashSet::new(),
            active_conversation: None,
            connected_jid: None,
            connection_status: ConnectionStatus::Disconnected,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            focused_panel: Panel::Sidebar,
            sidebar_index: 0,
            scroll_offset: 0,
            i18n,
            theme_manager,
            theme,
            command_feedback: None,
            should_quit: false,
        }
    }

    pub fn sidebar_items_count(&self) -> usize {
        self.roster.len() + self.rooms.len()
    }

    pub fn selected_jid(&self) -> Option<String> {
        let roster_len = self.roster.len();
        if self.sidebar_index < roster_len {
            self.roster
                .get(self.sidebar_index)
                .map(|e| e.item.jid.clone())
        } else {
            self.rooms
                .get(self.sidebar_index - roster_len)
                .map(|r| r.jid.clone())
        }
    }

    pub fn active_conversation_data(&self) -> Option<&Conversation> {
        self.active_conversation
            .as_ref()
            .and_then(|jid| self.conversations.get(jid))
    }

    pub fn ensure_conversation(&mut self, jid: &str) -> &mut Conversation {
        self.conversations.entry(jid.to_string()).or_default()
    }

    pub fn mark_conversation_read(&mut self, jid: &str) {
        if let Some(entry) = self.roster.iter_mut().find(|entry| entry.item.jid == jid) {
            entry.unread = 0;
        }
        if let Some(room) = self.rooms.iter_mut().find(|room| room.jid == jid) {
            room.unread = 0;
        }
    }
}
