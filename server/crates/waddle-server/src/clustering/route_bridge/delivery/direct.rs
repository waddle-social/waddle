//! Direct requests: delivery to a user whose account another node owns,
//! carried by the durable mailbox instead of an ordered relay envelope.
//!
//! The origin side replaces the relay send for a bare-JID or full-JID
//! recipient: it commits what the envelope would have carried as a direct
//! request, waits for the account owner's decision and reports it as the
//! outcome the relay's callers already handle. The target side is the
//! mailbox worker's handling of such a request: it runs the stored payload
//! through the code the relay receiver runs and records the decision.
//!
//! Custody is taken by the recipient's session path, which holds the
//! admission authority: a live connection admits a frame to durable stream
//! management on its own fence when it takes the frame from its endpoint,
//! and a detached session is appended to through the stream-management
//! session registry. The worker itself admits nothing.

use std::sync::Arc;
use std::time::Duration;

use waddle_xmpp::ingress::mailbox::{DirectRequestKey, MailboxLaneOrigin};
use waddle_xmpp::ownership::NodeIdentity;

use super::receiver::payload_target;
use super::{
    definite_no_effect_outcome, no_client_reply_outcome, no_client_reply_outcome_with_commit_state,
    FullJidDeliveryOutcome, OrderedRelayDeliveryBridge, OrderedRelayDeliveryServices,
    OrderedRelayNackReason, OrderedRelayOrigin, OrderedRelayPayload, OrderedRelayRecipient,
    RemoteDeliveryOutcome, RemoteDeliverySeed,
};
use crate::ingress::direct_request::{ClaimedDirectRequest, DirectCommit, DirectSubmission};
use crate::ingress::IngressAuthority;
use crate::ingress_uow::{DirectDecision, DirectRefusal, IngressUowError};

/// How long an origin waits for the account owner's decision before it
/// reports the delivery as uncertain. The account owner's poller scans
/// every second, so a decision that takes longer means the owner is not
/// working its backlog.
const DIRECT_DECISION_BUDGET: Duration = Duration::from_secs(10);

/// The append obligation a payload carries, if it is a copy an ingress
/// transaction owns.
fn append_obligation(
    payload: &OrderedRelayPayload,
) -> Option<&crate::ingress::identity::IngressAppendObligationRef> {
    match payload {
        OrderedRelayPayload::Message { ingress_append, .. }
        | OrderedRelayPayload::ProcessedDirectMessage { ingress_append, .. } => {
            ingress_append.as_ref()
        }
        OrderedRelayPayload::Iq { .. }
        | OrderedRelayPayload::Presence { .. }
        | OrderedRelayPayload::MucProxy { .. } => None,
    }
}

fn payload_stanza(payload: &OrderedRelayPayload) -> &waddle_xmpp::Stanza {
    match payload {
        OrderedRelayPayload::ProcessedDirectMessage { stanza, .. }
        | OrderedRelayPayload::Message { stanza, .. }
        | OrderedRelayPayload::Iq { stanza, .. }
        | OrderedRelayPayload::Presence { stanza, .. }
        | OrderedRelayPayload::MucProxy { stanza, .. } => &stanza.0,
    }
}

