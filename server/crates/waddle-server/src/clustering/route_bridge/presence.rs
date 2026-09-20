//! #1803 receiving side: answer "do I know this exact full JID?" for the
//! account whose `UserActor` claim this node holds.
//!
//! The room's host node evicts a ghost occupant only on a definitive
//! negative, so every branch that cannot rule the resource out answers
//! [`LocalResourcePresence::Present`] — a probe that cannot read its own
//! state is not proof of absence, and an eviction is not reversible.

use super::delivery::receiver::user_entity;
use super::*;

/// Bound on the receiver-side claim read. The executor runs in a delegated
/// relay task that outlives the asker's one-second budget, so a stalled
/// control-plane pool must not keep one pending task alive per stalled-row
/// repair attempt. On elapse the executor answers `Present` — fail closed,
/// never an absence conclusion.
pub(super) const RESOURCE_PRESENCE_CLAIM_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Owner-side answer to a #1803 cross-node resource-presence probe, executed
/// against THIS node's claim, actor tree and SM store. The relay actor maps
/// these onto [`super::super::relay::RelayResourcePresenceReply`] — kept as a
/// separate enum so the bridge does not depend on relay wire types, exactly
/// like [`super::LocalMediaGrantReassertion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalResourcePresence {
    /// This node's `UserActor` tree lists the exact resource (a local socket
    /// or a registered-remote mirror), or a resumable XEP-0198 session exists
    /// for it — including every read that could not answer.
    Present,
    /// This node holds the fresh `UserActor` claim for the account and knows
    /// nothing about the resource. The only authoritative negative.
    Absent,
    /// This node does not hold a fresh claim for the account, so it has no
    /// authority to answer.
    NotOwner,
}

impl OrderedRelayDeliveryBridge {
    /// Execute a relayed resource-presence probe on this node — the receiving
    /// side of #1803. Read-only: no claim is acquired, no actor is spawned
    /// (`try_get_resources_for_user` resolves an EXISTING `UserActor` and
    /// answers `Ok(vec![])` for a bare JID that has none), and no client-
    /// visible state changes, so a duplicate or abandoned ask costs nothing.
    pub async fn resource_presence_local(&self, target: &jid::FullJid) -> LocalResourcePresence {
        let Some(services) = self.services.get() else {
            // Not wired yet: this node cannot read its own state, which is
            // never proof that the resource is gone.
            return LocalResourcePresence::Present;
        };
        let bare = target.to_bare();
        // Authority gate, mirroring every other receiver gate in this module:
        // answer only while this node holds the claim WITH a fresh lease. A
        // deposed node's residual actor tree is exactly the stale evidence
        // this probe exists to avoid trusting.
        let me = services.node_identity.current();
        match tokio::time::timeout(
            RESOURCE_PRESENCE_CLAIM_READ_TIMEOUT,
            services.claim_store.current_claim(&user_entity(&bare)),
        )
        .await
        {
            Ok(Ok(Some(snapshot))) if snapshot.owner_lease_fresh && snapshot.owner == me => {}
            Ok(Ok(_)) => return LocalResourcePresence::NotOwner,
            Ok(Err(error)) => {
                tracing::debug!(
                    jid = %bare,
                    %error,
                    "resource-presence probe could not read the ownership claim"
                );
                return LocalResourcePresence::Present;
            }
            Err(_elapsed) => {
                tracing::debug!(
                    jid = %bare,
                    "resource-presence probe timed out reading the ownership claim"
                );
                return LocalResourcePresence::Present;
            }
        }
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
        // A detached resource has no actor-tree entry but may resume at any
        // moment; the probe reads this node's memory AND the shared durable
        // store, and a store it cannot read counts as present.
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
