use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::state::{AppState, ConnectionStatus, InputMode, Panel};
use waddle_core::event::PresenceShow;
use waddle_core::theme::Theme;

struct Palette {
    background: Color,
    foreground: Color,
    surface: Color,
    accent: Color,
    border: Color,
    success: Color,
    warning: Color,
    error: Color,
    muted: Color,
    roster_highlight: Color,
    status_bar_bg: Color,
    input_border: Color,
    unread_badge: Color,
}

impl Palette {
    fn from_theme(theme: &Theme) -> Self {
        let background = parse_theme_color(&theme.colors.background, Color::Black);
        let foreground = parse_theme_color(&theme.colors.foreground, Color::White);
        let surface = parse_theme_color(&theme.colors.surface, background);
        let accent = parse_theme_color(&theme.colors.accent, Color::Cyan);
        let border = parse_theme_color(&theme.colors.border, Color::DarkGray);
        let success = parse_theme_color(&theme.colors.success, Color::Green);
        let warning = parse_theme_color(&theme.colors.warning, Color::Yellow);
        let error = parse_theme_color(&theme.colors.error, Color::Red);
        let muted = parse_theme_color(&theme.colors.muted, Color::DarkGray);

        let roster_highlight = parse_theme_override(
            theme
                .tui_overrides
                .as_ref()
                .and_then(|overrides| overrides.roster_highlight.as_deref()),
            surface,
        );
        let status_bar_bg = parse_theme_override(
            theme
                .tui_overrides
                .as_ref()
                .and_then(|overrides| overrides.status_bar_bg.as_deref()),
            surface,
        );
        let input_border = parse_theme_override(
            theme
                .tui_overrides
                .as_ref()
                .and_then(|overrides| overrides.input_border.as_deref()),
            border,
        );
        let unread_badge = parse_theme_override(
            theme
                .tui_overrides
                .as_ref()
                .and_then(|overrides| overrides.unread_badge.as_deref()),
            warning,
        );

        Self {
            background,
            foreground,
            surface,
            accent,
            border,
            success,
            warning,
            error,
            muted,
            roster_highlight,
            status_bar_bg,
            input_border,
            unread_badge,
        }
    }
}

fn parse_theme_override(value: Option<&str>, fallback: Color) -> Color {
    value
        .map(|color| parse_theme_color(color, fallback))
        .unwrap_or(fallback)
}

fn parse_theme_color(value: &str, fallback: Color) -> Color {
    let trimmed = value.trim();
    let Some(hex) = trimmed.strip_prefix('#') else {
        return fallback;
    };

    let normalized = match hex.len() {
        3 => {
            let mut output = String::with_capacity(6);
            for c in hex.chars() {
                output.push(c);
                output.push(c);
            }
            output
        }
        6 => hex.to_string(),
        8 => hex[..6].to_string(),
        _ => return fallback,
    };

    let Ok(value) = u32::from_str_radix(&normalized, 16) else {
        return fallback;
    };

    let r = ((value >> 16) & 0xff) as u8;
    let g = ((value >> 8) & 0xff) as u8;
    let b = (value & 0xff) as u8;
    Color::Rgb(r, g, b)
}

pub fn draw(frame: &mut Frame, state: &AppState) {
    let palette = Palette::from_theme(&state.theme);

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

    draw_sidebar(frame, state, main_chunks[0], &palette);
    draw_conversation(frame, state, main_chunks[1], &palette);
    draw_status_bar(frame, state, chunks[1], &palette);
    draw_input(frame, state, chunks[2], &palette);
}

fn presence_indicator(show: &PresenceShow, palette: &Palette) -> (&'static str, Color) {
    match show {
        PresenceShow::Available | PresenceShow::Chat => ("●", palette.success),
        PresenceShow::Away => ("●", palette.warning),
        PresenceShow::Xa => ("●", palette.warning),
        PresenceShow::Dnd => ("●", palette.error),
        PresenceShow::Unavailable => ("○", palette.muted),
    }
}