impl OrderedRelayDeliveryBridge {
    /// Carry `seed` to the recipient account's owner as a direct request.
    ///
    /// `None` when the mailbox does not carry it and the caller sends the
    /// relay envelope: the recipient is a room, the deployment does not
    /// select the mailbox with durable stream management, the protocol
    /// epoch is below the mailbox epoch, the account has no claim, or the
    /// request could not be committed. Once a request is committed the
    /// result is always `Some`: an accepted request is `Delivered`, a
    /// rejected one is `Unavailable` so that the caller runs the fallback it
    /// runs for a refused relay send, and a request whose decision was not
    /// read is reported as possibly committed, which suppresses that
    /// fallback.
    pub(in super::super) async fn deliver_seeded_direct(
        &self,
        seed: &RemoteDeliverySeed,
    ) -> Option<RemoteDeliveryOutcome> {
        if !matches!(
            seed.channel.recipient,
            OrderedRelayRecipient::BareJid(_) | OrderedRelayRecipient::FullJid(_)
        ) {
            return None;
        }
        let state = seed.services.web_socket_state.upgrade()?;
        state.deps.app_state.durable_sm().store()?;
        let authority = Arc::clone(&state.deps.protocol.ingress);
        if !authority.room_requests_carried_now().await {
            return None;
        }
        let obligation = append_obligation(&seed.payload);
        let key = match obligation {
            Some(obligation) => DirectRequestKey::for_copy(
                obligation.message_key,
                &obligation.receipt.semantic_identity_hash,
                &seed.target,
            ),
            None => DirectRequestKey::mint(),
        };
        let submission = DirectSubmission {
            key,
            recipient: seed.target.clone(),
            origin: match &seed.channel.origin {
                OrderedRelayOrigin::SmSession(session) => {
                    MailboxLaneOrigin::SmSession(session.clone())
                }
                OrderedRelayOrigin::Entity(entity) => MailboxLaneOrigin::Entity(entity.clone()),
            },
            origin_epoch: seed.channel.origin_epoch,
            sender_entity: seed.sender_claim.entity.clone(),
            payload: seed.payload.clone(),
            message_key: obligation.map(|obligation| obligation.message_key),
        };
        // The channel lock orders this origin's commits to one recipient,
        // so the lane's sequence is the order the stanzas were sent in. It
        // is released before the wait.
        let lock = self.lock_for_channel(&seed.channel).await?;
        let committed = {
            let _guard = lock.lock().await;
            authority.commit_direct_request(submission).await
        };
        self.remove_channel_lock_if_unused(&seed.channel, &lock)
            .await;
        let pending = match committed {
            Ok(DirectCommit::Committed(pending)) => pending,
            Ok(DirectCommit::NotCarried) => return None,
            Ok(DirectCommit::Refused(refusal)) => {
                tracing::warn!(%refusal, target = %seed.target, "direct request refused: mailbox is full");
                return Some(no_client_reply_outcome(definite_no_effect_outcome(
                    seed.is_iq,
                )));
            }
            // The commit may have landed: nothing else may carry the stanza.
            Err(IngressUowError::AmbiguousCommit) => {
                return Some(no_client_reply_outcome_with_commit_state(
                    FullJidDeliveryOutcome::MaybeCommitted,
                    true,
                ));
            }
            Err(error) => {
                tracing::warn!(%error, target = %seed.target, "direct request not committed; using the relay");
                return None;
            }
        };
        Some(
            match authority
                .await_direct_request(&pending, DIRECT_DECISION_BUDGET)
                .await
            {
                Ok(DirectDecision::Accepted) => {
                    no_client_reply_outcome(FullJidDeliveryOutcome::Delivered)
                }
                Ok(DirectDecision::Rejected(
                    DirectRefusal::NotDelivered | DirectRefusal::HostedElsewhere,
                )) => no_client_reply_outcome(FullJidDeliveryOutcome::Unavailable),
                Err(reason) => {
                    tracing::warn!(%reason, target = %seed.target, "direct request decision not read");
                    no_client_reply_outcome_with_commit_state(
                        FullJidDeliveryOutcome::MaybeCommitted,
                        true,
                    )
                }
            },
        )
    }
}

/// Whether anything on this node could take a stanza for `recipient`: a
/// registered connection or a detached stream-management session of the
/// full JID, or any of the two for a bare JID.
async fn recipient_present(services: &OrderedRelayDeliveryServices, recipient: &jid::Jid) -> bool {
    let account = recipient.to_bare();
    let detached = services
        .sm_session_registry
        .available_detached_resources_for_user(&account)
        .await
        .unwrap_or_default();
    match recipient.clone().try_into_full() {
        Ok(resource) => {
            services.connection_registry.entry(&resource).is_some() || detached.contains(&resource)
        }
        Err(_) => {
            !detached.is_empty()
                || !waddle_xmpp::registry::available_resources_for_user(
                    &services.user_registry,
                    &account,
                )
                .await
                .is_empty()
        }
    }
}

