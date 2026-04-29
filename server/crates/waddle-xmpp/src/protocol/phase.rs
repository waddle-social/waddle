//! Connection lifecycle as a typed state machine.
//!
//! Replaces the six loose mutable variables (`authenticated`, `session_jid`,
//! `resource_bound`, `pending_scram`, …) that the existing WebSocket handler
//! threads through every function call. Each variant carries exactly the
//! state that is valid in that phase; the type system forbids illegal
//! access (e.g. reading the bound JID before authentication succeeds).

use crate::auth::ScramServer;
use jid::{BareJid, FullJid};
use std::collections::HashSet;
use std::fmt;

/// In-flight SCRAM challenge state held between `<auth/>` and `<response/>`.
pub struct ScramPendingState {
    scram_server: ScramServer,
    stored_key: Vec<u8>,
    server_key: Vec<u8>,
    username: String,
}

impl fmt::Debug for ScramPendingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScramPendingState")
            .field("scram_server", &"<redacted>")
            .field("stored_key", &"<redacted>")
            .field("server_key", &"<redacted>")
            .field("username", &self.username)
            .finish()
    }
}

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
    ScramPending { auth: ScramPendingState },

    /// SASL succeeded and the connection now has an authenticated bare JID,
    /// but resource binding has not yet completed.
    Authenticated { bare_jid: BareJid },

    /// Fully bound and registered. The connection may send/receive stanzas.
    ///
    /// `joined_rooms` is tracked here (rather than in a registry lookup) so
    /// the state machine can emit room leave presences on `TransportClosed`
    /// without needing any cross-connection state. See the plan's
    /// *Design patterns* section.
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
    /// Carries the bound JID (if any) so cleanup can unregister the connection
    /// even after the phase has transitioned out of `Ready`.
    Closing { full_jid: Option<FullJid> },
}

impl ConnectionPhase {
    /// The initial phase for a freshly-opened transport.
    pub fn new() -> Self {
        Self::Unauthenticated
    }

    pub fn scram_pending(auth: ScramPendingState) -> Self {
        Self::ScramPending { auth }
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

    pub fn is_resumed(&self) -> bool {
        matches!(self, Self::Ready { resumed: true, .. })
    }

    /// Create a `Closing` phase, preserving the bound JID if one was active.
    pub fn closing(full_jid: Option<FullJid>) -> Self {
        Self::Closing { full_jid }
    }

    pub fn bound_jid(&self) -> Option<&FullJid> {
        match self {
            Self::Ready { full_jid, .. } => Some(full_jid),
            _ => None,
        }
    }

    /// Returns the JID for post-close cleanup.
    ///
    /// Returns `Some` when the phase is `Ready` or `Closing` with a bound JID.
    /// Returns `None` when closing before binding completes.
    pub fn cleanup_jid(&self) -> Option<&FullJid> {
        match self {
            Self::Ready { full_jid, .. }
            | Self::Closing {
                full_jid: Some(full_jid),
            } => Some(full_jid),
            _ => None,
        }
    }

    pub fn authenticated_bare_jid(&self) -> Option<&BareJid> {
        match self {
            Self::Authenticated { bare_jid } => Some(bare_jid),
            _ => None,
        }
    }

    pub fn allows_sasl_auth(&self) -> bool {
        matches!(self, Self::Unauthenticated)
    }

    pub fn allows_sasl_response(&self) -> bool {
        matches!(self, Self::ScramPending { .. })
    }

    pub fn allows_resource_binding(&self) -> bool {
        matches!(self, Self::Authenticated { .. })
    }

    pub fn allows_stream_management_enable(&self) -> bool {
        self.is_ready()
    }

    pub fn allows_stream_management_resume(&self) -> bool {
        matches!(self, Self::Authenticated { .. })
    }

    pub fn is_closing(&self) -> bool {
        matches!(self, Self::Closing { .. })
    }

    pub fn scram_pending_username(&self) -> Option<&str> {
        match self {
            Self::ScramPending { auth } => Some(auth.username()),
            _ => None,
        }
    }

    pub fn take_scram_pending(&mut self) -> Option<ScramPendingState> {
        let previous = std::mem::replace(self, Self::Unauthenticated);
        match previous {
            Self::ScramPending { auth } => Some(auth),
            other => {
                *self = other;
                None
            }
        }
    }
}

impl ScramPendingState {
    pub fn new(
        scram_server: ScramServer,
        stored_key: Vec<u8>,
        server_key: Vec<u8>,
        username: impl Into<String>,
    ) -> Self {
        Self {
            scram_server,
            stored_key,
            server_key,
            username: username.into(),
        }
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn process_client_final(
        &mut self,
        client_final: &str,
    ) -> Result<crate::auth::ServerFinalMessage, crate::XmppError> {
        self.scram_server
            .process_client_final(client_final, &self.stored_key, &self.server_key)
    }
}

impl Default for ConnectionPhase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionPhase, ScramPendingState};
    use crate::auth::ScramServer;
    use jid::FullJid;

