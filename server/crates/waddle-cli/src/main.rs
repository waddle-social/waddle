// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Waddle CLI - Terminal UI client for Waddle Social.
//!
//! A decentralized social platform built on XMPP federation.

use anyhow::Result;
use clap::{Parser, Subcommand};
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

mod api;
mod app;
mod config;
mod event;
mod login;
mod sanitize;
mod stanza;
mod ui;
mod xmpp;

use crate::xmpp::{XmppClient, XmppClientEvent};
use app::{App, ConnectionState, Focus};
use config::Config;
use event::{key_to_action, Event, EventHandler, KeyAction};
use ui::render_layout;
use xmpp_parsers::jid::BareJid;

/// Waddle CLI - Terminal client for Waddle Social
#[derive(Parser)]
#[command(name = "waddle")]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Login to Waddle using a configured auth provider
    Login {
        #[arg(short = 'p', long)]
        provider: String,
        #[arg(short, long, default_value = "http://localhost:3000")]
        server: String,
    },
    /// Show current login status
    Status,
    /// Logout and clear saved credentials
    Logout,
    /// Create a new waddle
    Create {
        #[arg(short, long)]
        name: String,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(long)]
        private: bool,
    },
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

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

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut app: App,
    config: Config,
) -> Result<()> {
    let mut events = EventHandler::new(Duration::from_millis(250));
    let (xmpp_event_tx, mut xmpp_event_rx) = mpsc::unbounded_channel::<XmppClientEvent>();
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

    loop {
        if app.should_quit {
            break;
        }

        terminal.draw(|frame| {
            render_layout(frame, &app, &config);
        })?;

        tokio::select! {
            biased;

            event = events.next() => {
                if let Some(event) = event {
                    handle_terminal_event(&mut app, &mut xmpp_client, event, &config).await;
                }
            }

            xmpp_event = xmpp_event_rx.recv() => {
                if let Some(event) = xmpp_event {
                    handle_xmpp_event(&mut app, &mut xmpp_client, event).await;
                }
            }

            _ = async {
                if let Some(ref mut client) = xmpp_client {
                    if app.connection_state.is_connected()
                        || matches!(app.connection_state, ConnectionState::Connecting)
                        || matches!(app.connection_state, ConnectionState::Reconnecting { .. })
                    {
                        let _ = tokio::time::timeout(
                            Duration::from_millis(100),
                            client.poll_events()
                        ).await;
                    }
                } else {
                    // No XMPP client — sleep to avoid busy loop. Terminal events
                    // and tick will still drive redraws.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            } => {}
        }
    }

    if let Some(ref mut client) = xmpp_client {
        info!("Disconnecting XMPP client...");
        let _ = tokio::time::timeout(Duration::from_secs(2), client.disconnect()).await;
    }

    Ok(())
}

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
                KeyAction::Quit => app.quit(),
                KeyAction::FocusNext => app.focus_next(),
                KeyAction::FocusPrev => app.focus_prev(),
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
                        let selected_item = app.sidebar_select().cloned();
                        if let Some(item) = selected_item {
                            handle_sidebar_selection(app, xmpp_client, item).await;
                        }
                    }
                }
                KeyAction::Back => {
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
                        for _ in 0..5 {
                            app.scroll_messages_up();
                        }
                    }
                }
                KeyAction::PageDown => {
                    if app.focus == Focus::Messages {
                        for _ in 0..5 {
                            app.scroll_messages_down();
                        }
                    }
                }
                KeyAction::Char(c) => {
                    if app.focus == Focus::Input {
                        app.input_insert(c);
                    } else if c == 'i' {
                        app.focus = Focus::Input;
                    }
                }
                KeyAction::Submit => {
                    if app.focus == Focus::Input {
                        if let Some(message) = app.input_submit() {
                            send_message(app, xmpp_client, &message).await;
                        }
                    }
                }
                KeyAction::None => {}
            }
        }
        Event::Resize => {}
        Event::Tick => {}
        Event::Mouse => {}
    }
}

