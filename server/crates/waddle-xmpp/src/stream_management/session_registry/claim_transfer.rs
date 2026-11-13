//! Taking over an SM session's claim for a resume whose session state is
//! held outside this registry.
//!
//! [`super::cross_node_resume`] moves a claim and then hydrates the
//! session's persisted rows into this registry. A session that the durable
//! stream-management authority owns is never hydrated here: its lifecycle,
//! counters and retained frames stay in that authority's store, which
//! asserts the exact claim row on every write. A resume of such a session on
//! a node that does not hold its claim therefore needs the claim alone,
//! recorded as this node's fence so the registry releases it when the
//! session ends.
//!
//! The caller decides when a claim may move, because only it can read the
//! session's stored state. This module supplies the three steps it combines:
//! observe who holds the claim, ask a live holder to detach its connection,
//! and move the claim with one compare-and-swap bound to the observed epoch.
//! The swap is never repeated: a swap that loses means another resume or
//! the orphan reaper moved the claim first.

use std::time::Duration;

use jid::BareJid;

use crate::ownership::{
    verify_resume_identity, ClaimEpoch, ClaimError, CurrentNodeIdentityGuard, Entity, EntityType,
    NodeIdentity, StalePredicate,
};
use crate::stream_management::persistence::SmClaimFence;

use super::core::{InMemorySmSessionRegistry, ReclaimedClaimAdmissionError};
use super::cross_node_resume::RemoteResumeAskOutcome;
use super::SmRegistryError;

/// Bound on the claim swap and on the lookup that settles a swap whose
/// reply was lost.
const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(10);

/// A session claim another node holds, as observed at one point in time.
#[derive(Debug, Clone)]
pub struct ForeignSessionClaim {
    entity: Entity,
    owner: NodeIdentity,
    observed_epoch: ClaimEpoch,
    owner_lease_fresh: bool,
}

impl ForeignSessionClaim {
    /// The node incarnation that held the claim.
    pub fn owner(&self) -> &NodeIdentity {
        &self.owner
    }

    /// The claim epoch that was on file. The takeover binds this value.
    pub fn observed_epoch(&self) -> ClaimEpoch {
        self.observed_epoch
    }

    /// Whether the holder's node lease was not committed as expired.
    pub fn owner_lease_fresh(&self) -> bool {
        self.owner_lease_fresh
    }
}

/// Who holds a session's claim.
#[derive(Debug, Clone)]
pub enum ObservedSessionClaim {
    /// No node holds the claim.
    Unclaimed,
    /// This node incarnation holds the claim.
    HeldHere,
    /// Another node, or an earlier incarnation of this one, holds it.
    HeldElsewhere(ForeignSessionClaim),
}

/// Why a foreign claim may be moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionClaimEvidence {
    /// No connection of the holder owns the session any more: the holder
    /// detached it, or the session's stored owner was committed under a
    /// claim that no longer exists. The claim moves by the identity-bound
    /// resume swap, which does not require the holder's lease to be stale.
    OwnerReleased,
    /// The holder's node lease is committed as expired. The claim moves by
    /// the owner-stale swap, which the store refuses for a fresh lease.
    OwnerLeaseExpired,
}

/// The result of [`InMemorySmSessionRegistry::take_session_claim`].
pub enum SessionClaimTakeover {
    /// This node holds the claim under `fence`, and the fence is recorded
    /// for release. `publication` keeps this node's identity from rotating
    /// while the caller commits under the fence.
    Taken {
        publication: CurrentNodeIdentityGuard,
        fence: SmClaimFence,
    },
    /// The requester is not the account the session belongs to. Nothing
    /// was written.
    NotAuthorized,
    /// The claim was not at the observed epoch, its holder's lease is not
    /// stale, or another operation of this node is moving the same claim.
    /// This node does not hold the claim.
    Lost,
    /// The swap or its bookkeeping failed. A claim that was won is released
    /// or queued for release; the caller holds nothing.
    Unavailable,
}

