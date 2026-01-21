// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Waddle CLI - Terminal UI client for Waddle Social.
//!
//! A decentralized social platform built on XMPP federation.

use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{info, warn, Level};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod app;
mod config;
mod event;
mod ui;
mod xmpp;

use app::{App, ConnectionState, Focus};
use config::Config;
use event::{key_to_action, Event, EventHandler, KeyAction};
use ui::render_layout;
use xmpp::{XmppClient, XmppClientEvent};

/// Initialize the terminal for TUI mode
fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to its original state
fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Run the main application loop
async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut app: App,
    config: Config,
) -> Result<()> {
    let mut events = EventHandler::new(Duration::from_millis(250));

    // Create channel for XMPP client events
    let (xmpp_event_tx, mut xmpp_event_rx) = mpsc::unbounded_channel::<XmppClientEvent>();

    // Try to create XMPP client if configuration is present
    let mut xmpp_client: Option<XmppClient> = None;

    if config.xmpp.jid.is_some() && config.xmpp.token.is_some() {
        app.set_connection_state(ConnectionState::Connecting);

        match XmppClient::new(&config.xmpp, xmpp_event_tx.clone()).await {
            Ok(client) => {
                app.own_jid = Some(client.jid().clone());
                app.nickname = client.nickname().to_string();
                xmpp_client = Some(client);
                info!("XMPP client created successfully");
            }
            Err(e) => {
                warn!("Failed to create XMPP client: {}", e);
                app.set_connection_state(ConnectionState::Error(e.to_string()));
            }
        }
    } else {
        info!("XMPP not configured - running in offline mode");
    }

    // Spawn XMPP event polling task if client was created
    let _xmpp_poll_handle: Option<tokio::task::JoinHandle<()>> = if let Some(ref mut _client) = xmpp_client {
        // We need to move the client into a task, but we also need to send commands to it
        // For simplicity, we'll poll in the main loop using tokio::select!
        None
    } else {
        None
    };

    loop {
        // Render the UI
        terminal.draw(|frame| {
            render_layout(frame, &app, &config);
        })?;

        // Use tokio::select! to handle both terminal events and XMPP events concurrently
        tokio::select! {
            // Handle terminal events
            event = events.next() => {
                if let Some(event) = event {
                    handle_terminal_event(&mut app, &mut xmpp_client, event, &config).await;
                }
            }

            // Handle XMPP client events
            xmpp_event = xmpp_event_rx.recv() => {
                if let Some(event) = xmpp_event {
                    handle_xmpp_event(&mut app, event);
                }
            }

            // Poll XMPP client for new events (if connected)
            _ = async {
                if let Some(ref mut client) = xmpp_client {
                    if app.connection_state == ConnectionState::Connected
                        || app.connection_state == ConnectionState::Connecting
                    {
                        client.poll_events().await;
                    }
                }
            } => {}
        }

        if app.should_quit {
            break;
        }
    }

    // Clean shutdown of XMPP client
    if let Some(ref mut client) = xmpp_client {
        info!("Disconnecting XMPP client...");
        client.disconnect().await;
    }

    Ok(())
}

/// Handle a terminal event
async fn handle_terminal_event(
    app: &mut App,
    xmpp_client: &mut Option<XmppClient>,
    event: Event,
    _config: &Config,
) {
    match event {
        Event::Key(key) => {
            let in_input_mode = app.focus == Focus::Input;
            let action = key_to_action(key, in_input_mode);

            match action {
                KeyAction::Quit => {
                    app.quit();
                }
                KeyAction::FocusNext => {
                    app.focus_next();
                }
                KeyAction::FocusPrev => {
                    app.focus_prev();
                }
                KeyAction::Up => match app.focus {
                    Focus::Sidebar => app.sidebar_up(),
                    Focus::Messages => app.scroll_messages_up(),
                    Focus::Input => {}
                },
                KeyAction::Down => match app.focus {
                    Focus::Sidebar => app.sidebar_down(),
                    Focus::Messages => app.scroll_messages_down(),
                    Focus::Input => {}
                },
                KeyAction::Left => {
                    if app.focus == Focus::Input {
                        app.input_cursor_left();
                    }
                }
                KeyAction::Right => {
                    if app.focus == Focus::Input {
                        app.input_cursor_right();
                    }
                }
                KeyAction::Select => {
                    if app.focus == Focus::Sidebar {
                        // Handle channel selection and potentially join MUC room
                        // Clone the item to avoid holding a borrow on app
                        let selected_item = app.sidebar_select().cloned();
                        if let Some(item) = selected_item {
                            handle_sidebar_selection(app, xmpp_client, item).await;
                        }
                    }
                }
                KeyAction::Back => {
                    // Escape goes back to sidebar
                    app.focus = Focus::Sidebar;
                }
                KeyAction::Backspace => {
                    if app.focus == Focus::Input {
                        app.input_backspace();
                    }
                }
                KeyAction::Delete => {
                    if app.focus == Focus::Input {
                        app.input_delete();
                    }
                }
                KeyAction::Home => {
                    if app.focus == Focus::Input {
                        app.input_cursor_home();
                    }
                }
                KeyAction::End => {
                    if app.focus == Focus::Input {
                        app.input_cursor_end();
                    }
                }
                KeyAction::PageUp => {
                    if app.focus == Focus::Messages {
                        // Scroll up by multiple lines
                        for _ in 0..5 {
                            app.scroll_messages_up();
                        }
                    }
                }
                KeyAction::PageDown => {
                    if app.focus == Focus::Messages {
                        // Scroll down by multiple lines
                        for _ in 0..5 {
                            app.scroll_messages_down();
                        }
                    }
                }
                KeyAction::Char(c) => {
                    if app.focus == Focus::Input {
                        app.input_insert(c);
                    } else if c == 'i' {
                        // 'i' enters input mode from normal mode
                        app.focus = Focus::Input;
                    }
                }
                KeyAction::Submit => {
                    if app.focus == Focus::Input {
                        // Get the message and send via XMPP if connected
                        if let Some(message) = app.input_submit() {
                            send_message(app, xmpp_client, &message).await;
                        }
                    }
                }
                KeyAction::None => {}
            }
        }
        Event::Resize(_, _) => {
            // Terminal will re-render automatically
        }
        Event::Tick => {
            // Could update animations, check for new messages, etc.
        }
        Event::Mouse(_) => {
            // Mouse support could be added later
        }
    }
}

