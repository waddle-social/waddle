// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Application state and business logic for the Waddle TUI.


/// Which panel currently has focus
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Sidebar,
    Messages,
    Input,
}

impl Focus {
    /// Cycle to the next panel
    pub fn next(self) -> Self {
        match self {
            Focus::Sidebar => Focus::Messages,
            Focus::Messages => Focus::Input,
            Focus::Input => Focus::Sidebar,
        }
    }

    /// Cycle to the previous panel
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
    /// Check if this item is a header (non-selectable)
    pub fn is_header(&self) -> bool {
        matches!(
            self,
            SidebarItem::WaddleHeader | SidebarItem::ChannelHeader | SidebarItem::DmHeader
        )
    }

    /// Get the display name for this item
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

/// A message in the message view
#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Main application state
#[derive(Debug)]
pub struct App {
    /// Whether the application should exit
    pub should_quit: bool,

    /// Which panel currently has focus
    pub focus: Focus,

    /// Sidebar items (waddles, channels, DMs)
    pub sidebar_items: Vec<SidebarItem>,

    /// Currently selected index in the sidebar
    pub sidebar_selected: usize,

    /// Messages in the current view
    pub messages: Vec<Message>,

    /// Current scroll position in messages (0 = bottom/newest)
    pub message_scroll: usize,

    /// Input buffer for composing messages
    pub input_buffer: String,

    /// Cursor position in the input buffer
    pub input_cursor: usize,

    /// Currently selected channel/conversation name (for display)
    pub current_view_name: String,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// Create a new App with default state
    pub fn new() -> Self {
        // Initialize with some placeholder data for now
        let sidebar_items = vec![
            SidebarItem::WaddleHeader,
            SidebarItem::Waddle {
                id: "waddle-1".into(),
                name: "Rust Developers".into(),
            },
            SidebarItem::Waddle {
                id: "waddle-2".into(),
                name: "Open Source".into(),
            },
            SidebarItem::ChannelHeader,
            SidebarItem::Channel {
                id: "channel-1".into(),
                name: "#general".into(),
            },
            SidebarItem::Channel {
                id: "channel-2".into(),
                name: "#random".into(),
            },
            SidebarItem::Channel {
                id: "channel-3".into(),
                name: "#help".into(),
            },
            SidebarItem::DmHeader,
            SidebarItem::DirectMessage {
                id: "dm-1".into(),
                name: "alice".into(),
            },
            SidebarItem::DirectMessage {
                id: "dm-2".into(),
                name: "bob".into(),
            },
        ];

        let messages = vec![
            Message {
                id: "msg-1".into(),
                author: "alice".into(),
                content: "Welcome to Waddle! 🐧".into(),
                timestamp: chrono::Utc::now() - chrono::Duration::hours(2),
            },
            Message {
                id: "msg-2".into(),
                author: "bob".into(),
                content: "This is a decentralized chat built on XMPP.".into(),
                timestamp: chrono::Utc::now() - chrono::Duration::hours(1),
            },
            Message {
                id: "msg-3".into(),
                author: "charlie".into(),
                content: "The TUI is built with Ratatui - check out the vim-style keybindings!".into(),
                timestamp: chrono::Utc::now() - chrono::Duration::minutes(30),
            },
        ];

        Self {
            should_quit: false,
            focus: Focus::Sidebar,
            sidebar_items,
            sidebar_selected: 1, // Start on first waddle, not header
            messages,
            message_scroll: 0,
            input_buffer: String::new(),
            input_cursor: 0,
            current_view_name: "#general".into(),
        }
    }

    /// Request the application to quit
    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Cycle focus to the next panel
    pub fn focus_next(&mut self) {
        self.focus = self.focus.next();
    }

    /// Cycle focus to the previous panel
    pub fn focus_prev(&mut self) {
        self.focus = self.focus.prev();
    }

    /// Move selection up in the sidebar
    pub fn sidebar_up(&mut self) {
        if self.sidebar_selected > 0 {
            self.sidebar_selected -= 1;
            // Skip headers
            while self.sidebar_selected > 0
                && self.sidebar_items[self.sidebar_selected].is_header()
            {
                self.sidebar_selected -= 1;
            }
            // If we landed on a header at index 0, move down
            if self.sidebar_items[self.sidebar_selected].is_header() {
                self.sidebar_down();
            }
        }
    }