async fn handle_xmpp_event(
    app: &mut App,
    xmpp_client: &mut Option<XmppClient>,
    event: XmppClientEvent,
) {
    match event {
        XmppClientEvent::Connected => {
            app.set_connection_state(ConnectionState::Connected);
        }
        XmppClientEvent::Disconnected => {
            app.set_connection_state(ConnectionState::Disconnected);
        }
        XmppClientEvent::RetryScheduled {
            attempt,
            delay_secs,
        } => {
            app.set_connection_state(ConnectionState::Reconnecting {
                attempt,
                countdown_secs: delay_secs,
            });
        }
        XmppClientEvent::RoomJoined { room_jid } => {
            app.room_joined(room_jid.clone());
            if app.current_room_jid.is_none() {
                app.set_current_room(Some(room_jid.clone()));
            }
            // Request MAM history for the room
            if let Some(ref mut client) = xmpp_client {
                let query_id = format!("mam-{}", room_jid);
                app.mam_loading = true;
                client
                    .request_mam_history(&query_id, Some(&room_jid), 50)
                    .await;
            }
        }
        XmppClientEvent::RoomLeft { room_jid } => {
            app.room_left(&room_jid);
            if app.current_room_jid.as_ref() == Some(&room_jid) {
                app.set_current_room(None);
            }
        }
        XmppClientEvent::RoomMessage {
            room_jid,
            sender_nick,
            body,
            id,
            embeds,
        } => {
            let view_key = room_jid.to_string();
            app.add_message_to_with_id(&view_key, id, sender_nick, body, embeds);
        }
        XmppClientEvent::ChatMessage {
            from,
            body,
            id,
            embeds,
        } => {
            let view_key = from.to_string();
            let sender = from
                .node()
                .map(|n| n.to_string())
                .unwrap_or_else(|| from.to_string());
            app.add_message_to_with_id(&view_key, id, sender, body, embeds);
        }
        XmppClientEvent::RoomSubject { room_jid, subject } => {
            let view_key = room_jid.to_string();
            app.add_message_to(
                &view_key,
                "📋".to_string(),
                format!("Topic: {}", subject),
                vec![],
            );
        }
        XmppClientEvent::RosterItem { jid, name } => {
            app.add_roster_contact(jid, name);
        }
        XmppClientEvent::ContactPresence {
            jid,
            available,
            show,
        } => {
            app.update_presence(jid, available, show);
        }
        XmppClientEvent::MamMessage {
            room_jid,
            sender_nick,
            body,
            id,
            embeds,
            timestamp,
        } => {
            let view_key = room_jid.as_ref().map(|j| j.to_string()).unwrap_or_default();
            if !view_key.is_empty() {
                let author = sender_nick.unwrap_or_else(|| "?".to_string());
                let ts = timestamp
                    .as_ref()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc));
                app.prepend_message(&view_key, id, author, body, embeds, ts);
            }
        }
        XmppClientEvent::MamFinished => {
            app.mam_loading = false;
        }
    }
}

async fn handle_sidebar_selection(
    app: &mut App,
    xmpp_client: &mut Option<XmppClient>,
    item: app::SidebarItem,
) {
    use app::SidebarItem;

    match item {
        SidebarItem::Channel { id: _, name } => {
            let room_name = name.trim_start_matches('#');
            if let Some(ref mut client) = xmpp_client {
                if let Ok(room_jid) = xmpp::make_room_jid(room_name, client.muc_domain()) {
                    // Always switch view
                    app.set_current_room(Some(room_jid.clone()));
                    app.focus = Focus::Input;
                    // Only join if connected and not already in room
                    if app.connection_state.is_connected() && !app.is_in_room(&room_jid) {
                        client.join_room_jid(&room_jid, None).await;
                    }
                }
            } else {
                // Offline mode — just set the view name
                app.current_view_name = format!("#{}", room_name);
            }
        }
        SidebarItem::DirectMessage { id, name: _ } => {
            if let Ok(dm_jid) = id.parse::<BareJid>() {
                app.set_current_dm(Some(dm_jid));
                app.focus = Focus::Input;
            }
        }
        SidebarItem::Waddle { id, name } => {
            info!("Selected Waddle: {} ({})", name, id);
        }
        _ => {}
    }
}

