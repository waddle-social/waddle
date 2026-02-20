// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Messages widget for displaying channel/DM messages.
//!
//! Renders message content and any raw embed payloads. Embed rendering
//! is generic — the CLI shows namespace and element name for unknown
//! embeds. Plugin-driven rich rendering happens through the WASM runtime.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget, Wrap},
};

use crate::app::App;
use crate::config::Config;
use crate::sanitize::sanitize_for_terminal;
use crate::stanza::RawEmbed;

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

        let timestamp = if self.config.ui.show_timestamps {
            let formatted = msg
                .timestamp
                .format(&self.config.ui.time_format)
                .to_string();
            format!("[{}] ", formatted)
        } else {
            String::new()
        };

        let author_style = Style::default()
            .fg(author_color(&msg.author))
            .add_modifier(Modifier::BOLD);

        let header = Line::from(vec![
            Span::styled(timestamp, Style::default().fg(Color::DarkGray)),
            Span::styled(format!("<{}>", msg.author), author_style),
        ]);
        lines.push(header);

        let content = Line::from(vec![Span::styled(
            format!("  {}", msg.content),
            Style::default().fg(Color::White),
        )]);
        lines.push(content);

        // Render embeds (generic; plugins can provide richer rendering)
        for embed in &msg.embeds {
            lines.extend(format_embed(embed));
        }

        // Spacing
        lines.push(Line::default());

        lines
    }
}

/// Format a raw embed for TUI display.
/// This provides a generic rendering; the WASM plugin system can override
/// this with richer formatting via the `tui_renderer` hook.
fn format_embed(embed: &RawEmbed) -> Vec<Line<'static>> {
    let mut lines = vec![];

    let value_style = Style::default().fg(Color::Cyan);

    // Show a compact representation based on namespace
    let label = match embed.namespace.as_str() {
        "urn:waddle:github:0" => format_github_embed(embed),
        _ => {
            let namespace = sanitize_for_terminal(&embed.namespace, None);
            let name = sanitize_for_terminal(&embed.name, None);
            format!("  📎 [{}:{}]", namespace, name)
        }
    };

    lines.push(Line::from(vec![Span::styled(label, value_style)]));
    lines
}

/// Format a GitHub embed with a compact representation.
/// This is a basic fallback — the real rendering lives in the GitHub WASM plugin.
fn format_github_embed(embed: &RawEmbed) -> String {
    match embed.name.as_str() {
        "repo" => {
            // Parse basic attrs from XML for a one-line summary
            if let Some(owner) = extract_attr(&embed.xml, "owner") {
                if let Some(name) = extract_attr(&embed.xml, "name") {
                    return format!("  📦 {}/{}", owner, name);
                }
            }
            "  📦 [GitHub repo]".to_string()
        }
        "issue" => {
            if let Some(repo) = extract_attr(&embed.xml, "repo") {
                if let Some(number) = extract_attr(&embed.xml, "number") {
                    let state = extract_attr(&embed.xml, "state").unwrap_or_default();
                    let icon = if state == "closed" { "🟣" } else { "🟢" };
                    return format!("  {} {}#{}", icon, repo, number);
                }
            }
            "  🔵 [GitHub issue]".to_string()
        }
        "pr" => {
            if let Some(repo) = extract_attr(&embed.xml, "repo") {
                if let Some(number) = extract_attr(&embed.xml, "number") {
                    let state = extract_attr(&embed.xml, "state").unwrap_or_default();
                    let merged = extract_attr(&embed.xml, "merged").unwrap_or_default();
                    let icon = if merged == "true" {
                        "🟣"
                    } else if state == "closed" {
                        "🔴"
                    } else {
                        "🟢"
                    };
                    return format!("  {} {}#{} (PR)", icon, repo, number);
                }
            }
            "  🔀 [GitHub PR]".to_string()
        }
        _ => {
            let name = sanitize_for_terminal(&embed.name, None);
            format!("  📎 [github:{}]", name)
        }
    }
}

/// Quick attribute extraction from serialized XML (no full parse needed for display).
fn extract_attr<'a>(xml: &'a str, attr_name: &str) -> Option<String> {
    let pattern = format!("{}='", attr_name);
    if let Some(start) = xml.find(&pattern) {
        let value_start = start + pattern.len();
        if let Some(end) = xml[value_start..].find('\'') {
            return Some(sanitize_for_terminal(
                &xml[value_start..value_start + end],
                None,
            ));
        }
    }
    // Also try double quotes
    let pattern = format!("{}=\"", attr_name);
    if let Some(start) = xml.find(&pattern) {
        let value_start = start + pattern.len();
        if let Some(end) = xml[value_start..].find('"') {
            return Some(sanitize_for_terminal(
                &xml[value_start..value_start + end],
                None,
            ));
        }
    }
    None
}

