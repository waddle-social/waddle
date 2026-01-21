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

    /// Convert a sidebar item to a styled list item
    fn item_to_list_item(item: &SidebarItem, selected: bool) -> ListItem<'static> {
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
            SidebarItem::Channel { name, .. } => {
                let prefix = if selected { "▸ " } else { "  " };
                (
                    format!("{}{}", prefix, name),
                    if selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Green)
                    },
                )
            }
            SidebarItem::DirectMessage { name, .. } => {
                let prefix = if selected { "▸ " } else { "  " };
                (
                    format!("{}{}", prefix, name),
                    if selected {
                        Style::default()
                            .fg(Color::Cyan)
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
        // Create list items from sidebar items
        let items: Vec<ListItem> = self
            .app
            .sidebar_items
            .iter()
            .enumerate()
            .map(|(i, item)| Self::item_to_list_item(item, i == self.app.sidebar_selected))
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
        let item = SidebarItem::Channel {
            id: "test".into(),
            name: "#test".into(),
        };
        let _list_item = SidebarWidget::item_to_list_item(&item, false);
        // Just test it doesn't panic
    }
}
