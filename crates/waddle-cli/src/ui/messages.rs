// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Messages widget for displaying channel/DM messages.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget, Wrap},
};

use crate::app::App;
use crate::config::Config;

/// Messages widget showing the message history
pub struct MessagesWidget<'a> {
    app: &'a App,
    config: &'a Config,
    block: Option<Block<'a>>,
}

impl<'a> MessagesWidget<'a> {
    pub fn new(app: &'a App, config: &'a Config) -> Self {
        Self {
            app,
            config,
            block: None,
        }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// Format a message for display
    fn format_message(&self, msg: &crate::app::Message) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Format timestamp
        let timestamp = if self.config.ui.show_timestamps {
            let formatted = msg.timestamp.format(&self.config.ui.time_format).to_string();
            format!("[{}] ", formatted)
        } else {
            String::new()
        };

        // Author style
        let author_style = Style::default()
            .fg(author_color(&msg.author))
            .add_modifier(Modifier::BOLD);

        // Build the header line: [timestamp] <author>
        let header = Line::from(vec![
            Span::styled(timestamp, Style::default().fg(Color::DarkGray)),
            Span::styled(format!("<{}>", msg.author), author_style),
        ]);
        lines.push(header);

        // Content line(s)
        let content = Line::from(vec![Span::styled(
            format!("  {}", msg.content),
            Style::default().fg(Color::White),
        )]);
        lines.push(content);

        // Empty line for spacing
        lines.push(Line::default());

        lines
    }
}

/// Generate a consistent color for a username
fn author_color(author: &str) -> Color {
    // Simple hash to color mapping
    let hash: u32 = author.chars().map(|c| c as u32).sum();
    let colors = [
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
    ];
    colors[hash as usize % colors.len()]
}

impl<'a> Widget for MessagesWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Collect all formatted lines
        let mut all_lines: Vec<Line<'static>> = Vec::new();

        // Add a welcome message if no messages
        if self.app.messages.is_empty() {
            all_lines.push(Line::from(vec![Span::styled(
                "No messages yet. Say something!",
                Style::default().fg(Color::DarkGray),
            )]));
        } else {
            for msg in &self.app.messages {
                all_lines.extend(self.format_message(msg));
            }
        }

        // Calculate scroll offset
        let visible_height = self.block.as_ref().map_or(area.height, |_| {
            area.height.saturating_sub(2) // Account for borders
        }) as usize;

        let total_lines = all_lines.len();
        let scroll_offset = if total_lines > visible_height {
            // Scroll from bottom by default, adjusted by message_scroll
            let max_scroll = total_lines.saturating_sub(visible_height);
            max_scroll.saturating_sub(self.app.message_scroll)
        } else {
            0
        };

        let mut paragraph = Paragraph::new(all_lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_offset as u16, 0));

        if let Some(block) = self.block {
            paragraph = paragraph.block(block);
        }

        Widget::render(paragraph, area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_author_color() {
        // Same author should always get same color
        let color1 = author_color("alice");
        let color2 = author_color("alice");
        assert_eq!(color1, color2);

        // Different authors might get different colors
        let color3 = author_color("bob");
        // (color3 might or might not equal color1, depending on hash collision)
        let _ = color3; // Just ensure it doesn't panic
    }
}
