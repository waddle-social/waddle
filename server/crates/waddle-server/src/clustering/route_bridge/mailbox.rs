//! This node's durable-mailbox poller and its hint transport.
//!
//! The poller is the authority for finding work: it scans the lanes that owe
//! this node a decision, on an interval and whenever a peer's hint arrives. A
//! hint only shortens the wait; a lost one costs at most one interval. Each
//! pass examines a bounded number of lanes and works a bounded number at a
//! time, so a backlog cannot occupy more than that many ingress connections.
//!
//! Working a lane means running the owner's ordinary relayed-groupchat commit
//! for its head entry, rebuilt from canonical rows. That commit asserts the
//! room claim and records the decision; the poller itself decides nothing.

use std::sync::Arc;
use std::time::Duration;

use waddle_xmpp::ownership::NodeIdentity;
use waddle_xmpp::telemetry::attributes::MailboxDeliveryOutcome;
use waddle_xmpp::Stanza;

use super::delivery::deliver_reserved_muc_proxy;
use super::{
    MucProxyOrigin, NodeId, OrderedRelayDeliveryBridge, OrderedRelayDeliveryServices,
    OrderedRelayMucProxyKind, RelayHandle,
};
use crate::ingress::mailbox_worker::{
    ClaimedRoomAcceptance, MailboxAttemptRecord, MailboxLaneHead, MAILBOX_CLAIM_LIMIT,
    MAILBOX_UNCONSUMED_GRACE,
};
use crate::ingress::origin_gate::MailboxHintSink;
use crate::ingress_uow::MailboxLaneId;

/// How long the poller waits for a hint before it scans anyway.
const MAILBOX_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Poll passes between two adoption passes. A room whose claim moved is
/// served by its new owner within this many poll intervals.
const MAILBOX_ADOPTION_PASSES: u32 = 5;
/// Poll passes between two sweeps of unconsumed decisions.
const MAILBOX_SWEEP_PASSES: u32 = 30;
/// Budget of one hint. A hint that does not arrive is not retried.
const MAILBOX_HINT_TIMEOUT: Duration = Duration::from_secs(2);

impl OrderedRelayDeliveryBridge {
    /// Wake this node's mailbox poller. Called when a peer's hint arrives.
    pub(in crate::clustering) fn wake_mailbox_poller(&self) {
        self.mailbox_wake.notify_one();
    }
}

impl MailboxHintSink for OrderedRelayDeliveryBridge {
    /// Tell `target` that an entry was appended for it. Sent in the
    /// background and never awaited by the appender: the entry is already
    /// committed, and the target's poller finds it without the hint.
    fn hint(&self, target: NodeIdentity) {
        let stop_token = self.stop_token.clone();
        tokio::spawn(async move {
            let mut relay = RelayHandle::new(NodeId::new(target.node_id), stop_token)
                .with_ask_timeouts(MAILBOX_HINT_TIMEOUT, MAILBOX_HINT_TIMEOUT);
            let delivered = tokio::time::timeout(MAILBOX_HINT_TIMEOUT, relay.mailbox_hint())
                .await
                .is_ok_and(|result| result.is_ok());
            waddle_xmpp::telemetry::reliability::increment_mailbox_hint(delivered);
        });
    }
}

/// Start this node's mailbox poller. It runs until the bridge's stop token
/// fires and does nothing while remote room acceptance uses the relay.
pub fn spawn_mailbox_poller(bridge: Arc<OrderedRelayDeliveryBridge>) {
    tokio::spawn(async move {
        // Where the next pass resumes. A full page moves it forward, so
        // lanes whose heads are deferred or dead-lettered cannot keep the
        // poller from the lanes behind them; a short page starts over.
        let mut resume_after = None;
        let mut adoption_after = None;
        let mut sweep_after = None;
        let mut pass: u32 = 0;
        loop {
            tokio::select! {
                biased;
                _ = bridge.stop_token.cancelled() => return,
                _ = bridge.mailbox_wake.notified() => {}
                _ = tokio::time::sleep(MAILBOX_POLL_INTERVAL) => {}
            }
            let Some(services) = bridge.services.get().cloned() else {
                continue;
            };
            pass = pass.wrapping_add(1);
            if pass.is_multiple_of(MAILBOX_ADOPTION_PASSES) {
                adoption_after = adopt_once(&services, adoption_after).await;
            }
            resume_after = poll_once(&services, resume_after).await;
            if pass.is_multiple_of(MAILBOX_SWEEP_PASSES) {
                sweep_after = sweep_once(&services, sweep_after).await;
            }
        }
    });
}

/// The ingress authority, while this node carries remote room acceptance
/// through the mailbox.
fn mailbox_authority(
    services: &Arc<OrderedRelayDeliveryServices>,
) -> Option<Arc<crate::ingress::IngressAuthority>> {
    let state = services.web_socket_state.upgrade()?;
    let authority = Arc::clone(&state.deps.protocol.ingress);
    authority.mailbox_acceptance_enabled().then_some(authority)
}

