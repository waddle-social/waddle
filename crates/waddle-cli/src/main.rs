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
use tracing::{info, Level};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod app;
mod config;
mod event;
mod ui;

use app::{App, Focus};
use config::Config;
use event::{key_to_action, Event, EventHandler, KeyAction};
use ui::render_layout;

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

    loop {
        // Render the UI
        terminal.draw(|frame| {
            render_layout(frame, &app, &config);
        })?;

        // Handle events
        if let Some(event) = events.next().await {
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
                                app.sidebar_select();
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
                                app.input_submit();
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

        if app.should_quit {
            break;
        }
    }

    Ok(())
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
