//! #1594 cross-node routing for `participant_joined` media-grant
//! re-assertion: decide from the room's claim which replica should
//! enforce, and relay the re-assert to a fresh foreign claim owner.
//!
//! Split out of `livekit_webhook.rs` so the HTTP webhook module stays
//! HTTP-shaped: claim-store reads, relay-ask plumbing, and the
//! webhook-scoped timeout policy all live here, behind one entry
//! point ([`reassert_on_claim_owner`]).

use jid::{BareJid, FullJid};
#[cfg(feature = "clustering")]
use std::time::Duration as StdDuration;
#[cfg(feature = "clustering")]
use tracing::debug;
use tracing::warn;

use super::livekit_webhook::ReassertOutcome;
use super::websocket::WebSocketState;

/// Fallback when there is no fresh foreign owner to relay to (no
/// clustering, room unclaimed, stale owner lease, or the claim is this
/// node's own): acknowledge the delivery and leave convergence to the
/// owning node's reconciliation pass, exactly the pre-#1594 behavior.
/// (A claim-store read *error* is transient and maps to a LiveKit
/// retry instead — see [`route_from_claim`].)
fn converge_on_reconcile(room_jid: &BareJid, full_jid: &FullJid) -> ReassertOutcome {
    warn!(
        room = %room_jid,
        user = %full_jid.to_bare(),
        "no local room actor on this node; media grants for this join \
         will converge on the owning node's reconciliation pass",
    );
    ReassertOutcome::UnenforceableHere
}

/// #1594: this node has no room actor, so route the re-assert to the
/// replica holding the room's claim instead of waiting for its
/// reconciliation tick. Every path that cannot reach a fresh foreign
/// owner degrades to [`converge_on_reconcile`]; failures where a
/// LiveKit retry could plausibly land after the cluster settles map
/// to [`ReassertOutcome::RetryableFailure`].
/// Where a `participant_joined` re-assert should go, given the room's
/// claim state as read by the webhook-receiving node. Pure so every
/// branch is unit-testable without a claim-store double.
#[cfg(feature = "clustering")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClaimRoute {
    /// A fresh foreign owner holds the claim — relay the re-assert to
    /// this node.
    AskOwner(waddle_xmpp::ownership::NodeIdentity),
    /// No owner worth relaying to (unclaimed, stale lease, or the
    /// claim is our own spawn/teardown race) — acknowledge and leave
    /// it to the reconciler.
    ConvergeOnReconcile,
    /// The claim read itself failed transiently — ask LiveKit to
    /// retry the delivery.
    RetryLater,
}

#[cfg(feature = "clustering")]
fn route_from_claim(
    claim: Result<
        Option<waddle_xmpp::ownership::ClaimSnapshot>,
        waddle_xmpp::ownership::ClaimError,
    >,
    me: &waddle_xmpp::ownership::NodeIdentity,
) -> ClaimRoute {
    match claim {
        // Unclaimed room: nobody holds an authoritative occupant set,
        // so there is no owner to ask and nothing a retry would reach.
        Ok(None) => ClaimRoute::ConvergeOnReconcile,
        // The claim's owner stopped renewing its node lease. Relaying
        // to a possibly-dead node would just time out, and how long
        // until another node steals the claim is unbounded — don't
        // burn LiveKit's bounded retries on it.
        Ok(Some(snapshot)) if !snapshot.owner_lease_fresh => ClaimRoute::ConvergeOnReconcile,
        // This node owns the claim but has no actor — a startup or
        // teardown race. The local reconciliation pass covers it.
        Ok(Some(snapshot)) if snapshot.owner == *me => ClaimRoute::ConvergeOnReconcile,
        Ok(Some(snapshot)) => ClaimRoute::AskOwner(snapshot.owner),
        // A store read error is transient (unlike the structural
        // no-owner cases above): the next LiveKit retry re-reads it.
        Err(_) => ClaimRoute::RetryLater,
    }
}