/// Handle an XMPP client event
fn handle_xmpp_event(app: &mut App, event: XmppClientEvent) {
    match event {
        XmppClientEvent::Connected => {
            app.set_connection_state(ConnectionState::Connected);
        }
        XmppClientEvent::Disconnected => {
            app.set_connection_state(ConnectionState::Disconnected);
            app.clear_xmpp_state();
        }
        XmppClientEvent::Error(err) => {
            app.set_connection_state(ConnectionState::Error(err));
        }
        XmppClientEvent::RoomJoined { room_jid } => {
            app.room_joined(room_jid.clone());
            // If this is the first room or we don't have a current room, make it current
            if app.current_room_jid.is_none() {
                app.set_current_room(Some(room_jid));
            }
        }
        XmppClientEvent::RoomLeft { room_jid } => {
            app.room_left(&room_jid);
            // If we left the current room, clear it
            if app.current_room_jid.as_ref() == Some(&room_jid) {
                app.set_current_room(None);
            }
        }
        XmppClientEvent::RoomMessage {
            room_jid,
            sender_nick,
            body,
            id: _,
        } => {
            // Only show messages from the current room
            if app.current_room_jid.as_ref() == Some(&room_jid) {
                // Don't show our own messages (we'll get an echo)
                // Actually, we should show them - the server echoes messages back
                app.add_message(sender_nick, body);
            }
        }
        XmppClientEvent::ChatMessage { from, body, id: _ } => {
            // For direct messages, show them if we're viewing that conversation
            // For now, just log them
            info!("Direct message from {}: {}", from, body);
        }
    }
}

/// Handle sidebar item selection (join rooms, switch views)
async fn handle_sidebar_selection(
    app: &mut App,
    xmpp_client: &mut Option<XmppClient>,
    item: app::SidebarItem,
) {
    use app::SidebarItem;

    match item {
        SidebarItem::Channel { id: _, name } => {
            // Try to construct room JID and join if connected
            if let Some(ref mut client) = xmpp_client {
                if app.connection_state.is_connected() {
                    let room_name = name.trim_start_matches('#');
                    if let Ok(room_jid) = xmpp::make_room_jid(room_name, client.muc_domain()) {
                        // Switch to this room
                        app.set_current_room(Some(room_jid.clone()));

                        // Join if not already in the room
                        if !app.is_in_room(&room_jid) {
                            client.join_room_jid(&room_jid, None).await;
                        }
                    }
                }
            }
        }
        SidebarItem::DirectMessage { id: _, name } => {
            // For DMs, we'd switch to that conversation
            app.current_view_name = name;
            app.current_room_jid = None; // DMs aren't MUC rooms
            app.messages.clear();
        }
        SidebarItem::Waddle { id, name } => {
            // Selecting a Waddle could show its info or channels
            info!("Selected Waddle: {} ({})", name, id);
        }
        _ => {}
    }
}

/// Send a message to the current room/conversation
async fn send_message(app: &mut App, xmpp_client: &mut Option<XmppClient>, message: &str) {
    if let Some(ref mut client) = xmpp_client {
        if app.connection_state.is_connected() {
            if let Some(ref room_jid) = app.current_room_jid {
                // Send to MUC room
                client.send_room_message(room_jid, message).await;
            } else {
                // Not in a room - could be a DM or just show locally
                app.add_message(app.nickname.clone(), message.to_string());
            }
        } else {
            // Not connected - add message locally
            app.add_message(app.nickname.clone(), message.to_string());
        }
    } else {
        // No XMPP client - add message locally (offline mode)
        app.add_message("you".to_string(), message.to_string());
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration
    let config = Config::load().unwrap_or_default();

    // Set up logging to a file (since we're in TUI mode)
    let log_dir = Config::data_dir().unwrap_or_else(|_| std::env::temp_dir());
    let log_file = std::fs::File::create(log_dir.join("waddle.log")).ok();

    if let Some(file) = log_file {
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(file)
            .with_ansi(false);

        tracing_subscriber::registry()
            .with(file_layer)
            .with(tracing_subscriber::filter::LevelFilter::from_level(
                Level::INFO,
            ))
            .init();
    }

    info!("Waddle CLI starting...");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("License: AGPL-3.0");

    // Initialize terminal
    let mut terminal = setup_terminal()?;

    // Create app state
    let app = App::new();

    // Run the application
    let result = run_app(&mut terminal, app, config).await;

    // Always restore terminal, even on error
    restore_terminal(&mut terminal)?;

    // Report any errors after terminal is restored
    if let Err(e) = result {
        eprintln!("Application error: {}", e);
        return Err(e);
    }

    info!("Waddle CLI exiting normally");
    Ok(())
}
