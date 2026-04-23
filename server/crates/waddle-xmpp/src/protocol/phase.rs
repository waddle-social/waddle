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
/// - `DetachedResumable { ... }` — transport gone, waiting for XEP-0198
///   resume within the registry TTL.
#[derive(Debug)]
pub enum ConnectionPhase {
    /// Stream opened, awaiting SASL `<auth>`. The initial phase.
    Unauthenticated,

    /// SCRAM client-first has been accepted and the server has sent a
    /// challenge. The next legal client step is `<response>`.
    ScramPending { username: String },

    /// SASL succeeded and the connection now has an authenticated bare JID,
    /// but resource binding has not yet completed.
    Authenticated { bare_jid: BareJid },

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
        /// True when this phase came from XEP-0198 resume rather than a fresh
        /// SASL + bind sequence.
        resumed: bool,
    },

    /// The client has sent `<close/>` and the transport is draining.
    Closing,
}

impl ConnectionPhase {
    /// The initial phase for a freshly-opened transport.
    pub fn new() -> Self {
        Self::Unauthenticated
    }

    pub fn scram_pending(username: impl Into<String>) -> Self {
        Self::ScramPending {
            username: username.into(),
        }
    }

    pub fn authenticated(full_jid: &FullJid) -> Self {
        Self::Authenticated {
            bare_jid: full_jid.to_bare(),
        }
    }

    pub fn ready(full_jid: FullJid, resumed: bool) -> Self {
        Self::ready_with_joined_rooms(full_jid, HashSet::new(), resumed)
    }

    pub fn ready_with_joined_rooms(
        full_jid: FullJid,
        joined_rooms: HashSet<BareJid>,
        resumed: bool,
    ) -> Self {
        Self::Ready {
            full_jid,
            joined_rooms,
            resumed,
        }
    }

    pub fn is_authenticated(&self) -> bool {
        matches!(self, Self::Authenticated { .. } | Self::Ready { .. })
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

impl Default for ConnectionPhase {
    fn default() -> Self {
        Self::new()
    }
}
