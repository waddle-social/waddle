// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Sidebar widget for displaying Waddles, Channels, and DMs.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, List, ListItem, Widget},
};

use crate::app::{App, SidebarItem};

/// Sidebar widget showing the tree structure of Waddles/Channels/DMs
pub struct SidebarWidget<'a> {
    app: &'a App,
    block: Option<Block<'a>>,
}

impl<'a> SidebarWidget<'a> {
    pub fn new(app: &'a App) -> Self {
        Self { app, block: None }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Convert a sidebar item to a styled list item, with presence indicator for DMs.
    fn item_to_list_item(&self, item: &SidebarItem, selected: bool) -> ListItem<'static> {
        let (text, style) = match item {
            SidebarItem::WaddleHeader => (
                "🐧 Waddles".to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            SidebarItem::ChannelHeader => (
                "📢 Channels".to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            SidebarItem::DmHeader => (
                "💬 DMs".to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            SidebarItem::Waddle { name, .. } => {
                let prefix = if selected { "▸ " } else { "  " };
                (
                    format!("{}{}", prefix, name),
                    if selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    },
                )
            }
            SidebarItem::Channel { id: _, name } => {
                let prefix = if selected { "▸ " } else { "  " };
                // Look up unread by trying all view keys that end with the channel name
                let room_name = name.trim_start_matches('#');
                let unread = self.app.unread_for_channel(room_name);
                let badge = if unread > 0 {
                    format!(" ({})", unread)
                } else {
                    String::new()
                };
                (
                    format!("{}{}{}", prefix, name, badge),
                    if selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else if unread > 0 {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Green)
                    },
                )
            }
            SidebarItem::DirectMessage { id, name } => {
                let prefix = if selected { "▸ " } else { "  " };
                // Add presence indicator
                let indicator = id
                    .parse::<xmpp_parsers::jid::BareJid>()
                    .ok()
                    .and_then(|jid| self.app.get_presence(&jid))
                    .map(|p| p.indicator())
                    .unwrap_or("○");
                let unread = self.app.unread_count(id);
                let badge = if unread > 0 {
                    format!(" ({})", unread)
                } else {
                    String::new()
                };
                (
                    format!("{}{} {}{}", prefix, indicator, name, badge),
                    if selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else if unread > 0 {
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Magenta)
                    },
                )
            }
        };

        let line = Line::from(vec![Span::styled(text, style)]);

        if selected && !item.is_header() {
            ListItem::new(line).style(Style::default().bg(Color::DarkGray))
        } else {
            ListItem::new(line)
        }
    }
}

impl<'a> Widget for SidebarWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self
            .app
            .sidebar_items
            .iter()
            .enumerate()
            .map(|(i, item)| self.item_to_list_item(item, i == self.app.sidebar_selected))
            .collect();

        let mut list = List::new(items);

        if let Some(block) = self.block {
            list = list.block(block);
        }

        Widget::render(list, area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_item_to_list_item() {
        let app = App::new();
        let widget = SidebarWidget::new(&app);
        let item = SidebarItem::Channel {
            id: "test".into(),
            name: "#test".into(),
        };
        let _list_item = widget.item_to_list_item(&item, false);
    }
}
