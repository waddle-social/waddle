// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Input widget for composing messages.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget},
};

use crate::app::App;

/// Input widget for message composition
pub struct InputWidget<'a> {
    app: &'a App,
    block: Option<Block<'a>>,
}

impl<'a> InputWidget<'a> {
    pub fn new(app: &'a App) -> Self {
        Self { app, block: None }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }
}

impl<'a> Widget for InputWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let text = if self.app.input_buffer.is_empty() {
            Line::from(vec![Span::styled(
                "Type a message...",
                Style::default().fg(Color::DarkGray),
            )])
        } else {
            Line::from(vec![Span::styled(
                self.app.input_buffer.clone(),
                Style::default().fg(Color::White),
            )])
        };

        let mut paragraph = Paragraph::new(text);

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
    fn test_input_widget_creation() {
        let app = App::new();
        let _widget = InputWidget::new(&app);
        // Just test it doesn't panic
    }
}
