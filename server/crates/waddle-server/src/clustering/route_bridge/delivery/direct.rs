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
//!
//! A bound request (`deliver_bound`) is addressed to the node that hosts
//! the binding the account coordinator named for a full JID. That node
//! decides it, and hands the stanza only to the connection that carries
//! the named binding, or to the resource's detached session when no
//! connection holds the resource.

use std::sync::Arc;
use std::time::Duration;

use waddle_xmpp::ingress::mailbox::{DirectRequestKey, MailboxLaneOrigin};
use waddle_xmpp::ownership::{BindingRef, NodeIdentity};
use waddle_xmpp::registry::DeliveryKind;
use waddle_xmpp::Stanza;

use super::receiver::payload_target;
use super::{
    definite_no_effect_outcome, no_client_reply_outcome, no_client_reply_outcome_with_commit_state,
    FullJidDeliveryOutcome, OrderedRelayDeliveryBridge, OrderedRelayDeliveryServices,
    OrderedRelayNackReason, OrderedRelayOrigin, OrderedRelayPayload, OrderedRelayRecipient,
    RemoteDeliveryOutcome, RemoteDeliverySeed,
};
use super::{payload_for_recipient, user_entity, RemoteStanza};
use crate::ingress::direct_request::{
    BoundSubmission, ClaimedDirectRequest, DirectCommit, DirectSubmission,
};
use crate::ingress::identity::IngressAppendObligationRef;
use crate::ingress::IngressAuthority;
use crate::ingress_uow::{DirectDecision, DirectRefusal, DirectTarget, IngressUowError};
use crate::server::routes::interpret::SmIngressAppendContext;

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
    /// select the mailbox, the protocol epoch is below the mailbox epoch,
    /// the account has no claim, or the request could not be committed.
    /// Once a request is committed the
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
            Ok(DirectCommit::LaneRetargetPending) => {
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

/// One stanza for a bound resource, as [`OrderedRelayDeliveryBridge::deliver_bound`]
/// carries it.
pub(crate) struct BoundDelivery<'a> {
    pub(crate) target: &'a jid::FullJid,
    /// The binding the account coordinator named for `target`.
    pub(crate) binding: BindingRef,
    /// The node that hosts `binding`.
    pub(crate) node: NodeIdentity,
    pub(crate) stanza: &'a Stanza,
    pub(crate) kind: DeliveryKind,
    pub(crate) ingress_append_context: Option<&'a SmIngressAppendContext>,
    pub(crate) received_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl OrderedRelayDeliveryBridge {
    /// Carry a stanza for `target` to the node that hosts `binding`, the
    /// binding the account coordinator named for it, as a bound request.
    ///
    /// The hosting node hands the stanza to that exact binding. The outcome
    /// maps the decision as [`Self::deliver_seeded_direct`] does: accepted
    /// is `Delivered`, rejected is `Unavailable` so that the caller runs its
    /// fallback, and a committed request whose decision was not read is
    /// `MaybeCommitted`, which suppresses the fallback. A request that was
    /// not committed is a definite no-effect outcome. When the deployment
    /// does not carry direct requests nothing reaches the hosting node and
    /// the outcome is `Unavailable`.
    pub(crate) async fn deliver_bound(
        &self,
        delivery: BoundDelivery<'_>,
    ) -> FullJidDeliveryOutcome {
        let BoundDelivery {
            target,
            binding,
            node,
            stanza,
            kind,
            ingress_append_context,
            received_at,
        } = delivery;
        let Some(state) = self
            .services
            .get()
            .and_then(|services| services.web_socket_state.upgrade())
        else {
            return FullJidDeliveryOutcome::Unavailable;
        };
        let authority = Arc::clone(&state.deps.protocol.ingress);
        if !authority.room_requests_carried_now().await {
            tracing::debug!(%target, "bound request not carried: the mailbox is not selected");
            return FullJidDeliveryOutcome::Unavailable;
        }
        let recipient = jid::Jid::from(target.clone());
        let Some(payload) = bound_payload(
            &recipient,
            stanza,
            kind,
            ingress_append_context,
            received_at,
        ) else {
            return FullJidDeliveryOutcome::Unavailable;
        };
        let obligation = append_obligation(&payload).cloned();
        let sender = obligation
            .as_ref()
            .map(|obligation| obligation.sender_bare.clone())
            .or_else(|| stanza_sender(stanza))
            .unwrap_or_else(|| target.to_bare());
        let key = match &obligation {
            Some(obligation) => DirectRequestKey::for_copy(
                obligation.message_key,
                &obligation.receipt.semantic_identity_hash,
                &recipient,
            ),
            None => DirectRequestKey::mint(),
        };
        let is_iq = matches!(stanza, Stanza::Iq(_));
        let committed = authority
            .commit_bound_request(BoundSubmission {
                key,
                recipient: target.clone(),
                node,
                binding,
                sender_entity: user_entity(&sender),
                payload,
                message_key: obligation.map(|obligation| obligation.message_key),
            })
            .await;
        let pending = match committed {
            Ok(DirectCommit::Committed(pending)) => pending,
            Ok(DirectCommit::NotCarried) => return FullJidDeliveryOutcome::Unavailable,
            Ok(DirectCommit::Refused(refusal)) => {
                tracing::warn!(%refusal, %target, "bound request refused: mailbox is full");
                return definite_no_effect_outcome(is_iq);
            }
            Ok(DirectCommit::LaneRetargetPending) => {
                tracing::debug!(%target, "bound request deferred: the lane owes another node decisions");
                return definite_no_effect_outcome(is_iq);
            }
            Err(IngressUowError::AmbiguousCommit) => {
                return FullJidDeliveryOutcome::MaybeCommitted;
            }
            Err(error) => {
                tracing::warn!(%error, %target, "bound request not committed");
                return definite_no_effect_outcome(is_iq);
            }
        };
        match authority
            .await_direct_request(&pending, DIRECT_DECISION_BUDGET)
            .await
        {
            Ok(DirectDecision::Accepted) => FullJidDeliveryOutcome::Delivered,
            Ok(DirectDecision::Rejected(_)) => FullJidDeliveryOutcome::Unavailable,
            Err(reason) => {
                tracing::warn!(%reason, %target, "bound request decision not read");
                FullJidDeliveryOutcome::MaybeCommitted
            }
        }
    }
}

/// The relay payload a bound request carries: a processed copy for a frame
/// that skips the recipient pass, the peer payload otherwise. A copy an
/// ingress transaction owns carries its append obligation when the context
/// permits a remote one, exactly as a relay envelope would.
fn bound_payload(
    recipient: &jid::Jid,
    stanza: &Stanza,
    kind: DeliveryKind,
    context: Option<&SmIngressAppendContext>,
    received_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<OrderedRelayPayload> {
    if kind == DeliveryKind::DirectFrame {
        return Some(OrderedRelayPayload::ProcessedDirectMessage {
            recipient: recipient.clone(),
            stanza: RemoteStanza(stanza.clone()),
            ingress_append: IngressAppendObligationRef::for_remote_message(context, stanza),
        });
    }
    let mut payload = payload_for_recipient(recipient.clone(), stanza)?;
    if let OrderedRelayPayload::Message {
        ingress_append,
        received_at: timestamp,
        ..
    } = &mut payload
    {
        let remote = context.filter(|context| context.authority.permits_remote_obligation());
        *ingress_append = match (stanza, remote) {
            (Stanza::Message(message), Some(context)) => message
                .from
                .as_ref()
                .map(|sender| IngressAppendObligationRef::from_context(context, sender.to_bare()))
                .filter(|obligation| obligation.kind_is_append_eligible()),
            _ => None,
        };
        *timestamp = if remote.is_some() {
            None
        } else {
            received_at.or_else(|| context.and_then(|context| context.received_at))
        };
    }
    Some(payload)
}

fn stanza_sender(stanza: &Stanza) -> Option<jid::BareJid> {
    let from = match stanza {
        Stanza::Message(message) => message.from.as_ref(),
        Stanza::Presence(presence) => presence.from.as_ref(),
        Stanza::Iq(iq) => iq.from(),
    };
    from.map(jid::Jid::to_bare)
}

/// Whether anything on this node could take a stanza for `recipient`: a
/// registered connection or a detached stream-management session of the
/// full JID, or any of the two for a bare JID.
///
/// A live resource is one the account coordinator lists with an active
/// binding, wherever that binding is hosted: delivery reaches it through a
/// bound request. Without the route state no coordinator is reachable and
/// only detached sessions count.
async fn recipient_present(services: &OrderedRelayDeliveryServices, recipient: &jid::Jid) -> bool {
    let account = recipient.to_bare();
    let detached = services
        .sm_session_registry
        .available_detached_resources_for_user(&account)
        .await
        .unwrap_or_default();
    let full = recipient.clone().try_into_full().ok();
    let detached_present = match &full {
        Some(resource) => detached.contains(resource),
        None => !detached.is_empty(),
    };
    let Some(state) = services.web_socket_state.upgrade() else {
        return detached_present;
    };
    let selection = state.deps.app_state.account_authority();
    let live = match &full {
        Some(resource) => selection
            .bound_resource(resource)
            .await
            .is_ok_and(|binding| binding.is_some()),
        None => selection
            .resources(&account)
            .await
            .is_ok_and(|resources| !resources.is_empty()),
    };
    live || detached_present
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
    if let Some(bound) = &request.target {
        return decide_bound(services, authority, claimed, bound, target).await;
    }
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

async fn record_bound(
    authority: &IngressAuthority,
    claimed: &ClaimedDirectRequest,
    node: &NodeIdentity,
    decision: DirectDecision,
) -> Option<DirectDecision> {
    match authority
        .decide_bound_request(claimed, node, decision)
        .await
    {
        Ok(recorded) => Some(recorded),
        Err(error) => {
            tracing::warn!(%error, recipient = %claimed.request.recipient, "bound request decision not committed");
            None
        }
    }
}

/// Take custody of one bound request on `node` and decide it. `true` when
/// the decision committed.
///
/// A node other than the request's target, which works the lane because
/// it adopted it as the account's new owner, refuses the request as hosted
/// elsewhere. The target hands the stanza only to the connection that
/// carries the request's binding, or to the resource's detached session
/// when no connection holds the resource; a connection that carries any
/// other binding refuses it. The order of hand-over and decision follows
/// the rule for direct requests: a copy an ingress transaction owns is
/// handed over first, anything else is decided first and handed over once.
async fn decide_bound(
    services: &Arc<OrderedRelayDeliveryServices>,
    authority: &Arc<IngressAuthority>,
    claimed: &ClaimedDirectRequest,
    bound: &DirectTarget,
    node: &NodeIdentity,
) -> bool {
    let request = &claimed.request;
    if let Some(recorded) = request.decision {
        return record_bound(authority, claimed, node, recorded)
            .await
            .is_some();
    }
    if bound.node != *node {
        return record_bound(
            authority,
            claimed,
            node,
            DirectDecision::Rejected(DirectRefusal::HostedElsewhere),
        )
        .await
        .is_some();
    }
    let not_delivered = DirectDecision::Rejected(DirectRefusal::NotDelivered);
    let Ok(resource) = request.recipient.clone().try_into_full() else {
        return record_bound(authority, claimed, node, not_delivered)
            .await
            .is_some();
    };
    let Some(state) = services.web_socket_state.upgrade() else {
        return false;
    };
    let stanza = payload_stanza(&request.payload);
    let obligation = append_obligation(&request.payload);
    let context = super::ingress_append::authorize_ingress_append(
        services,
        &request.sender_entity,
        stanza,
        obligation,
    );
    if obligation.is_some() && context.is_none() {
        return record_bound(authority, claimed, node, not_delivered)
            .await
            .is_some();
    }
    let mut deps =
        crate::server::routes::websocket::interpret_loop::build_interpret_deps(&state, None);
    deps.connection_registry = &services.connection_registry;
    deps.user_registry = Some(&services.user_registry);
    deps.sm_session_registry = Some(&services.sm_session_registry);
    deps.ingress_append_context = context;
    let kind = match request.payload {
        OrderedRelayPayload::ProcessedDirectMessage { .. } => DeliveryKind::DirectFrame,
        _ => DeliveryKind::PeerStanza,
    };
    let received_at = match request.payload {
        OrderedRelayPayload::Message { received_at, .. } => received_at,
        _ => None,
    };
    let live_only = matches!(stanza, Stanza::Iq(_));
    let hand_over = || {
        crate::server::routes::interpret::deliver_to_local_binding(
            &deps,
            &resource,
            &bound.binding,
            stanza,
            kind,
            received_at,
            live_only,
        )
    };
    if obligation.is_some() {
        let decision = match hand_over().await {
            FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached => {
                DirectDecision::Accepted
            }
            FullJidDeliveryOutcome::Unavailable => not_delivered,
            outcome
            @ (FullJidDeliveryOutcome::Dropped | FullJidDeliveryOutcome::MaybeCommitted) => {
                tracing::warn!(?outcome, recipient = %request.recipient, "bound request not handed over yet");
                return false;
            }
        };
        return record_bound(authority, claimed, node, decision)
            .await
            .is_some();
    }
    let present = services
        .connection_registry
        .hosts_binding(&resource, &bound.binding)
        || (!live_only
            && services.connection_registry.entry(&resource).is_none()
            && services
                .sm_session_registry
                .available_detached_resources_for_user(&resource.to_bare())
                .await
                .unwrap_or_default()
                .contains(&resource));
    if !present {
        return record_bound(authority, claimed, node, not_delivered)
            .await
            .is_some();
    }
    let recorded = record_bound(authority, claimed, node, DirectDecision::Accepted).await;
    if recorded == Some(DirectDecision::Accepted) {
        let outcome = hand_over().await;
        if !matches!(
            outcome,
            FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached
        ) {
            tracing::warn!(?outcome, recipient = %request.recipient, "accepted bound request was not handed over");
        }
    }
    recorded.is_some()
}