async fn record(
    authority: &IngressAuthority,
    claimed: &ClaimedDirectRequest,
    target: &NodeIdentity,
    decision: DirectDecision,
) -> Option<DirectDecision> {
    match authority
        .decide_direct_request(claimed, target, decision)
        .await
    {
        Ok(recorded) => Some(recorded),
        Err(error) => {
            tracing::warn!(%error, recipient = %claimed.request.recipient, "direct request decision not committed");
            None
        }
    }
}

/// Take custody of one direct request on this node, which owns the
/// recipient's account, and decide it. `true` when the decision committed.
///
/// A copy that an ingress transaction owns and that is addressed to a full
/// JID is handed over first and decided afterwards: its append records the
/// copy's ingress receipt, so a repeated attempt after a lost decision is
/// recognised and nothing is delivered twice. Every other stanza is decided
/// first and handed over afterwards, once: a failure after the decision
/// loses the stanza the way an uncertain relay attempt does and can never
/// deliver it twice. A recipient nothing on this node can take a stanza for
/// is rejected, and the origin runs its own fallback.
pub(in super::super) async fn decide_direct(
    services: &Arc<OrderedRelayDeliveryServices>,
    authority: &Arc<IngressAuthority>,
    claimed: &ClaimedDirectRequest,
    target: &NodeIdentity,
) -> bool {
    let request = &claimed.request;
    // A request that records a decision only needs its entry moved; its
    // stanza was handed over, or deliberately not, by the attempt that
    // recorded it.
    if let Some(recorded) = request.decision {
        return record(authority, claimed, target, recorded).await.is_some();
    }
    let Some(bridge) = services.web_socket_state.upgrade().and_then(|state| {
        state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .clone()
    }) else {
        return false;
    };
    let recipient = match request.recipient.clone().try_into_full() {
        Ok(resource) => OrderedRelayRecipient::FullJid(resource),
        Err(account) => OrderedRelayRecipient::BareJid(account),
    };
    let stanza = payload_stanza(&request.payload);
    #[cfg(feature = "test-support")]
    crate::clustering::acceptance_seams::hold_owner_request(
        crate::clustering::acceptance_seams::OwnerAcceptancePoint::BeforeCommit,
        stanza,
    )
    .await;
    let repeatable = append_obligation(&request.payload).is_some()
        && matches!(recipient, OrderedRelayRecipient::FullJid(_));
    let decided = if repeatable {
        let Ok(delivery) = payload_target(&request.payload, &recipient) else {
            return false;
        };
        match bridge
            .deliver_user_payload(services, &request.sender_entity, &request.payload, delivery)
            .await
        {
            Ok(()) => record(authority, claimed, target, DirectDecision::Accepted).await,
            Err(OrderedRelayNackReason::TargetUnavailable) => {
                record(
                    authority,
                    claimed,
                    target,
                    DirectDecision::Rejected(DirectRefusal::NotDelivered),
                )
                .await
            }
            Err(reason) => {
                tracing::warn!(?reason, recipient = %request.recipient, "direct request not handed over yet");
                None
            }
        }
    } else if !recipient_present(services, &request.recipient).await {
        record(
            authority,
            claimed,
            target,
            DirectDecision::Rejected(DirectRefusal::NotDelivered),
        )
        .await
    } else {
        let recorded = record(authority, claimed, target, DirectDecision::Accepted).await;
        if recorded == Some(DirectDecision::Accepted) {
            #[cfg(feature = "test-support")]
            crate::clustering::acceptance_seams::hold_owner_request(
                crate::clustering::acceptance_seams::OwnerAcceptancePoint::AfterCommit,
                stanza,
            )
            .await;
            match payload_target(&request.payload, &recipient) {
                Ok(delivery) => {
                    if let Err(reason) = bridge
                        .deliver_user_payload(
                            services,
                            &request.sender_entity,
                            &request.payload,
                            delivery,
                        )
                        .await
                    {
                        tracing::warn!(?reason, recipient = %request.recipient, "accepted direct request was not handed over");
                    }
                }
                Err(reason) => {
                    tracing::warn!(?reason, recipient = %request.recipient, "accepted direct request names another recipient");
                }
            }
        }
        recorded
    };
    decided.is_some()
}