/// Ask-timeout budget for the webhook-triggered relay hop. Deliberately
/// much tighter than the clustering defaults (5s mailbox / 20s reply):
/// LiveKit abandons a webhook delivery after a few seconds and retries,
/// and a retry that arrives while the previous attempt still holds the
/// dedupe entry is swallowed as a duplicate — so the whole hop must
/// resolve well inside LiveKit's own delivery timeout or the retry
/// budget burns without enforcing anything.
#[cfg(feature = "clustering")]
const WEBHOOK_RELAY_MAILBOX_TIMEOUT: StdDuration = StdDuration::from_secs(1);
/// Deliberately smaller than the receiver's own wedge bounds (two
/// 5s-capped actor asks + a claim read): a merely-slow owner costs
/// this asker a timeout → 503 → one LiveKit retry, but the owner
/// still completes the idempotent re-assert, so the retry confirms
/// cheaply against a warm path. The alternative — waiting out the
/// receiver's worst case — would hold the webhook socket past
/// LiveKit's own delivery timeout, burning the retry anyway.
#[cfg(feature = "clustering")]
const WEBHOOK_RELAY_REPLY_TIMEOUT: StdDuration = StdDuration::from_secs(3);
/// Overall bound on the relay hop. Kademlia name resolution (up to
/// three lookups with ~2.1s of backoff) and the stale-ref
/// re-resolve-and-retry both sit OUTSIDE the per-ask timeouts, so a
/// cold lookup against a just-died-but-fresh-leased owner could
/// otherwise hold the webhook socket well past LiveKit's delivery
/// timeout. On elapse the owner may still complete the (idempotent)
/// re-assert; the LiveKit retry then confirms cheaply.
#[cfg(feature = "clustering")]
const WEBHOOK_RELAY_OVERALL_TIMEOUT: StdDuration = StdDuration::from_secs(4);
/// Bound on the claim-store read that precedes the relay hop — it
/// runs before [`WEBHOOK_RELAY_OVERALL_TIMEOUT`] starts, so it needs
/// its own deadline or a stalled control-plane pool holds the webhook
/// socket indefinitely while the dedupe entry swallows retries.
#[cfg(feature = "clustering")]
const WEBHOOK_CLAIM_READ_TIMEOUT: StdDuration = StdDuration::from_secs(2);

#[cfg(feature = "clustering")]
pub(super) async fn reassert_on_claim_owner(
    state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
) -> ReassertOutcome {
    use crate::clustering::relay::{RelayHandle, RelayReassertMediaGrantsReply};
    use crate::clustering::NodeId;
    use waddle_xmpp::ownership::{Entity, EntityType};

    let handles = &state.deps.app_state.clustering_claims;
    let (Some((claim_store, node_identity)), Some(stop_token)) =
        (handles.claim_pair(), handles.stop_token.clone())
    else {
        return converge_on_reconcile(room_jid, full_jid);
    };
    let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
    // Bounded like the relay hop below: this read precedes the relay
    // deadline, and a stalled control-plane pool must not hold the
    // webhook socket while the dedupe entry swallows LiveKit's
    // retries as duplicates.
    let claim = match tokio::time::timeout(
        WEBHOOK_CLAIM_READ_TIMEOUT,
        claim_store.current_claim(&entity),
    )
    .await
    {
        Ok(claim) => claim,
        Err(_elapsed) => {
            warn!(
                room = %room_jid,
                user = %full_jid.to_bare(),
                "room claim lookup timed out on participant join; \
                 asking LiveKit to retry",
            );
            return ReassertOutcome::RetryableFailure;
        }
    };
    if let Err(error) = &claim {
        warn!(
            room = %room_jid,
            user = %full_jid.to_bare(),
            error = %error,
            "room claim lookup failed on participant join; \
             asking LiveKit to retry",
        );
    }
    let owner = match route_from_claim(claim, &node_identity.current()) {
        ClaimRoute::AskOwner(owner) => owner,
        ClaimRoute::ConvergeOnReconcile => return converge_on_reconcile(room_jid, full_jid),
        ClaimRoute::RetryLater => return ReassertOutcome::RetryableFailure,
    };
    let mut relay = RelayHandle::new(NodeId::new(owner.node_id.clone()), stop_token.clone())
        .with_ask_timeouts(WEBHOOK_RELAY_MAILBOX_TIMEOUT, WEBHOOK_RELAY_REPLY_TIMEOUT);
    let asked = tokio::time::timeout(
        WEBHOOK_RELAY_OVERALL_TIMEOUT,
        relay.reassert_media_grants(room_jid.clone(), full_jid.clone()),
    )
    .await;
    let Ok(result) = asked else {
        warn!(
            room = %room_jid,
            user = %full_jid.to_bare(),
            owner = %owner.node_id,
            "cross-node media grant re-assert timed out resolving the owner; \
             asking LiveKit to retry",
        );
        return ReassertOutcome::RetryableFailure;
    };
    match result {
        Ok(RelayReassertMediaGrantsReply::Applied) => {
            debug!(
                room = %room_jid,
                user = %full_jid.to_bare(),
                owner = %owner.node_id,
                "media grants re-asserted on the room's claim owner",
            );
            ReassertOutcome::Handled
        }
        Ok(RelayReassertMediaGrantsReply::NotOccupantEvicted) => {
            // The owner's actor answered authoritatively and evicted;
            // its own warn line carries the details.
            ReassertOutcome::Handled
        }
        Ok(RelayReassertMediaGrantsReply::NotOwner) => {
            // The claim moved between our read and the ask (or the
            // owner's actor is mid-(re)spawn). A retry re-resolves the
            // claim and reaches the new owner.
            warn!(
                room = %room_jid,
                user = %full_jid.to_bare(),
                owner = %owner.node_id,
                "room claim owner had no local room actor; \
                 asking LiveKit to retry",
            );
            ReassertOutcome::RetryableFailure
        }
        Ok(RelayReassertMediaGrantsReply::Unavailable) => {
            warn!(
                room = %room_jid,
                user = %full_jid.to_bare(),
                owner = %owner.node_id,
                "room claim owner could not re-assert media grants; \
                 asking LiveKit to retry",
            );
            ReassertOutcome::RetryableFailure
        }
        Err(error) => {
            warn!(
                room = %room_jid,
                user = %full_jid.to_bare(),
                owner = %owner.node_id,
                error = %error,
                "cross-node media grant re-assert failed; \
                 asking LiveKit to retry",
            );
            ReassertOutcome::RetryableFailure
        }
    }
}