fn draw_sidebar(frame: &mut Frame, state: &AppState, area: Rect, palette: &Palette) {
    let focused = state.focused_panel == Panel::Sidebar;
    let border_style = if focused {
        Style::default().fg(palette.accent)
    } else {
        Style::default().fg(palette.border)
    };

    let title = format!(" {} ", state.i18n.t("roster-title", None));
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(Style::default().bg(palette.surface).fg(palette.foreground))
        .border_style(border_style);

    let mut items: Vec<ListItem> = Vec::new();

    for (i, entry) in state.roster.iter().enumerate() {
        let (indicator, color) = presence_indicator(&entry.presence, palette);
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
                    .fg(palette.unread_badge)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        let mut item = ListItem::new(Line::from(spans));
        if is_selected {
            item = item.style(Style::default().bg(palette.roster_highlight));
        }
        items.push(item);
    }

    if !state.rooms.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            format!("── {} ──", state.i18n.t("rooms-title", None)),
            Style::default().fg(palette.muted),
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
                        .fg(palette.unread_badge)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            let mut item = ListItem::new(Line::from(spans));
            if is_selected {
                item = item.style(Style::default().bg(palette.roster_highlight));
            }
            items.push(item);
        }
    }

    if items.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            state.i18n.t("roster-empty", None),
            Style::default().fg(palette.muted),
        ))));
    }

    let list = List::new(items)
        .block(block)
        .style(Style::default().bg(palette.surface).fg(palette.foreground));
    frame.render_widget(list, area);
}

