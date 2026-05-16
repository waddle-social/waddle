//! XEP-0050: Ad-Hoc Commands
//!
//! Provides a framework for executing multi-step commands between XMPP
//! entities using data forms (XEP-0004).
//!
//! ## Overview
//!
//! Ad-hoc commands allow entities to advertise and execute structured operations.
//! Commands are discovered via service discovery (XEP-0030) on the well-known
//! node `http://jabber.org/protocol/commands`, and executed as IQ set/result
//! exchanges that may span multiple steps.
//!
//! ## Flow
//!
//! 1. **Discovery**: Client queries disco#items on the commands node to list
//!    available commands.
//! 2. **Execution**: Client sends `<command action='execute'/>` to start.
//! 3. **Multi-step**: Server responds with status `executing` and a data form;
//!    client fills and submits. This repeats until the server sets status
//!    `completed` or `canceled`.
//!
//! ## Runtime Status
//!
//! This module provides parser/builder helpers for XEP-0050 payloads.
//! The server runtime advertises `http://jabber.org/protocol/commands`
//! in `server_features()` and dispatches command IQs via the
//! `CommandRegistry`; unknown commands return XEP-0050 §3.5
//! `<bad-request/>` / `<item-not-found/>` from the dispatcher
//! rather than a blanket service-unavailable.

mod element;
mod iq;
mod types;

pub use iq::*;
pub use types::*;

#[cfg(test)]
mod tests;

/// Namespace for XEP-0050 Ad-Hoc Commands.
pub const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";

/// Well-known disco#items node for listing available commands.
pub const NODE_COMMANDS: &str = "http://jabber.org/protocol/commands";
