use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::state::{AppState, InputMode, Panel};

pub enum Action {
    None,
    SendMessage { to: String, body: String },
    ExecuteCommand(String),
    OpenConversation(String),
    Quit,
}

pub fn handle_key(state: &mut AppState, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.should_quit = true;
        return Action::Quit;
    }

    match state.input_mode {
        InputMode::Normal => handle_normal_mode(state, key),
        InputMode::Insert => handle_insert_mode(state, key),
        InputMode::Command => handle_command_mode(state, key),
    }
}

fn handle_normal_mode(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char('q') => {
            state.should_quit = true;
            Action::Quit
        }
        KeyCode::Char('i') => {
            state.input_mode = InputMode::Insert;
            Action::None
        }
        KeyCode::Char(':') => {
            state.input_mode = InputMode::Command;
            state.input_buffer.clear();
            Action::None
        }
        KeyCode::Tab => {
            state.focused_panel = match state.focused_panel {
                Panel::Sidebar => Panel::Conversation,
                Panel::Conversation => Panel::Sidebar,
            };
            Action::None
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if state.focused_panel == Panel::Sidebar {
                let count = state.sidebar_items_count();
                if count > 0 && state.sidebar_index < count - 1 {
                    state.sidebar_index += 1;
                }
            } else if state.scroll_offset > 0 {
                state.scroll_offset -= 1;
            }
            Action::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if state.focused_panel == Panel::Sidebar {
                if state.sidebar_index > 0 {
                    state.sidebar_index -= 1;
                }
            } else {
                state.scroll_offset = state.scroll_offset.saturating_add(1);
            }
            Action::None
        }
        KeyCode::Enter => {
            if state.focused_panel == Panel::Sidebar {
                if let Some(jid) = state.selected_jid() {
                    state.active_conversation = Some(jid.clone());
                    state.scroll_offset = 0;
                    state.ensure_conversation(&jid);
                    state.mark_conversation_read(&jid);
                    return Action::OpenConversation(jid);
                }
            }
            Action::None
        }
        _ => Action::None,
    }
}

fn handle_insert_mode(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc => {
            state.input_mode = InputMode::Normal;
            Action::None
        }
        KeyCode::Enter => {
            let body = state.input_buffer.drain(..).collect::<String>();
            if body.is_empty() {
                return Action::None;
            }
            if let Some(to) = state.active_conversation.clone() {
                Action::SendMessage { to, body }
            } else {
                Action::None
            }
        }
        KeyCode::Backspace => {
            state.input_buffer.pop();
            Action::None
        }
        KeyCode::Char(c) => {
            state.input_buffer.push(c);
            Action::None
        }
        _ => Action::None,
    }
}

fn handle_command_mode(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc => {
            state.input_mode = InputMode::Normal;
            state.input_buffer.clear();
            Action::None
        }
        KeyCode::Enter => {
            let cmd = state.input_buffer.drain(..).collect::<String>();
            state.input_mode = InputMode::Normal;
            if cmd.is_empty() {
                return Action::None;
            }
            Action::ExecuteCommand(cmd)
        }
        KeyCode::Backspace => {
            state.input_buffer.pop();
            if state.input_buffer.is_empty() {
                state.input_mode = InputMode::Normal;
            }
            Action::None
        }
        KeyCode::Char(c) => {
            state.input_buffer.push(c);
            Action::None
        }
        _ => Action::None,
    }
}