/// Without the `clustering` feature there is no relay to route
/// through; keep the pre-#1594 acknowledge-and-reconcile behavior.
#[cfg(not(feature = "clustering"))]
pub(super) async fn reassert_on_claim_owner(
    _state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
) -> ReassertOutcome {
    converge_on_reconcile(room_jid, full_jid)
}

#[cfg(test)]
#[cfg(feature = "clustering")]
mod tests {
    use super::*;
    use waddle_xmpp::ownership::NodeIdentity;

    /// Every branch of the pure claim-routing decision: only a
    /// FRESH claim held by ANOTHER node is worth a relay ask;
    /// unclaimed / stale-lease / self-owned all degrade to the
    /// reconciler, and only a claim-store read error burns a
    /// LiveKit retry.
    #[test]
    fn claim_routing_decides_owner_fallback_and_retry() {
        use waddle_xmpp::ownership::{ClaimEpoch, ClaimError, ClaimSnapshot};
        let me = NodeIdentity::new("node-self", "epoch-1");
        let other = NodeIdentity::new("node-other", "epoch-9");
        let fresh = |owner: &NodeIdentity| ClaimSnapshot {
            owner: owner.clone(),
            claim_epoch: ClaimEpoch(1),
            owner_lease_fresh: true,
        };

        assert_eq!(
            route_from_claim(Ok(None), &me),
            ClaimRoute::ConvergeOnReconcile,
            "unclaimed room has no owner to ask"
        );
        assert_eq!(
            route_from_claim(
                Ok(Some(ClaimSnapshot {
                    owner_lease_fresh: false,
                    ..fresh(&other)
                })),
                &me
            ),
            ClaimRoute::ConvergeOnReconcile,
            "a stale-leased owner must not be relayed to"
        );
        assert_eq!(
            route_from_claim(Ok(Some(fresh(&me))), &me),
            ClaimRoute::ConvergeOnReconcile,
            "a self-owned claim is a local race, not a relay target"
        );
        assert_eq!(
            route_from_claim(Ok(Some(fresh(&other))), &me),
            ClaimRoute::AskOwner(other.clone()),
            "a fresh foreign owner gets the relay ask"
        );
        assert_eq!(
            route_from_claim(Err(ClaimError::Poisoned), &me),
            ClaimRoute::RetryLater,
            "a transient store error is worth a LiveKit retry"
        );
    }
}
