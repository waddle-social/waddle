//! Connection lifecycle as a typed state machine.
//!
//! Replaces the six loose mutable variables (`authenticated`, `session_jid`,
//! `resource_bound`, `pending_scram`, …) that the existing WebSocket handler
//! threads through every function call. Each variant carries exactly the
//! state that is valid in that phase; the type system forbids illegal
//! access (e.g. reading the bound JID before authentication succeeds).

use jid::{BareJid, FullJid};
use std::collections::HashSet;

/// XMPP connection lifecycle phases.
///
/// Later migration steps add:
/// - `LoadingCredentials { callback_id, mechanism }` — transient phase while
///   a SCRAM credential lookup or OAuth bearer validation is in flight.
/// - `ScramChallenge { scram_server, stored_key, server_key, username }` —
///   SCRAM challenge sent, awaiting client-final-message.
/// - `Authenticated { session, bare_jid }` — SASL succeeded, awaiting
///   resource binding IQ.
#[derive(Debug)]
pub enum ConnectionPhase {
    /// Stream opened, awaiting SASL `<auth>`. The initial phase.
    Unauthenticated,

    /// Fully bound and registered. The connection may send/receive stanzas.
    ///
    /// `joined_rooms` is tracked here (rather than in a registry lookup) so
    /// the state machine can emit `BroadcastToRoom` leave presences on
    /// `TransportClosed` without needing any cross-connection state. See the
    /// plan's *Design patterns* section.
    Ready {
        /// The connection's bound full JID, e.g. `alice@waddle.social/web-1`.
        full_jid: FullJid,
        /// MUC rooms this connection has joined. Kept minimal — only what
        /// is needed for on-disconnect cleanup.
        joined_rooms: HashSet<BareJid>,
    },
}

impl ConnectionPhase {
    /// The initial phase for a freshly-opened transport.
    pub fn new() -> Self {
        Self::Unauthenticated
    }
}

impl Default for ConnectionPhase {
    fn default() -> Self {
        Self::new()
    }
}
