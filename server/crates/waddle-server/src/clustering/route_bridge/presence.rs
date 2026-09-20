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
/// against THIS node's connection registry, actor tree and SM store. The relay
/// actor maps these onto
/// [`super::super::relay::RelayResourcePresenceReply`] — kept as a separate
/// enum so the bridge does not depend on relay wire types, exactly like
/// [`super::LocalMediaGrantReassertion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalResourcePresence {
    /// This node hosts a socket for the exact resource, its `UserActor` tree
    /// lists it (a local socket or a registered-remote mirror), or it holds a
    /// resumable XEP-0198 session for it — including every read that could
    /// not answer.
    Present,
    /// This node knows nothing about the resource: no socket, no actor-tree
    /// entry, and no resumable session. The only authoritative negative.
    Absent,
}

impl OrderedRelayDeliveryBridge {
    /// Execute a relayed resource-presence probe on this node — the receiving
    /// side of #1803. Read-only: no claim is read or acquired, no actor is
    /// spawned (`try_get_resources_for_user` resolves an EXISTING `UserActor`
    /// and answers `Ok(vec![])` for a bare JID that has none), and no client-
    /// visible state changes, so a duplicate or abandoned ask costs nothing.
    pub async fn resource_presence_local(&self, target: &jid::FullJid) -> LocalResourcePresence {
        let Some(services) = self.services.get() else {
            // Not wired yet: this node cannot read its own state, which is
            // never proof that the resource is gone.
            return LocalResourcePresence::Present;
        };
        // A socket this node holds open is the most direct proof there is,
        // and the one the old claim gate could hide. It also covers the
        // clustered registered-remote mirror, which installs a
        // non-locally-hosted entry for a peer's socket.
        if services.connection_registry.is_connected(target) {
            return LocalResourcePresence::Present;
        }
        let bare = target.to_bare();
        // The non-degrading resource read is deliberate: the routing variant
        // reports an unanswered actor as "no resources", which is the right
        // default for a route and the wrong one for an eviction.
        match waddle_xmpp::registry::try_get_resources_for_user(&services.user_registry, &bare)
            .await
        {
            Ok(resources) if resources.contains(target) => return LocalResourcePresence::Present,
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(
                    jid = %bare,
                    %error,
                    "resource-presence probe could not read the user's resources"
                );
                return LocalResourcePresence::Present;
            }
        }
        // A detached resource has no socket and no actor-tree entry but may
        // resume at any moment; the probe reads this node's memory AND the
        // shared durable store, and a store it cannot read counts as present.
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
                LocalResourcePresence::Absent
            }
        }
    }
}
