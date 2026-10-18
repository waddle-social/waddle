//! Waddle for Linux: GTK4 + libadwaita.
//!
//! This is the scaffold from `claude/linux-client-plan.md` Phase 2.1:
//! device-flow sign-in, accounts in the Secret Service, and the adaptive
//! shell. Client state (rooms, timelines, presence) arrives with
//! `waddle-client-session` (ADR-0019); nothing here keeps its own copy.

mod application;
mod attachments;
mod auth;
mod runtime;
mod session;
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
