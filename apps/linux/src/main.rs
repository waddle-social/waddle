//! Waddle for Linux: GTK4 + libadwaita.
//!
//! Device-flow sign-in, accounts in the Secret Service, and an adaptive
//! chat shell. Timeline and judgment state use `waddle-client-session`
//! (ADR-0019); GTK renders conversations and separately paged threads.

mod application;
mod attachments;
mod auth;
mod composer;
mod judgments;
mod runtime;
mod session;
mod thread_updates;
mod threads;
mod ui;

use gtk::glib;
use gtk::prelude::*;

pub const APP_ID: &str = "social.waddle.Waddle";

/// rustls needs a process-wide crypto provider; ring, as in
/// `waddle-xmpp-client`. Installing twice is harmless.
pub(crate) fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    install_crypto_provider();
    application::WaddleApplication::new().run()
}
