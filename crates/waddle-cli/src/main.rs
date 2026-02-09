// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Waddle CLI - Terminal UI client for Waddle Social.
//!
//! A decentralized social platform built on XMPP federation.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::path::{Path, PathBuf};
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
mod ui;
mod xmpp;

use crate::xmpp::{XmppClient, XmppClientEvent};
use ::xmpp::BareJid;
use app::{App, ConnectionState, Focus};
use config::Config;
use event::{key_to_action, Event, EventHandler, KeyAction};
use ui::render_layout;

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
    /// Login to Waddle using your Bluesky account
    Login {
        /// Your Bluesky handle (e.g., user.bsky.social)
        #[arg(short = 'u', long)]
        handle: String,

        /// Waddle server URL (default: http://localhost:3000)
        #[arg(short, long, default_value = "http://localhost:3000")]
        server: String,
    },
    /// Show current login status
    Status,
    /// Logout and clear saved credentials
    Logout,
    /// Create a new waddle
    Create {
        /// Name of the waddle
        #[arg(short, long)]
        name: String,

        /// Description of the waddle (optional)
        #[arg(short, long)]
        description: Option<String>,

        /// Make the waddle private (default is public)
        #[arg(long)]
        private: bool,
    },
    /// Run XMPP compliance suite via the managed testcontainers harness
    Compliance {
        /// Compliance profile: best_effort_full | core_strict | full_strict
        #[arg(long, default_value = "best_effort_full")]
        profile: String,

        /// XMPP domain advertised by the server under test
        #[arg(short = 'd', long, default_value = "localhost")]
        domain: String,

        /// Hostname/IP used by interop clients to connect
        #[arg(short = 'H', long, default_value = "host.docker.internal")]
        host: String,

        /// Reply timeout for interop tests in milliseconds
        #[arg(short = 't', long, default_value_t = 10_000)]
        timeout_ms: u32,

        /// Optional admin username to force service-administration account mode
        #[arg(short = 'u', long, default_value = "")]
        admin_username: String,

        /// Optional admin password to force service-administration account mode
        #[arg(short = 'P', long, default_value = "")]
        admin_password: String,

        /// Comma-separated enabled specifications (e.g. RFC6120,RFC6121,XEP-0030)
        #[arg(short = 'e', long)]
        enabled_specs: Option<String>,

        /// Comma-separated disabled specifications
        #[arg(short = 'D', long)]
        disabled_specs: Option<String>,

        /// Directory for logs/artifacts written by the harness
        #[arg(short = 'l', long, default_value = "./test-logs")]
        artifact_dir: String,

        /// Keep interop container after run (for debugging)
        #[arg(long)]
        keep_containers: bool,
    },
}

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
    let _xmpp_poll_handle: Option<tokio::task::JoinHandle<()>> =
        if let Some(ref mut _client) = xmpp_client {
            // We need to move the client into a task, but we also need to send commands to it
            // For simplicity, we'll poll in the main loop using tokio::select!
            None
        } else {
            None
        };

    loop {
        // Check quit flag first, before any blocking operations
        if app.should_quit {
            break;
        }

        // Render the UI
        terminal.draw(|frame| {
            render_layout(frame, &app, &config);
        })?;

        // Use tokio::select! with biased to prioritize terminal events
        tokio::select! {
            biased;

            // Handle terminal events (highest priority)
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

            // Poll XMPP client for new events (if connected) - with timeout
            _ = async {
                if let Some(ref mut client) = xmpp_client {
                    if app.connection_state == ConnectionState::Connected
                        || app.connection_state == ConnectionState::Connecting
                    {
                        // Use timeout to ensure we don't block forever
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            client.poll_events()
                        ).await;
                    }
                } else {
                    // No XMPP client, just yield briefly
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            } => {}
        }
    }

    // Clean shutdown of XMPP client (with timeout to avoid hanging)
    if let Some(ref mut client) = xmpp_client {
        info!("Disconnecting XMPP client...");
        let disconnect_timeout =
            tokio::time::timeout(std::time::Duration::from_secs(2), client.disconnect());
        if disconnect_timeout.await.is_err() {
            warn!("XMPP disconnect timed out, forcing exit");
        }
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
            // Show direct messages if we're viewing that conversation
            if app.current_dm_jid.as_ref() == Some(&from) {
                let sender_name = from
                    .node()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| from.to_string());
                app.add_message(sender_name, body);
            } else {
                // TODO: Store for later / show notification
                info!("Direct message from {} (not in view): {}", from, body);
            }
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
        SidebarItem::DirectMessage { id, name: _ } => {
            // For DMs, switch to that conversation
            // The id is the JID of the DM partner
            if let Ok(dm_jid) = id.parse::<BareJid>() {
                app.set_current_dm(Some(dm_jid));
            }
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
            } else if let Some(ref dm_jid) = app.current_dm_jid {
                // Send direct message
                client.send_chat_message(dm_jid, message).await;
                // Add our own message to the view (DMs don't echo back like MUC)
                app.add_message(app.nickname.clone(), message.to_string());
            } else {
                // Not in a room or DM - just show locally
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
    let cli = Cli::parse();

    // Handle subcommands first (before TUI mode)
    match cli.command {
        Some(Commands::Login { handle, server }) => {
            return login::run_login(&handle, &server).await;
        }
        Some(Commands::Status) => {
            return run_status().await;
        }
        Some(Commands::Logout) => {
            return run_logout().await;
        }
        Some(Commands::Create {
            name,
            description,
            private,
        }) => {
            return run_create(&name, description.as_deref(), !private).await;
        }
        Some(Commands::Compliance {
            profile,
            domain,
            host,
            timeout_ms,
            admin_username,
            admin_password,
            enabled_specs,
            disabled_specs,
            artifact_dir,
            keep_containers,
        }) => {
            return run_compliance(
                &profile,
                &domain,
                &host,
                timeout_ms,
                &admin_username,
                &admin_password,
                enabled_specs.as_deref(),
                disabled_specs.as_deref(),
                &artifact_dir,
                keep_containers,
            );
        }
        None => {
            // No subcommand - run TUI
        }
    }

    // Load configuration
    let mut config = Config::load().unwrap_or_default();

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

    // Create app state
    let mut app = App::new();

    // Load saved credentials if available
    if let Ok(creds) = login::load_credentials() {
        config.xmpp.jid = Some(creds.jid);
        config.xmpp.token = Some(creds.token.clone());
        config.xmpp.server = Some(creds.xmpp_host);
        config.xmpp.port = creds.xmpp_port;
        info!("Loaded saved credentials for: {}", creds.handle);

        // Fetch waddles and channels from the API
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

    // Initialize terminal
    let mut terminal = setup_terminal()?;

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

fn run_compliance(
    profile: &str,
    domain: &str,
    host: &str,
    timeout_ms: u32,
    admin_username: &str,
    admin_password: &str,
    enabled_specs: Option<&str>,
    disabled_specs: Option<&str>,
    artifact_dir: &str,
    keep_containers: bool,
) -> Result<()> {
    let workspace = workspace_root();
    let resolved_artifact_dir = resolve_artifact_dir(artifact_dir)?;

    println!("Running XMPP compliance harness...");
    println!("  Profile:      {}", profile);
    println!("  Domain:       {}", domain);
    println!("  Host:         {}", host);
    println!("  Timeout (ms): {}", timeout_ms);
    println!(
        "  Registration: {}",
        if !admin_username.trim().is_empty() && !admin_password.trim().is_empty() {
            "service administration"
        } else {
            "in-band registration"
        }
    );
    println!("  Artifacts:    {}", resolved_artifact_dir.display());

    let mut command = std::process::Command::new("cargo");
    command
        .current_dir(&workspace)
        .arg("test")
        .arg("--package")
        .arg("waddle-xmpp")
        .arg("--test")
        .arg("xep0479_compliance")
        .arg("--")
        .arg("--ignored")
        .arg("--nocapture")
        .env("WADDLE_COMPLIANCE_PROFILE", profile)
        .env("WADDLE_COMPLIANCE_DOMAIN", domain)
        .env("WADDLE_COMPLIANCE_HOST", host)
        .env("WADDLE_COMPLIANCE_TIMEOUT_MS", timeout_ms.to_string())
        .env(
            "WADDLE_COMPLIANCE_ARTIFACT_DIR",
            resolved_artifact_dir.to_string_lossy().to_string(),
        )
        .env(
            "WADDLE_COMPLIANCE_KEEP_CONTAINERS",
            if keep_containers { "true" } else { "false" },
        );

    if !admin_username.trim().is_empty() {
        command.env("WADDLE_COMPLIANCE_ADMIN_USERNAME", admin_username);
    }
    if !admin_password.trim().is_empty() {
        command.env("WADDLE_COMPLIANCE_ADMIN_PASSWORD", admin_password);
    }

    if let Some(value) = enabled_specs {
        command.env("WADDLE_COMPLIANCE_ENABLED_SPECS", value);
    }
    if let Some(value) = disabled_specs {
        command.env("WADDLE_COMPLIANCE_DISABLED_SPECS", value);
    }

    let status = command
        .status()
        .context("Running compliance harness command")?;
    if status.success() {
        return Ok(());
    }

    Err(anyhow::anyhow!(
        "Compliance harness exited with status {status}"
    ))
}

fn resolve_artifact_dir(path: &str) -> Result<PathBuf> {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return Ok(candidate);
    }

    let cwd = std::env::current_dir().context("Resolving current working directory")?;
    Ok(cwd.join(candidate))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// Show current login status
async fn run_status() -> Result<()> {
    match login::load_credentials() {
        Ok(creds) => {
            println!("Logged in as: @{}", creds.handle);
            println!("DID: {}", creds.did);
            println!("JID: {}", creds.jid);
            println!("XMPP: {}:{}", creds.xmpp_host, creds.xmpp_port);
            println!("API: {}", creds.server_url);
        }
        Err(_) => {
            println!("Not logged in.");
            println!();
            println!("Run 'waddle login -u <handle>' to login with your Bluesky account.");
        }
    }
    Ok(())
}

/// Logout and clear saved credentials
async fn run_logout() -> Result<()> {
    match login::clear_credentials() {
        Ok(()) => {
            println!("Logged out successfully.");
        }
        Err(e) => {
            eprintln!("Failed to logout: {}", e);
        }
    }
    Ok(())
}

/// Create a new waddle
async fn run_create(name: &str, description: Option<&str>, is_public: bool) -> Result<()> {
    // Load credentials
    let creds = login::load_credentials()
        .map_err(|_| anyhow::anyhow!("Not logged in. Run 'waddle login -u <handle>' first."))?;

    println!("Creating waddle \"{}\"...", name);

    // Build the request
    let mut request = api::CreateWaddleRequest::new(name);
    if let Some(desc) = description {
        request = request.with_description(desc);
    }
    request = request.with_public(is_public);

    // Create API client and make the request
    let api_client = api::ApiClient::new(&creds.server_url, &creds.token);
    match api_client.create_waddle(request).await {
        Ok(waddle) => {
            println!();
            println!("✓ Waddle created successfully!");
            println!();
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