fn draw_conversation(frame: &mut Frame, state: &AppState, area: Rect, palette: &Palette) {
    let focused = state.focused_panel == Panel::Conversation;
    let border_style = if focused {
        Style::default().fg(palette.accent)
    } else {
        Style::default().fg(palette.border)
    };

    let title = match &state.active_conversation {
        Some(jid) => format!(" {} ", jid),
        None => format!(" {} ", state.i18n.t("conversation-none-title", None)),
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .style(
            Style::default()
                .bg(palette.background)
                .fg(palette.foreground),
        )
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
                let sender = if sender.is_empty() { "you" } else { sender };

                let mut spans = vec![
                    Span::styled(format!("{time} "), Style::default().fg(palette.muted)),
                    Span::styled(
                        format!("{sender}: "),
                        Style::default()
                            .fg(palette.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(&msg.body),
                ];

                if state.delivered_message_ids.contains(&msg.id) {
                    spans.push(Span::styled(
                        format!(" [{}]", state.i18n.t("message-delivered", None)),
                        Style::default().fg(palette.muted),
                    ));
                }

                lines.push(Line::from(spans));

                // Render plugin embeds as inline cards below the message
                for embed in &msg.embeds {
                    render_embed_lines(&mut lines, embed, palette);
                }
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
                        state.i18n.t("chatstate-typing", None),
                        Style::default()
                            .fg(palette.muted)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }

            let paragraph = Paragraph::new(typing_lines)
                .block(block)
                .style(
                    Style::default()
                        .bg(palette.background)
                        .fg(palette.foreground),
                )
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
        Some(_) => {
            let text = state.i18n.t("conversation-empty", None);
            let paragraph = Paragraph::new(text)
                .style(Style::default().fg(palette.muted).bg(palette.background))
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
        None => {
            let text = state.i18n.t("conversation-select", None);
            let paragraph = Paragraph::new(text)
                .style(Style::default().fg(palette.muted).bg(palette.background))
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
    }
}

fn draw_status_bar(frame: &mut Frame, state: &AppState, area: Rect, palette: &Palette) {
    let mut status_text = match &state.connection_status {
        ConnectionStatus::Connected { jid } => format!(
            " {} {jid} │ {} │ UTF-8 ",
            state.i18n.t("status-connected-as", None),
            state.i18n.t("status-available", None),
        ),
        ConnectionStatus::Syncing => format!(" {} │ UTF-8 ", state.i18n.t("status-syncing", None)),
        ConnectionStatus::Connecting => format!(" {} ", state.i18n.t("status-connecting", None)),
        ConnectionStatus::Disconnected => {
            format!(" {} ", state.i18n.t("status-disconnected", None))
        }
    };

    if let Some(feedback) = &state.command_feedback {
        status_text.push_str("│ ");
        status_text.push_str(feedback);
        status_text.push(' ');
    }

    let status_color = match &state.connection_status {
        ConnectionStatus::Connected { .. } => palette.success,
        ConnectionStatus::Syncing => palette.warning,
        ConnectionStatus::Connecting => palette.warning,
        ConnectionStatus::Disconnected => palette.error,
    };

    let status = Paragraph::new(status_text).style(
        Style::default()
            .bg(palette.status_bar_bg)
            .fg(status_color)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(status, area);
}

fn draw_input(frame: &mut Frame, state: &AppState, area: Rect, palette: &Palette) {
    let (title, color) = match state.input_mode {
        InputMode::Normal => (state.i18n.t("mode-normal", None), palette.input_border),
        InputMode::Insert => (state.i18n.t("mode-insert", None), palette.success),
        InputMode::Command => (state.i18n.t("mode-command", None), palette.warning),
    };

    let prefix = match state.input_mode {
        InputMode::Command => ":",
        _ => "> ",
    };

    let block = Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .style(Style::default().bg(palette.surface).fg(palette.foreground))
        .border_style(Style::default().fg(color));

    let input = Paragraph::new(format!("{prefix}{}", state.input_buffer))
        .style(Style::default().fg(palette.foreground).bg(palette.surface))
        .block(block);
    frame.render_widget(input, area);

    if state.input_mode != InputMode::Normal {
        let cursor_x = area.x + 1 + prefix.len() as u16 + state.input_buffer.len() as u16;
        let cursor_y = area.y + 1;
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Maximum number of lines a single embed card can occupy in the TUI.
const MAX_EMBED_LINES: usize = 20;

/// Render a `MessageEmbed` as styled inline lines (a simple card).
///
/// If a plugin runtime is available and has a `render_tui` export, it would
/// be called here. For now we render a built-in card for known namespaces
/// and a generic fallback for unknown ones.
fn render_embed_lines(
    lines: &mut Vec<Line<'_>>,
    embed: &waddle_core::event::MessageEmbed,
    palette: &Palette,
) {
    let data = &embed.data;
    let mut card_lines: Vec<Line<'_>> = Vec::new();

    match embed.namespace.as_str() {
        // ── GitHub repo embed ──────────────────────────────────────────
        "urn:waddle:github:0" if data.get("type").and_then(|v| v.as_str()) == Some("repo")
            || data.get("owner").is_some() =>
        {
            let owner = data
                .get("owner")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let desc = data
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let stars = data
                .get("stars")
                .and_then(|v| v.as_u64())
                .map(|n| format!("⭐ {n}"))
                .unwrap_or_default();
            let lang = data
                .get("language")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            card_lines.push(Line::from(Span::styled(
                format!("  ┌─ {owner}/{name}"),
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )));
            if !desc.is_empty() {
                // Truncate description to a reasonable length
                let truncated: String = desc.chars().take(80).collect();
                card_lines.push(Line::from(Span::styled(
                    format!("  │ {truncated}"),
                    Style::default().fg(palette.foreground),
                )));
            }
            let mut meta_parts = Vec::new();
            if !stars.is_empty() {
                meta_parts.push(stars);
            }
            if !lang.is_empty() {
                meta_parts.push(lang.to_string());
            }
            if let Some(license) = data.get("license").and_then(|v| v.as_str()) {
                meta_parts.push(format!("📄 {license}"));
            }
            if !meta_parts.is_empty() {
                card_lines.push(Line::from(Span::styled(
                    format!("  │ {}", meta_parts.join(" · ")),
                    Style::default().fg(palette.muted),
                )));
            }
            card_lines.push(Line::from(Span::styled(
                "  └───",
                Style::default().fg(palette.border),
            )));
        }

        // ── GitHub issue embed ─────────────────────────────────────────
        "urn:waddle:github:0" if data.get("type").and_then(|v| v.as_str()) == Some("issue")
            || data.get("number").is_some() && data.get("title").is_some() =>
        {
            let repo = data.get("repo").and_then(|v| v.as_str()).unwrap_or("?");
            let number = data
                .get("number")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let state_val = data
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("open");
            let icon = if state_val == "closed" { "🟣" } else { "🟢" };

            card_lines.push(Line::from(Span::styled(
                format!("  ┌─ {icon} {repo}#{number}"),
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            )));
            if !title.is_empty() {
                let truncated: String = title.chars().take(80).collect();
                card_lines.push(Line::from(Span::styled(
                    format!("  │ {truncated}"),
                    Style::default().fg(palette.foreground),
                )));
            }
            card_lines.push(Line::from(Span::styled(
                "  └───",
                Style::default().fg(palette.border),
            )));
        }

        // ── Generic fallback ───────────────────────────────────────────
        _ => {
            card_lines.push(Line::from(Span::styled(
                format!("  [📎 embed: {}]", embed.namespace),
                Style::default().fg(palette.muted),
            )));
        }
    }

    // Cap the output
    for line in card_lines.into_iter().take(MAX_EMBED_LINES) {
        lines.push(line);
    }
}