async fn send_message(app: &mut App, xmpp_client: &mut Option<XmppClient>, message: &str) {
    if let Some(ref mut client) = xmpp_client {
        if app.connection_state.is_connected() {
            if let Some(ref room_jid) = app.current_room_jid {
                client.send_room_message(room_jid, message).await;
            } else if let Some(ref dm_jid) = app.current_dm_jid {
                client.send_chat_message(dm_jid, message).await;
                let view_key = dm_jid.to_string();
                app.add_message_to(&view_key, app.nickname.clone(), message.to_string(), vec![]);
            } else {
                app.add_message(app.nickname.clone(), message.to_string());
            }
        } else {
            app.add_message(app.nickname.clone(), message.to_string());
        }
    } else {
        app.add_message("you".to_string(), message.to_string());
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Login { provider, server }) => {
            return login::run_login(&provider, &server).await;
        }
        Some(Commands::Status) => return run_status().await,
        Some(Commands::Logout) => return run_logout().await,
        Some(Commands::Create {
            name,
            description,
            private,
        }) => return run_create(&name, description.as_deref(), !private).await,
        None => {}
    }

    let mut config = Config::load().unwrap_or_default();

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

    let mut app = App::new();

    if let Ok(creds) = login::load_credentials() {
        config.xmpp.jid = Some(creds.jid);
        config.xmpp.token = Some(creds.token.clone());
        config.xmpp.server = Some(creds.xmpp_host);
        config.xmpp.port = creds.xmpp_port;
        info!("Loaded saved credentials for: {}", creds.username);

        info!("Fetching waddles and channels from server...");
        let api_client = api::ApiClient::new(&creds.server_url, &creds.token);
        match api_client.fetch_all().await {
            Ok((waddles, channels)) => {
                info!(
                    "Fetched {} waddles and {} channels",
                    waddles.len(),
                    channels.len()
                );
                let waddle_data: Vec<(String, String)> =
                    waddles.into_iter().map(|w| (w.id, w.name)).collect();
                let channel_data: Vec<(String, String)> =
                    channels.into_iter().map(|c| (c.id, c.name)).collect();
                app.set_waddles_and_channels(waddle_data, channel_data);
            }
            Err(e) => {
                warn!(
                    "Failed to fetch data from server: {}. Running in offline mode.",
                    e
                );
            }
        }
    } else {
        info!("No saved credentials - run 'waddle login' to authenticate");
    }

    let mut terminal = setup_terminal()?;
    let result = run_app(&mut terminal, app, config).await;
    restore_terminal(&mut terminal)?;

    if let Err(e) = result {
        eprintln!("Application error: {}", e);
        return Err(e);
    }

    info!("Waddle CLI exiting normally");
    Ok(())
}

async fn run_status() -> Result<()> {
    match login::load_credentials() {
        Ok(_) => {
            println!("Logged in.");
            println!("Saved credentials are available for the CLI session.");
            println!();
            println!("Run 'waddle logout' to clear the saved session.");
        }
        Err(_) => {
            println!("Not logged in.");
            println!();
            println!("Run 'waddle login -p <provider>' to login.");
        }
    }
    Ok(())
}

async fn run_logout() -> Result<()> {
    match login::clear_credentials() {
        Ok(()) => println!("Logged out successfully."),
        Err(e) => eprintln!("Failed to logout: {}", e),
    }
    Ok(())
}

async fn run_create(name: &str, description: Option<&str>, is_public: bool) -> Result<()> {
    let creds = login::load_credentials()
        .map_err(|_| anyhow::anyhow!("Not logged in. Run 'waddle login -p <provider>' first."))?;

    println!("Creating waddle \"{}\"...", name);

    let mut request = api::CreateWaddleRequest::new(name);
    if let Some(desc) = description {
        request = request.with_description(desc);
    }
    request = request.with_public(is_public);

    let api_client = api::ApiClient::new(&creds.server_url, &creds.token);
    match api_client.create_waddle(request).await {
        Ok(waddle) => {
            println!();
            println!("✓ Waddle created successfully!");
            println!("  Name: {}", waddle.name);
            println!("  ID: {}", waddle.id);
            if let Some(desc) = &waddle.description {
                println!("  Description: {}", desc);
            }
            println!("  Public: {}", if waddle.is_public { "yes" } else { "no" });
            println!();
            println!("A #general channel has been created automatically.");
            println!("Run 'waddle' to open the TUI and start chatting!");
        }
        Err(e) => {
            eprintln!("Failed to create waddle: {}", e);
            return Err(e);
        }
    }

    Ok(())
}
