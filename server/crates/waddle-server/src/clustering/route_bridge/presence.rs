//! #1803 receiving side: answer "do I know this exact full JID?" about THIS
//! node's own sockets and sessions.
//!
//! Every node is unconditionally authoritative about the sockets it hosts and
//! the XEP-0198 sessions it holds, and about nothing else. No claim gate runs
//! here on purpose: a `UserActor` claim is ROUTING AUTHORITY, not socket
//! liveness. A live, idle socket lives in this node's `ConnectionRegistry` and
//! nothing re-registers it anywhere when the account's claim owner dies or
//! moves, so gating the answer on the claim would make a node deny a socket it
//! is holding open — the exact failure that let a LIVE user be evicted from
//! every room (#1803).
//!
//! The room's host node evicts a ghost occupant only when EVERY peer answers
//! [`LocalResourcePresence::Absent`], so every branch that cannot rule the
//! resource out answers [`LocalResourcePresence::Present`]: a probe that
//! cannot read its own state is not proof of absence, and an eviction is not
//! reversible.

use super::*;

/// Owner-side answer to a #1803 cross-node resource-presence probe, executed
/// against THIS node's locally hosted connections and SM store. The relay
/// actor maps these onto
/// [`super::super::relay::RelayResourcePresenceReply`] — kept as a separate
/// enum so the bridge does not depend on relay wire types, exactly like
/// [`super::LocalMediaGrantReassertion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalResourcePresence {
    /// This node hosts a socket or holds a resumable XEP-0198 session for
    /// the exact resource — including every read that could not answer.
    Present,
    /// This node hosts no socket and holds no resumable session for the
    /// resource. The only authoritative negative about this node.
    Absent,
}

impl OrderedRelayDeliveryBridge {
    /// Execute a relayed resource-presence probe on this node — the receiving
    /// side of #1803. Read-only: no claim is read or acquired, no actor is
    /// spawned, and no client-visible state changes.
    pub async fn resource_presence_local(&self, target: &jid::FullJid) -> LocalResourcePresence {
        let Some(services) = self.services.get() else {
            // Not wired yet: this node cannot read its own state, which is
            // never proof that the resource is gone.
            return LocalResourcePresence::Present;
        };
        // Only this node can vouch for its own sockets. Remote mirrors are
        // routing state and can outlive a missed unregister on a healthy peer.
        if services
            .connection_registry
            .get_entry(target)
            .is_some_and(|entry| entry.is_locally_hosted())
        {
            return LocalResourcePresence::Present;
        }
        // A detached resource has no socket but may resume at any moment.
        // The probe reads this node's memory AND the shared durable store,
        // and a store it cannot read counts as present.
        match services
            .sm_session_registry
            .probe_resumable_session_for_full_jid(target)
            .await
        {
            waddle_xmpp::stream_management::ResumableSessionProbe::Present
            | waddle_xmpp::stream_management::ResumableSessionProbe::Failed => {
                LocalResourcePresence::Present
            }
            waddle_xmpp::stream_management::ResumableSessionProbe::Absent => {
                // Resume publishes its socket before removing the detached
                // session; the asynchronous session read may overlap it.
                if services
                    .connection_registry
                    .get_entry(target)
                    .is_some_and(|entry| entry.is_locally_hosted())
                {
                    LocalResourcePresence::Present
                } else {
                    LocalResourcePresence::Absent
                }
            }
        }
    }
}
