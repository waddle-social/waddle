// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Main layout for the Waddle TUI.
//!
//! The layout consists of three panels:
//! ```text
//! ┌─────────────┬────────────────────────────┐
//! │             │                            │
//! │   Sidebar   │         Messages           │
//! │             │                            │
//! │  (Waddles)  │                            │
//! │  (Channels) │                            │
//! │  (DMs)      │                            │
//! │             ├────────────────────────────┤
//! │             │         Input              │
//! └─────────────┴────────────────────────────┘
//! ```

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders},
    Frame,
};

use super::{InputWidget, MessagesWidget, SidebarWidget};
use crate::app::{App, ConnectionState, Focus};
use crate::config::Config;

/// Layout areas for the three panels
pub struct LayoutAreas {
    pub sidebar: Rect,
    pub messages: Rect,
    pub input: Rect,
}

/// Calculate the layout areas for the three panels
pub fn calculate_layout(area: Rect, sidebar_width: u16) -> LayoutAreas {
    // Split horizontally: sidebar | main content
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(30)])
        .split(area);

    // Split main content vertically: messages | input
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(horizontal[1]);

    LayoutAreas {
        sidebar: horizontal[0],
        messages: vertical[0],
        input: vertical[1],
    }
}

/// Get the border style for a panel based on focus
fn border_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Render the complete UI layout
pub fn render_layout(frame: &mut Frame, app: &App, config: &Config) {
    let areas = calculate_layout(frame.area(), config.ui.sidebar_width);

    // Render sidebar with connection status in title
    let (status_indicator, status_color) = match &app.connection_state {
        ConnectionState::Disconnected => ("○", Color::DarkGray),
        ConnectionState::Connecting => ("◐", Color::Yellow),
        ConnectionState::Connected => ("●", Color::Green),
        ConnectionState::Error(_) => ("✕", Color::Red),
    };

    let sidebar_title = Line::from(vec![
        Span::raw(" Waddle "),
        Span::styled(status_indicator, Style::default().fg(status_color)),
        Span::raw(" "),
    ]);

    let sidebar_block = Block::default()
        .title(sidebar_title)
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::Sidebar));

    let sidebar_widget = SidebarWidget::new(app).block(sidebar_block);
    frame.render_widget(sidebar_widget, areas.sidebar);

    // Render messages
    let messages_title = format!(" {} ", app.current_view_name);
    let messages_block = Block::default()
        .title(messages_title)
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::Messages));

    let messages_widget = MessagesWidget::new(app, config).block(messages_block);
    frame.render_widget(messages_widget, areas.messages);

    // Render input
    let input_block = Block::default()
        .title(" Message ")
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::Input));

    let input_widget = InputWidget::new(app).block(input_block);
    frame.render_widget(input_widget, areas.input);

    // Show cursor in input area when focused
    if app.focus == Focus::Input {
        // Calculate cursor position within input area
        let inner = areas.input.inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        });
        let cursor_x = inner.x + app.input_cursor as u16;
        let cursor_y = inner.y;

        // Clamp cursor to visible area
        let cursor_x = cursor_x.min(inner.x + inner.width.saturating_sub(1));

        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layout_calculation() {
        let area = Rect::new(0, 0, 100, 40);
        let layout = calculate_layout(area, 24);

        assert_eq!(layout.sidebar.width, 24);
        assert_eq!(layout.sidebar.height, 40);
        assert_eq!(layout.messages.x, 24);
        assert_eq!(layout.input.x, 24);
        assert_eq!(layout.input.height, 3);
    }
}