impl std::fmt::Debug for SessionClaimTakeover {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Taken { fence, .. } => formatter
                .debug_struct("Taken")
                .field("fence", fence)
                .finish_non_exhaustive(),
            Self::NotAuthorized => formatter.write_str("NotAuthorized"),
            Self::Lost => formatter.write_str("Lost"),
            Self::Unavailable => formatter.write_str("Unavailable"),
        }
    }
}

impl InMemorySmSessionRegistry {
    /// Read who holds `stream_id`'s claim. The read authorizes nothing: it
    /// names the holder to ask and the epoch a takeover binds.
    pub async fn observe_session_claim(
        &self,
        stream_id: &str,
    ) -> Result<ObservedSessionClaim, SmRegistryError> {
        let entity = Entity::new(EntityType::SmSession, stream_id.to_string());
        let snapshot =
            self.claim_store
                .current_claim(&entity)
                .await
                .map_err(|error| match error {
                    ClaimError::Backend(_) | ClaimError::Poisoned => {
                        SmRegistryError::StorageUnavailable(
                            super::traits::StorageOutageCause::Backend,
                        )
                    }
                    other => SmRegistryError::Internal(other.to_string()),
                })?;
        let Some(snapshot) = snapshot else {
            return Ok(ObservedSessionClaim::Unclaimed);
        };
        if snapshot.owner == self.node_identity.current() {
            return Ok(ObservedSessionClaim::HeldHere);
        }
        Ok(ObservedSessionClaim::HeldElsewhere(ForeignSessionClaim {
            entity,
            owner: snapshot.owner,
            observed_epoch: snapshot.claim_epoch,
            owner_lease_fresh: snapshot.owner_lease_fresh,
        }))
    }

    /// Ask the node holding `claim` to detach the connection that owns the
    /// session, on behalf of `requester_bare_jid`. `None` when no cross-node
    /// asker is wired, so the holder cannot be reached from this node.
    pub async fn ask_session_owner_to_detach(
        &self,
        claim: &ForeignSessionClaim,
        requester_bare_jid: &BareJid,
    ) -> Option<RemoteResumeAskOutcome> {
        let asker = self.remote_resume.clone()?;
        Some(
            asker
                .ask_remote_detach(&claim.owner.node_id, &claim.entity.id, requester_bare_jid)
                .await,
        )
    }

