use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::state::{AppState, ConnectionStatus, InputMode, Panel};
use waddle_core::event::PresenceShow;

pub fn draw(frame: &mut Frame, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(chunks[0]);

    draw_sidebar(frame, state, main_chunks[0]);
    draw_conversation(frame, state, main_chunks[1]);
    draw_status_bar(frame, state, chunks[1]);
    draw_input(frame, state, chunks[2]);
}

fn presence_indicator(show: &PresenceShow) -> (&str, Color) {
    match show {
        PresenceShow::Available | PresenceShow::Chat => ("●", Color::Green),
        PresenceShow::Away => ("●", Color::Yellow),
        PresenceShow::Xa => ("●", Color::Yellow),
        PresenceShow::Dnd => ("●", Color::Red),
        PresenceShow::Unavailable => ("○", Color::DarkGray),
    }
}

fn draw_sidebar(frame: &mut Frame, state: &AppState, area: Rect) {
    let focused = state.focused_panel == Panel::Sidebar;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Roster ")
        .borders(Borders::ALL)
        .border_style(border_style);

    let mut items: Vec<ListItem> = Vec::new();

    for (i, entry) in state.roster.iter().enumerate() {
        let (indicator, color) = presence_indicator(&entry.presence);
        let name = entry.item.name.as_deref().unwrap_or(&entry.item.jid);

        let mut spans = vec![
            Span::styled(indicator, Style::default().fg(color)),
            Span::raw(" "),
        ];

        let is_selected = focused && i == state.sidebar_index;
        let is_active = state.active_conversation.as_deref() == Some(&entry.item.jid);

        let name_style = if is_active {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        spans.push(Span::styled(name.to_string(), name_style));

        if entry.unread > 0 {
            spans.push(Span::styled(
                format!(" ({})", entry.unread),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        let mut item = ListItem::new(Line::from(spans));
        if is_selected {
            item = item.style(Style::default().bg(Color::DarkGray));
        }
        items.push(item);
    }

    if !state.rooms.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "── Rooms ──",
            Style::default().fg(Color::DarkGray),
        ))));

        let roster_len = state.roster.len();
        for (i, room) in state.rooms.iter().enumerate() {
            let sidebar_i = roster_len + i;
            let is_selected = focused && sidebar_i == state.sidebar_index;
            let is_active = state.active_conversation.as_deref() == Some(&room.jid);

            let mut spans = vec![Span::raw("# ")];

            let name_style = if is_active {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            spans.push(Span::styled(room.name.clone(), name_style));

            if room.unread > 0 {
                spans.push(Span::styled(
                    format!(" ({})", room.unread),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            let mut item = ListItem::new(Line::from(spans));
            if is_selected {
                item = item.style(Style::default().bg(Color::DarkGray));
            }
            items.push(item);
        }
    }

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_conversation(frame: &mut Frame, state: &AppState, area: Rect) {
    let focused = state.focused_panel == Panel::Conversation;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = match &state.active_conversation {
        Some(jid) => format!(" {} ", jid),
        None => " No conversation selected ".to_string(),
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let conv = state.active_conversation_data();

    let inner_area = block.inner(area);
    let visible_height = inner_area.height as usize;

    match conv {
        Some(conversation) if !conversation.messages.is_empty() => {
            let mut lines: Vec<Line> = Vec::new();

            for msg in &conversation.messages {
                let time = msg.timestamp.format("%H:%M");
                let sender = msg.from.split('@').next().unwrap_or(&msg.from);

                lines.push(Line::from(vec![
                    Span::styled(format!("{time} "), Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{sender}: "),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(&msg.body),
                ]));
            }

            let skip = if lines.len() > visible_height + state.scroll_offset as usize {
                lines.len() - visible_height - state.scroll_offset as usize
            } else {
                0
            };

            let visible_lines: Vec<Line> = lines.into_iter().skip(skip).collect();

            let mut typing_lines = visible_lines;
            if let Some(chat_state) = &conversation.remote_chat_state {
                if matches!(chat_state, waddle_core::event::ChatState::Composing) {
                    typing_lines.push(Line::from(Span::styled(
                        "typing...",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }

            let paragraph = Paragraph::new(typing_lines)
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
        _ => {
            let help = Paragraph::new("Press Enter on a contact to start chatting")
                .style(Style::default().fg(Color::DarkGray))
                .block(block);
            frame.render_widget(help, area);
        }
    }
}

fn draw_status_bar(frame: &mut Frame, state: &AppState, area: Rect) {
    let status_text = match &state.connection_status {
        ConnectionStatus::Connected { jid } => {
            format!(" Connected as {jid} │ Online │ UTF-8 ")
        }
        ConnectionStatus::Syncing => " Syncing... │ Online │ UTF-8 ".to_string(),
        other => format!(" {} ", other.label()),
    };

    let status_color = match &state.connection_status {
        ConnectionStatus::Connected { .. } => Color::Green,
        ConnectionStatus::Syncing => Color::Yellow,
        ConnectionStatus::Connecting => Color::Yellow,
        ConnectionStatus::Disconnected => Color::Red,
    };

    let status =
        Paragraph::new(status_text).style(Style::default().bg(Color::DarkGray).fg(status_color));
    frame.render_widget(status, area);
}

fn draw_input(frame: &mut Frame, state: &AppState, area: Rect) {
    let (title, style) = match state.input_mode {
        InputMode::Normal => (" Normal ", Style::default().fg(Color::DarkGray)),
        InputMode::Insert => (" Insert ", Style::default().fg(Color::Green)),
        InputMode::Command => (" Command ", Style::default().fg(Color::Yellow)),
    };

    let prefix = match state.input_mode {
        InputMode::Command => ":",
        _ => "> ",
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(style);

    let input = Paragraph::new(format!("{prefix}{}", state.input_buffer)).block(block);
    frame.render_widget(input, area);

    if state.input_mode != InputMode::Normal {
        let cursor_x = area.x + 1 + prefix.len() as u16 + state.input_buffer.len() as u16;
        let cursor_y = area.y + 1;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}
