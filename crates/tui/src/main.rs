mod app;
mod error;
mod input;
mod state;
mod ui;

use std::sync::Arc;

use waddle_core::config;
use waddle_core::event::BroadcastEventBus;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = match config::load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load config: {e}");
            std::process::exit(1);
        }
    };

    let event_bus = Arc::new(BroadcastEventBus::new(config.event_bus.channel_capacity));

    if let Err(e) = app::TuiApp::run(event_bus, &config).await {
        eprintln!("TUI error: {e}");
        std::process::exit(1);
    }
}