/// Adopt lanes of rooms whose claim moved to this node, so the next backlog
/// scan finds them. Returns where the next adoption pass resumes.
async fn adopt_once(
    services: &Arc<OrderedRelayDeliveryServices>,
    resume_after: Option<MailboxLaneId>,
) -> Option<MailboxLaneId> {
    let authority = mailbox_authority(services)?;
    let target = services.node_identity.current();
    match authority
        .adopt_transferred_lanes(&target, resume_after)
        .await
    {
        Ok(adoption) => {
            if adoption.adopted > 0 {
                tracing::info!(
                    adopted = adoption.adopted,
                    "mailbox poller adopted lanes of rooms this node now owns"
                );
            }
            adoption.resume_after
        }
        Err(error) => {
            tracing::warn!(%error, "mailbox poller could not adopt transferred lanes");
            None
        }
    }
}

/// Consume decided entries no origin read. Returns where the next sweep
/// resumes.
async fn sweep_once(
    services: &Arc<OrderedRelayDeliveryServices>,
    resume_after: Option<waddle_xmpp::ingress::MessageKey>,
) -> Option<waddle_xmpp::ingress::MessageKey> {
    let authority = mailbox_authority(services)?;
    match authority
        .sweep_unconsumed_decisions(MAILBOX_UNCONSUMED_GRACE, resume_after)
        .await
    {
        Ok(sweep) => {
            if sweep.consumed > 0 || sweep.unproven > 0 {
                tracing::info!(
                    consumed = sweep.consumed,
                    unproven = sweep.unproven,
                    "mailbox poller swept decided entries no origin consumed"
                );
            }
            sweep.resume_after
        }
        Err(error) => {
            tracing::warn!(%error, "mailbox poller could not sweep unconsumed decisions");
            None
        }
    }
}

/// One pass: scan this node's backlog after `resume_after` and work its
/// lanes, at most [`MAILBOX_CLAIM_LIMIT`] at a time. Returns where the next
/// pass resumes.
async fn poll_once(
    services: &Arc<OrderedRelayDeliveryServices>,
    resume_after: Option<MailboxLaneId>,
) -> Option<MailboxLaneId> {
    let state = services.web_socket_state.upgrade()?;
    let authority = Arc::clone(&state.deps.protocol.ingress);
    if !authority.mailbox_acceptance_enabled() {
        return None;
    }
    let target = services.node_identity.current();
    let lanes = match authority.scan_mailbox_backlog(&target, resume_after).await {
        Ok(lanes) => lanes,
        Err(error) => {
            tracing::warn!(%error, "mailbox poller could not scan its backlog");
            return None;
        }
    };
    for claimed in lanes.chunks(MAILBOX_CLAIM_LIMIT) {
        futures::future::join_all(
            claimed
                .iter()
                .map(|lane| work_lane(services, &authority, *lane, &target)),
        )
        .await;
    }
    let full_page = lanes.len() >= crate::ingress::mailbox_worker::MAILBOX_SCAN_LIMIT as usize;
    lanes.last().copied().filter(|_| full_page)
}

/// Work the head entry of one lane and record how the attempt ended.
async fn work_lane(
    services: &Arc<OrderedRelayDeliveryServices>,
    authority: &Arc<crate::ingress::IngressAuthority>,
    lane: MailboxLaneId,
    target: &NodeIdentity,
) {
    let outcome = match authority.claim_room_acceptance(lane, target).await {
        Ok(MailboxLaneHead::Idle) => return,
        Ok(MailboxLaneHead::DeadLettered) => MailboxDeliveryOutcome::DeadLettered,
        Ok(MailboxLaneHead::Ready(claimed)) => {
            if accept(services, &claimed).await {
                MailboxDeliveryOutcome::Decided
            } else {
                match authority
                    .record_room_acceptance_failure(claimed.lane, claimed.operation, target)
                    .await
                {
                    Ok(MailboxAttemptRecord::DeadLettered) => MailboxDeliveryOutcome::DeadLettered,
                    Ok(MailboxAttemptRecord::Settled) => MailboxDeliveryOutcome::Decided,
                    Ok(MailboxAttemptRecord::Deferred { .. }) | Err(_) => {
                        MailboxDeliveryOutcome::Retried
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "mailbox poller could not read a lane head");
            MailboxDeliveryOutcome::Retried
        }
    };
    waddle_xmpp::telemetry::reliability::increment_mailbox_delivery(outcome);
}

/// Run the owner's relayed-groupchat commit for one entry. `true` when the
/// owner committed a decision.
///
/// Frames the owner produces for the sender are not written here: their
/// receipts stay pending, so ingress recovery delivers them through the
/// sender's keyed route.
async fn accept(
    services: &Arc<OrderedRelayDeliveryServices>,
    claimed: &ClaimedRoomAcceptance,
) -> bool {
    let mut completion = None;
    deliver_reserved_muc_proxy(
        services,
        &claimed.room,
        OrderedRelayMucProxyKind::GroupchatMessage,
        MucProxyOrigin::Server,
        &Stanza::Message(claimed.message.clone()),
        Some(&claimed.admission),
        &mut completion,
    )
    .await
    .is_ok()
}