    fn scram_pending_state(username: &str) -> ScramPendingState {
        ScramPendingState::new(ScramServer::new(), vec![1, 2, 3], vec![4, 5, 6], username)
    }

    #[test]
    fn unauthenticated_phase_allows_initial_auth_only() {
        let phase = ConnectionPhase::new();
        assert!(phase.allows_sasl_auth());
        assert!(!phase.allows_stream_management_resume());
        assert!(!phase.allows_sasl_response());
        assert!(!phase.allows_resource_binding());
        assert!(!phase.allows_stream_management_enable());
    }

    #[test]
    fn scram_pending_phase_only_allows_sasl_response() {
        let phase = ConnectionPhase::scram_pending(scram_pending_state("alice"));
        assert!(!phase.allows_sasl_auth());
        assert!(phase.allows_sasl_response());
        assert!(!phase.allows_resource_binding());
        assert!(!phase.allows_stream_management_enable());
        assert!(!phase.allows_stream_management_resume());
        assert_eq!(phase.scram_pending_username(), Some("alice"));
    }

    #[test]
    fn authenticated_phase_allows_binding_and_resume() {
        let full_jid = "alice@example.com/pending".parse().expect("valid jid");
        let phase = ConnectionPhase::authenticated(&full_jid);
        assert!(!phase.allows_sasl_auth());
        assert!(!phase.allows_sasl_response());
        assert!(phase.allows_resource_binding());
        assert!(!phase.allows_stream_management_enable());
        assert!(phase.allows_stream_management_resume());
    }

    #[test]
    fn ready_phase_allows_stream_management_enable() {
        let full_jid = "alice@example.com/web".parse().expect("valid jid");
        let phase = ConnectionPhase::ready(full_jid, false);
        assert!(!phase.allows_sasl_auth());
        assert!(!phase.allows_sasl_response());
        assert!(!phase.allows_resource_binding());
        assert!(phase.allows_stream_management_enable());
        assert!(!phase.allows_stream_management_resume());
    }

    #[test]
    fn closing_phase_rejects_all_legal_transitions() {
        let phase = ConnectionPhase::closing(None);
        assert!(phase.is_closing());
        assert!(!phase.allows_sasl_auth());
        assert!(!phase.allows_sasl_response());
        assert!(!phase.allows_resource_binding());
        assert!(!phase.allows_stream_management_enable());
        assert!(!phase.allows_stream_management_resume());
    }

    #[test]
    fn cleanup_jid_survives_closing_transition() {
        let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
        let ready = ConnectionPhase::ready(jid.clone(), false);
        let closing = ConnectionPhase::closing(Some(jid.clone()));
        let closing_nobound = ConnectionPhase::closing(None);

        assert_eq!(ready.cleanup_jid(), Some(&jid));
        assert_eq!(closing.cleanup_jid(), Some(&jid));
        assert_eq!(closing_nobound.cleanup_jid(), None);
        // bound_jid stays None in Closing so stanza routing still rejects
        assert_eq!(closing.bound_jid(), None);
    }

    #[test]
    fn resumed_ready_phase_reports_resume_status() {
        let fresh_jid = "alice@example.com/web".parse().expect("valid jid");
        let resumed_jid = "alice@example.com/mobile".parse().expect("valid jid");

        assert!(!ConnectionPhase::ready(fresh_jid, false).is_resumed());
        assert!(ConnectionPhase::ready(resumed_jid, true).is_resumed());
    }

    #[test]
    fn bound_jid_is_only_available_for_ready_phase() {
        let pending_jid: FullJid = "alice@example.com/pending".parse().expect("valid jid");
        let full_jid: FullJid = "alice@example.com/web".parse().expect("valid jid");

        assert!(ConnectionPhase::new().bound_jid().is_none());
        assert!(ConnectionPhase::authenticated(&pending_jid)
            .bound_jid()
            .is_none());
        assert_eq!(
            ConnectionPhase::ready(full_jid.clone(), false)
                .bound_jid()
                .map(ToString::to_string),
            Some(full_jid.to_string())
        );
    }

    #[test]
    fn take_scram_pending_returns_state_and_resets_phase() {
        let mut phase = ConnectionPhase::scram_pending(scram_pending_state("alice"));
        let auth = phase.take_scram_pending().expect("scram pending state");
        assert_eq!(auth.username(), "alice");
        assert!(matches!(phase, ConnectionPhase::Unauthenticated));
    }

    #[test]
    fn scram_pending_debug_redacts_secret_material() {
        let phase = ConnectionPhase::scram_pending(scram_pending_state("alice"));
        let debug = format!("{phase:?}");
        assert!(debug.contains("alice"));
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("[1, 2, 3]"));
        assert!(!debug.contains("[4, 5, 6]"));
    }
}