    /// Move `claim` to this node and record the resulting fence, without
    /// hydrating any session state.
    ///
    /// `requester_bare_jid` is the account the resuming connection
    /// authenticated as and `session_bare_jid` the account the session is
    /// bound to; they must be equal before anything is written. The swap is
    /// bound to the epoch `claim` was observed at and is issued once.
    ///
    /// The caller runs this to completion. A swap whose reply is lost is
    /// settled by one lookup that waits for the swap's own transaction; when
    /// that lookup also fails the caller holds nothing, and a claim this
    /// node did win is taken up again by its next idempotent claim of the
    /// same session.
    pub async fn take_session_claim(
        &self,
        claim: &ForeignSessionClaim,
        requester_bare_jid: &BareJid,
        session_bare_jid: &BareJid,
        evidence: SessionClaimEvidence,
    ) -> SessionClaimTakeover {
        let Some(witness) = verify_resume_identity(requester_bare_jid, session_bare_jid) else {
            return SessionClaimTakeover::NotAuthorized;
        };
        let entity = &claim.entity;
        let me = self.node_identity.current();
        let reservation = match self.reserve_reclaimed_claim_fence_capacity(&entity.id) {
            Ok(reservation) => reservation,
            Err(ReclaimedClaimAdmissionError::BusySameStream) => return SessionClaimTakeover::Lost,
            Err(
                ReclaimedClaimAdmissionError::Exhausted
                | ReclaimedClaimAdmissionError::LockPoisoned,
            ) => return SessionClaimTakeover::Unavailable,
        };
        let swap = async {
            match evidence {
                SessionClaimEvidence::OwnerReleased => {
                    self.claim_store
                        .steal_for_resume(entity, claim.observed_epoch, witness, &me)
                        .await
                }
                SessionClaimEvidence::OwnerLeaseExpired => {
                    self.claim_store
                        .steal_stale(
                            entity,
                            claim.observed_epoch,
                            StalePredicate::OwnerStale,
                            &me,
                        )
                        .await
                }
            }
        };
        let epoch = match tokio::time::timeout(TAKEOVER_TIMEOUT, swap).await {
            Ok(Ok(epoch)) => epoch,
            Ok(Err(ClaimError::Conflict | ClaimError::AlreadyClaimed | ClaimError::Draining)) => {
                self.cancel_reclaimed_claim_capacity(entity, reservation);
                return SessionClaimTakeover::Lost;
            }
            Ok(Err(error)) => {
                self.cancel_reclaimed_claim_capacity(entity, reservation);
                tracing::warn!(entity = %entity, %error, "SM session claim takeover failed");
                return SessionClaimTakeover::Unavailable;
            }
            Err(_elapsed) => {
                // The swap may still commit. The lookup below queues behind
                // it on the claim row, so it reports the swap's outcome.
                let settled = tokio::time::timeout(
                    TAKEOVER_TIMEOUT,
                    self.claim_store.current_claim_after_pending_writes(entity),
                )
                .await;
                match settled {
                    Ok(Ok(Some(snapshot))) if snapshot.owner == me => snapshot.claim_epoch,
                    Ok(Ok(_)) => {
                        self.cancel_reclaimed_claim_capacity(entity, reservation);
                        return SessionClaimTakeover::Lost;
                    }
                    Ok(Err(_)) | Err(_) => {
                        self.cancel_reclaimed_claim_capacity(entity, reservation);
                        tracing::warn!(
                            entity = %entity,
                            "SM session claim takeover outcome is unknown; the claim is not recorded"
                        );
                        return SessionClaimTakeover::Unavailable;
                    }
                }
            }
        };

        let fence = SmClaimFence::new(me.clone(), epoch);
        if let Some(publication) = self.node_identity.guard_if_current(&me).await {
            // The swap's own result proves `fence` is the store's current
            // owner and epoch, so it replaces any older fence this node
            // still records for the session.
            if self.try_record_verified_reclaimed_fence(&entity.id, fence.clone(), reservation) {
                return SessionClaimTakeover::Taken { publication, fence };
            }
        }
        // The claim belongs to an incarnation that is being replaced, or it
        // could not be recorded. Nothing was committed under it; it is
        // released so that a later resume finds a free claim.
        if let Err(error) = self
            .release_reclaimed_claim(entity, &fence, reservation)
            .await
        {
            tracing::warn!(
                entity = %entity,
                %error,
                "an SM session claim taken for a resume was not released; its release is queued"
            );
        }
        SessionClaimTakeover::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ownership::{ClaimStore as _, InProcessClaimStore, SharedNodeIdentity};

    const STREAM: &str = "5f0c1d1e-7f0b-4a55-9d53-0d1b2f6a9c11";

    fn node(name: &str) -> NodeIdentity {
        NodeIdentity::new(name, format!("{name}-incarnation"))
    }

    fn registry_for(
        store: &Arc<InProcessClaimStore>,
        identity: &NodeIdentity,
    ) -> InMemorySmSessionRegistry {
        InMemorySmSessionRegistry::new()
            .with_claim_store(store.clone(), SharedNodeIdentity::new(identity.clone()))
    }

    fn alice() -> BareJid {
        "alice@example.com".parse().expect("bare JID")
    }

    async fn foreign_claim(registry: &InMemorySmSessionRegistry) -> ForeignSessionClaim {
        match registry
            .observe_session_claim(STREAM)
            .await
            .expect("claim lookup")
        {
            ObservedSessionClaim::HeldElsewhere(claim) => claim,
            other => panic!("expected a foreign claim, observed {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_released_owner_claim_moves_and_is_recorded_without_session_state() {
        let store = Arc::new(InProcessClaimStore::new());
        let (node_a, node_b) = (node("a"), node("b"));
        let registry_a = registry_for(&store, &node_a);
        let registry_b = registry_for(&store, &node_b);
        let held = registry_a
            .ensure_session_claim(STREAM)
            .await
            .expect("node A claims the session");
        drop(held);

        let claim = foreign_claim(&registry_b).await;
        assert_eq!(claim.owner(), &node_a);
        let takeover = registry_b
            .take_session_claim(
                &claim,
                &alice(),
                &alice(),
                SessionClaimEvidence::OwnerReleased,
            )
            .await;
        let SessionClaimTakeover::Taken { fence, .. } = takeover else {
            panic!("takeover must win: {takeover:?}");
        };

        assert_eq!(fence.owner(), &node_b);
        assert_eq!(registry_b.current_sm_claim_fence(STREAM), Some(fence));
        assert!(matches!(
            registry_b.observe_session_claim(STREAM).await,
            Ok(ObservedSessionClaim::HeldHere)
        ));
        assert!(
            registry_b
                .claim_session(STREAM)
                .await
                .expect("registry lookup")
                .is_none(),
            "a claim-only takeover hydrates no session into the registry"
        );
    }

    #[tokio::test]
    async fn two_takeovers_of_one_observed_epoch_have_one_winner() {
        let store = Arc::new(InProcessClaimStore::new());
        let (node_a, node_b, node_c) = (node("a"), node("b"), node("c"));
        let registry_a = registry_for(&store, &node_a);
        let registry_b = registry_for(&store, &node_b);
        let registry_c = registry_for(&store, &node_c);
        drop(
            registry_a
                .ensure_session_claim(STREAM)
                .await
                .expect("claim"),
        );

        let observed_by_b = foreign_claim(&registry_b).await;
        let observed_by_c = foreign_claim(&registry_c).await;
        let (alice_b, alice_c) = (alice(), alice());
        let (taken_by_b, taken_by_c) = tokio::join!(
            registry_b.take_session_claim(
                &observed_by_b,
                &alice_b,
                &alice_b,
                SessionClaimEvidence::OwnerReleased,
            ),
            registry_c.take_session_claim(
                &observed_by_c,
                &alice_c,
                &alice_c,
                SessionClaimEvidence::OwnerReleased,
            ),
        );

        let won = |takeover: &SessionClaimTakeover| {
            matches!(takeover, SessionClaimTakeover::Taken { .. })
        };
        let lost = |takeover: &SessionClaimTakeover| matches!(takeover, SessionClaimTakeover::Lost);
        assert!(
            (won(&taken_by_b) && lost(&taken_by_c)) || (lost(&taken_by_b) && won(&taken_by_c)),
            "exactly one takeover wins: {taken_by_b:?} / {taken_by_c:?}"
        );
        let loser = if won(&taken_by_b) {
            &registry_c
        } else {
            &registry_b
        };
        assert_eq!(loser.current_sm_claim_fence(STREAM), None);
        assert_eq!(loser.claim_fence_capacity_used(), 0);
    }

    #[tokio::test]
    async fn another_account_cannot_move_the_claim() {
        let store = Arc::new(InProcessClaimStore::new());
        let (node_a, node_b) = (node("a"), node("b"));
        let registry_a = registry_for(&store, &node_a);
        let registry_b = registry_for(&store, &node_b);
        drop(
            registry_a
                .ensure_session_claim(STREAM)
                .await
                .expect("claim"),
        );

        let claim = foreign_claim(&registry_b).await;
        let mallory: BareJid = "mallory@example.com".parse().expect("bare JID");
        let takeover = registry_b
            .take_session_claim(
                &claim,
                &mallory,
                &alice(),
                SessionClaimEvidence::OwnerReleased,
            )
            .await;

        assert!(matches!(takeover, SessionClaimTakeover::NotAuthorized));
        let entity = Entity::new(EntityType::SmSession, STREAM.to_string());
        let current = store
            .current_claim(&entity)
            .await
            .expect("claim lookup")
            .expect("claim row");
        assert_eq!(current.owner, node_a);
        assert_eq!(current.claim_epoch, claim.observed_epoch());
    }
}