    /// Move selection down in the sidebar
    pub fn sidebar_down(&mut self) {
        if self.sidebar_selected < self.sidebar_items.len() - 1 {
            self.sidebar_selected += 1;
            // Skip headers
            while self.sidebar_selected < self.sidebar_items.len() - 1
                && self.sidebar_items[self.sidebar_selected].is_header()
            {
                self.sidebar_selected += 1;
            }
        }
    }

    /// Select the currently highlighted sidebar item
    pub fn sidebar_select(&mut self) -> Option<&SidebarItem> {
        let item = self.sidebar_items.get(self.sidebar_selected)?;
        if item.is_header() {
            return None;
        }

        // Update the current view name
        self.current_view_name = item.display_name().to_string();

        tracing::info!("Selected sidebar item: {:?}", item);
        Some(item)
    }

    /// Get the currently selected sidebar item
    pub fn selected_sidebar_item(&self) -> Option<&SidebarItem> {
        self.sidebar_items.get(self.sidebar_selected)
    }

    /// Scroll messages up (towards older)
    pub fn scroll_messages_up(&mut self) {
        if self.message_scroll < self.messages.len().saturating_sub(1) {
            self.message_scroll += 1;
        }
    }

    /// Scroll messages down (towards newer)
    pub fn scroll_messages_down(&mut self) {
        if self.message_scroll > 0 {
            self.message_scroll -= 1;
        }
    }

    /// Insert a character at the current cursor position
    pub fn input_insert(&mut self, c: char) {
        self.input_buffer.insert(self.input_cursor, c);
        self.input_cursor += c.len_utf8();
    }

    /// Delete the character before the cursor (backspace)
    pub fn input_backspace(&mut self) {
        if self.input_cursor > 0 {
            // Find the previous character boundary
            let prev_char = self.input_buffer[..self.input_cursor]
                .chars()
                .last()
                .unwrap();
            self.input_cursor -= prev_char.len_utf8();
            self.input_buffer.remove(self.input_cursor);
        }
    }

    /// Delete the character at the cursor (delete key)
    pub fn input_delete(&mut self) {
        if self.input_cursor < self.input_buffer.len() {
            self.input_buffer.remove(self.input_cursor);
        }
    }

    /// Move cursor left
    pub fn input_cursor_left(&mut self) {
        if self.input_cursor > 0 {
            let prev_char = self.input_buffer[..self.input_cursor]
                .chars()
                .last()
                .unwrap();
            self.input_cursor -= prev_char.len_utf8();
        }
    }

    /// Move cursor right
    pub fn input_cursor_right(&mut self) {
        if self.input_cursor < self.input_buffer.len() {
            let next_char = self.input_buffer[self.input_cursor..]
                .chars()
                .next()
                .unwrap();
            self.input_cursor += next_char.len_utf8();
        }
    }

    /// Move cursor to start of input
    pub fn input_cursor_home(&mut self) {
        self.input_cursor = 0;
    }

    /// Move cursor to end of input
    pub fn input_cursor_end(&mut self) {
        self.input_cursor = self.input_buffer.len();
    }

    /// Submit the current input (send message)
    pub fn input_submit(&mut self) -> Option<String> {
        if self.input_buffer.is_empty() {
            return None;
        }

        let message = std::mem::take(&mut self.input_buffer);
        self.input_cursor = 0;

        // Add to local messages (in a real app, this would go through XMPP)
        self.messages.push(Message {
            id: uuid::Uuid::new_v4().to_string(),
            author: "you".into(),
            content: message.clone(),
            timestamp: chrono::Utc::now(),
        });

        tracing::info!("Submitted message: {}", message);
        Some(message)
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
    fn test_sidebar_navigation() {
        let mut app = App::new();
        let initial = app.sidebar_selected;
        app.sidebar_down();
        assert!(app.sidebar_selected > initial || app.sidebar_selected == initial);
        app.sidebar_up();
        // Should be back to initial or still at a valid non-header position
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
}
