// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Event handling for the Waddle TUI.
//!
//! This module provides an async event loop that handles terminal events
//! (key presses, mouse events, resize) and converts them to application events.

use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;
use tokio::sync::mpsc;

/// Application events
#[derive(Debug, Clone)]
pub enum Event {
    /// Terminal tick for animations/updates
    Tick,
    /// Key press event
    Key(KeyEvent),
    /// Mouse event (future use)
    Mouse(crossterm::event::MouseEvent),
    /// Terminal resize
    Resize(u16, u16),
}

/// Event handler that runs in a background task
pub struct EventHandler {
    /// Channel receiver for events
    rx: mpsc::UnboundedReceiver<Event>,
    /// Handle to the background task
    _task: tokio::task::JoinHandle<()>,
}

impl EventHandler {
    /// Create a new event handler with the given tick rate
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let task = tokio::spawn(async move {
            let mut last_tick = std::time::Instant::now();

            loop {
                // Calculate timeout until next tick
                let timeout = tick_rate
                    .checked_sub(last_tick.elapsed())
                    .unwrap_or(Duration::ZERO);

                // Poll for events with timeout
                if event::poll(timeout).unwrap_or(false) {
                    match event::read() {
                        Ok(CrosstermEvent::Key(key)) => {
                            if tx.send(Event::Key(key)).is_err() {
                                break;
                            }
                        }
                        Ok(CrosstermEvent::Mouse(mouse)) => {
                            if tx.send(Event::Mouse(mouse)).is_err() {
                                break;
                            }
                        }
                        Ok(CrosstermEvent::Resize(width, height)) => {
                            if tx.send(Event::Resize(width, height)).is_err() {
                                break;
                            }
                        }
                        Ok(_) => {} // Ignore other events
                        Err(_) => break,
                    }
                }

                // Send tick if enough time has passed
                if last_tick.elapsed() >= tick_rate {
                    if tx.send(Event::Tick).is_err() {
                        break;
                    }
                    last_tick = std::time::Instant::now();
                }
            }
        });

        Self { rx, _task: task }
    }

    /// Wait for the next event
    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}

/// Key action that the application should take
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// No action / unhandled key
    None,
    /// Quit the application
    Quit,
    /// Cycle focus to next panel
    FocusNext,
    /// Cycle focus to previous panel
    FocusPrev,
    /// Move up in current context
    Up,
    /// Move down in current context
    Down,
    /// Move left in current context
    Left,
    /// Move right in current context
    Right,
    /// Select/confirm in current context
    Select,
    /// Go back / cancel
    Back,
    /// Delete character before cursor
    Backspace,
    /// Delete character at cursor
    Delete,
    /// Move to start
    Home,
    /// Move to end
    End,
    /// Page up
    PageUp,
    /// Page down
    PageDown,
    /// Insert character
    Char(char),
    /// Submit input (Enter in input mode)
    Submit,
}

/// Convert a key event to an action based on the current focus
pub fn key_to_action(key: KeyEvent, in_input_mode: bool) -> KeyAction {
    // Global keybindings (always active)
    match (key.modifiers, key.code) {
        // Ctrl+C always quits
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => return KeyAction::Quit,
        // Tab cycles focus
        (KeyModifiers::NONE, KeyCode::Tab) => return KeyAction::FocusNext,
        (KeyModifiers::SHIFT, KeyCode::BackTab) => return KeyAction::FocusPrev,
        _ => {}
    }

    // Input mode keybindings
    if in_input_mode {
        return match (key.modifiers, key.code) {
            // Escape exits input mode (goes to sidebar)
            (KeyModifiers::NONE, KeyCode::Esc) => KeyAction::Back,
            // Enter submits
            (KeyModifiers::NONE, KeyCode::Enter) => KeyAction::Submit,
            // Navigation
            (KeyModifiers::NONE, KeyCode::Left) => KeyAction::Left,
            (KeyModifiers::NONE, KeyCode::Right) => KeyAction::Right,
            (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
                KeyAction::Home
            }
            (KeyModifiers::NONE, KeyCode::End) | (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
                KeyAction::End
            }
            // Deletion
            (KeyModifiers::NONE, KeyCode::Backspace) => KeyAction::Backspace,
            (KeyModifiers::NONE, KeyCode::Delete) => KeyAction::Delete,
            // Character input
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => KeyAction::Char(c),
            _ => KeyAction::None,
        };
    }

    // Normal mode keybindings (sidebar, messages)
    match (key.modifiers, key.code) {
        // q quits in normal mode
        (KeyModifiers::NONE, KeyCode::Char('q')) => KeyAction::Quit,
        // Vim-style navigation
        (KeyModifiers::NONE, KeyCode::Char('j') | KeyCode::Down) => KeyAction::Down,
        (KeyModifiers::NONE, KeyCode::Char('k') | KeyCode::Up) => KeyAction::Up,
        (KeyModifiers::NONE, KeyCode::Char('h') | KeyCode::Left) => KeyAction::Left,
        (KeyModifiers::NONE, KeyCode::Char('l') | KeyCode::Right) => KeyAction::Right,
        // Selection
        (KeyModifiers::NONE, KeyCode::Enter | KeyCode::Char(' ')) => KeyAction::Select,
        // Page navigation
        (KeyModifiers::NONE, KeyCode::PageUp) | (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            KeyAction::PageUp
        }
        (KeyModifiers::NONE, KeyCode::PageDown) | (KeyModifiers::CONTROL, KeyCode::Char('d')) => {
            KeyAction::PageDown
        }
        // Home/End
        (KeyModifiers::NONE, KeyCode::Home) | (KeyModifiers::NONE, KeyCode::Char('g')) => {
            KeyAction::Home
        }
        (KeyModifiers::NONE, KeyCode::End) | (KeyModifiers::SHIFT, KeyCode::Char('G')) => {
            KeyAction::End
        }
        // Escape
        (KeyModifiers::NONE, KeyCode::Esc) => KeyAction::Back,
        // Start typing to enter input mode
        (KeyModifiers::NONE, KeyCode::Char('i')) => KeyAction::Char('i'),
        _ => KeyAction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn make_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Press)
    }

    #[test]
    fn test_quit_keybindings() {
        // Ctrl+C always quits
        assert_eq!(
            key_to_action(make_key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            KeyAction::Quit
        );
        assert_eq!(
            key_to_action(make_key(KeyCode::Char('c'), KeyModifiers::CONTROL), true),
            KeyAction::Quit
        );

        // q quits in normal mode only
        assert_eq!(
            key_to_action(make_key(KeyCode::Char('q'), KeyModifiers::NONE), false),
            KeyAction::Quit
        );
    }

    #[test]
    fn test_vim_navigation() {
        assert_eq!(
            key_to_action(make_key(KeyCode::Char('j'), KeyModifiers::NONE), false),
            KeyAction::Down
        );
        assert_eq!(
            key_to_action(make_key(KeyCode::Char('k'), KeyModifiers::NONE), false),
            KeyAction::Up
        );
    }

    #[test]
    fn test_input_mode() {
        // Characters in input mode
        assert_eq!(
            key_to_action(make_key(KeyCode::Char('a'), KeyModifiers::NONE), true),
            KeyAction::Char('a')
        );
        // Enter submits in input mode
        assert_eq!(
            key_to_action(make_key(KeyCode::Enter, KeyModifiers::NONE), true),
            KeyAction::Submit
        );
    }
}
