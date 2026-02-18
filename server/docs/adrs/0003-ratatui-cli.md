# ADR-0003: Use Ratatui for CLI TUI

## Status

Accepted

## Context

Waddle Social's MVP will be a CLI-first Terminal User Interface (TUI) to:
- Validate core messaging functionality before web/mobile development
- Serve power users and developers who prefer terminal workflows
- Enable rapid iteration without frontend framework complexity

We evaluated TUI libraries:
- **Ratatui**: Modern fork of tui-rs, actively maintained, widget-rich
- **tui-rs**: Original library, now deprecated in favor of Ratatui
- **Cursive**: Dialog-based, less flexible for custom layouts
- **Termion/Crossterm**: Lower-level, require building UI primitives

## Decision

We will use **Ratatui** for the CLI TUI client.

## Consequences

### Positive

- **Active Maintenance**: Regular releases, responsive maintainers
- **Widget Library**: Built-in tables, lists, charts, paragraphs, gauges
- **Backend Agnostic**: Works with crossterm (cross-platform) or termion
- **Immediate Mode**: Stateless rendering simplifies UI logic
- **Community**: Growing ecosystem of examples and extensions
- **Unicode Support**: Full Unicode and emoji rendering

### Negative

- **Immediate Mode Overhead**: Must re-render entire UI each frame
- **No Built-in State**: Application must manage all state externally
- **Learning Curve**: Terminal UI paradigms differ from web/native

### Neutral

- **Styling**: Supports colors and modifiers; styling is code-based (no CSS)

## Implementation Notes

The TUI will feature:
- Split-pane layout (channel list, message view, input)
- Vim-style keybindings (configurable)
- Inline markdown rendering
- Status bar with connection state and presence

## Related

- [ADR-0001: Rust Backend](./0001-rust-backend.md) (shared language)
- [Spec: CLI Commands](../specs/cli-commands.md)