/// Generate a consistent color for a username
fn author_color(author: &str) -> Color {
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
        let mut all_lines: Vec<Line<'static>> = Vec::new();

        let messages = self.app.messages();

        if messages.is_empty() {
            if self.app.mam_loading {
                all_lines.push(Line::from(vec![Span::styled(
                    "Loading history...",
                    Style::default().fg(Color::Yellow),
                )]));
            } else {
                all_lines.push(Line::from(vec![Span::styled(
                    "No messages yet. Say something!",
                    Style::default().fg(Color::DarkGray),
                )]));
            }
        } else {
            if self.app.mam_loading {
                all_lines.push(Line::from(vec![Span::styled(
                    "⏳ Loading older messages...",
                    Style::default().fg(Color::Yellow),
                )]));
                all_lines.push(Line::default());
            }
            for msg in messages {
                all_lines.extend(self.format_message(msg));
            }
        }

        let visible_height =
            self.block
                .as_ref()
                .map_or(area.height, |_| area.height.saturating_sub(2)) as usize;

        let total_lines = all_lines.len();
        let scroll_offset = if total_lines > visible_height {
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
    fn test_author_color_deterministic() {
        let color1 = author_color("alice");
        let color2 = author_color("alice");
        assert_eq!(color1, color2);
    }

    #[test]
    fn test_extract_attr_single_quotes() {
        let xml = "<repo xmlns='urn:waddle:github:0' owner='rust-lang' name='rust'/>";
        assert_eq!(extract_attr(xml, "owner"), Some("rust-lang".to_string()));
        assert_eq!(extract_attr(xml, "name"), Some("rust".to_string()));
    }

    #[test]
    fn test_extract_attr_double_quotes() {
        let xml = r#"<repo xmlns="urn:waddle:github:0" owner="a" name="b"/>"#;
        assert_eq!(extract_attr(xml, "owner"), Some("a".to_string()));
    }

    #[test]
    fn test_extract_attr_missing() {
        let xml = "<repo xmlns='urn:waddle:github:0'/>";
        assert_eq!(extract_attr(xml, "owner"), None);
    }

    #[test]
    fn test_extract_attr_sanitizes_terminal_escapes() {
        let xml = "<repo owner='safe\x1b[31mred\x1b[0m'/>";
        assert_eq!(extract_attr(xml, "owner"), Some("safered".to_string()));
    }

    #[test]
    fn test_format_github_repo_embed() {
        let embed = RawEmbed {
            namespace: "urn:waddle:github:0".into(),
            name: "repo".into(),
            xml: "<repo xmlns='urn:waddle:github:0' owner='rust-lang' name='rust'/>".into(),
        };
        let result = format_github_embed(&embed);
        assert!(result.contains("rust-lang/rust"));
    }

    #[test]
    fn test_format_github_issue_embed() {
        let embed = RawEmbed {
            namespace: "urn:waddle:github:0".into(),
            name: "issue".into(),
            xml: "<issue xmlns='urn:waddle:github:0' repo='a/b' number='42' state='open'/>".into(),
        };
        let result = format_github_embed(&embed);
        assert!(result.contains("a/b#42"));
    }

    #[test]
    fn test_format_unknown_embed() {
        let embed = RawEmbed {
            namespace: "urn:custom:ext:0".into(),
            name: "widget".into(),
            xml: "<widget xmlns='urn:custom:ext:0'/>".into(),
        };
        let lines = format_embed(&embed);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_format_unknown_embed_sanitizes_namespace_and_name() {
        let embed = RawEmbed {
            namespace: "urn:custom:\x1b[31mext\x1b[0m:0".into(),
            name: "wid\x1b]0;evil\x07get".into(),
            xml: "<widget xmlns='urn:custom:ext:0'/>".into(),
        };
        let lines = format_embed(&embed);
        let rendered = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("[urn:custom:ext:0:widget]"));
        assert!(!rendered.contains('\x1b'));
    }

    #[test]
    fn test_format_github_unknown_embed_sanitizes_name() {
        let embed = RawEmbed {
            namespace: "urn:waddle:github:0".into(),
            name: "unk\x1b[2Jnown".into(),
            xml: "<unknown xmlns='urn:waddle:github:0'/>".into(),
        };
        let rendered = format_github_embed(&embed);
        assert_eq!(rendered, "  📎 [github:unknown]");
    }
}
