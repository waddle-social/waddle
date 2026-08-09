use super::delivery::receiver::current_claim;
use super::*;

pub(super) async fn validate_claims(
    services: &OrderedRelayDeliveryServices,
    envelope: &RemoteStanzaEnvelope,
) -> Result<(), OrderedRelayNackReason> {
    let origin = services
        .claim_store
        .current_claim(&envelope.origin_claim.entity)
        .await
        .map_err(|error| {
            tracing::warn!(
                entity = %envelope.origin_claim.entity,
                %error,
                "ordered relay: origin claim lookup failed"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        })?;
    if !origin.owner_lease_fresh
        || origin.claim_epoch != envelope.origin_claim.epoch
        || origin.owner.node_id != envelope.asserted_origin_node.as_str()
    {
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    validate_origin_proof(services, envelope, &origin.owner).await?;

    let sender = services
        .claim_store
        .current_claim(&envelope.sender_claim.entity)
        .await
        .map_err(|error| {
            tracing::warn!(
                entity = %envelope.sender_claim.entity,
                %error,
                "ordered relay: sender claim lookup failed"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Sender,
        })?;
    if !sender.owner_lease_fresh
        || sender.claim_epoch != envelope.sender_claim.epoch
        || sender.owner != origin.owner
    {
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Sender,
        });
    }

    let target = services
        .claim_store
        .current_claim(&envelope.target_claim.entity)
        .await
        .map_err(|error| {
            tracing::warn!(
                entity = %envelope.target_claim.entity,
                %error,
                "ordered relay: target claim lookup failed"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        })?;
    let me = services.node_identity.current();
    if !target.owner_lease_fresh
        || target.claim_epoch != envelope.target_claim.epoch
        || target.owner != me
    {
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        });
    }
    Ok(())
}

pub(super) async fn validate_origin_proof(
    services: &OrderedRelayDeliveryServices,
    envelope: &RemoteStanzaEnvelope,
    origin_owner: &waddle_xmpp::ownership::NodeIdentity,
) -> Result<(), OrderedRelayNackReason> {
    let Some(proof) = &envelope.origin_proof else {
        tracing::warn!(
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: unsigned origin envelope rejected"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    };
    let public_key = PublicKey::try_decode_protobuf(&proof.public_key).map_err(|error| {
        tracing::warn!(
            %error,
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: origin proof public key did not decode"
        );
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        }
    })?;
    let signing_bytes = envelope.signing_bytes().map_err(|error| {
        tracing::warn!(
            %error,
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: failed to serialize origin verification bytes"
        );
        OrderedRelayNackReason::ParseFailure
    })?;
    if !public_key.verify(&signing_bytes, &proof.signature) {
        tracing::warn!(
            asserted_origin_node = %envelope.asserted_origin_node,
            "ordered relay: origin proof signature verification failed"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    let signed_peer = public_key.to_peer_id();
    let signed_peer_id = signed_peer.to_string();
    let registered_peer_id = services
        .node_lease
        .peer_id_for_node(origin_owner)
        .await
        .map_err(|error| {
            tracing::warn!(
                %error,
                node_id = %origin_owner.node_id,
                "ordered relay: failed to load origin node PeerId binding"
            );
            OrderedRelayNackReason::Unreachable
        })?
        .ok_or_else(|| {
            tracing::warn!(
                node_id = %origin_owner.node_id,
                "ordered relay: origin node has no PeerId binding"
            );
            OrderedRelayNackReason::NotOwner {
                role: OrderedRelayClaimRole::Origin,
            }
        })?;
    if registered_peer_id != signed_peer_id {
        tracing::warn!(
            node_id = %origin_owner.node_id,
            registered_peer_id = %registered_peer_id,
            signed_peer_id = %signed_peer_id,
            "ordered relay: origin proof PeerId does not match node lease binding"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    let enrolled = services
        .allowlist_store
        .enrolled_peers()
        .await
        .map_err(|error| {
            tracing::warn!(
                %error,
                node_id = %origin_owner.node_id,
                signed_peer_id = %signed_peer_id,
                "ordered relay: failed to revalidate origin PeerId allowlist enrollment"
            );
            OrderedRelayNackReason::Unreachable
        })?;
    if !enrolled.contains(&signed_peer) {
        tracing::warn!(
            node_id = %origin_owner.node_id,
            signed_peer_id = %signed_peer_id,
            "ordered relay: origin PeerId is not enrolled in current allowlist"
        );
        return Err(OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        });
    }
    Ok(())
}

pub(super) async fn outcome_for_nack(
    services: &OrderedRelayDeliveryServices,
    target_entity: &Entity,
    previous_owner: &waddle_xmpp::ownership::NodeIdentity,
    nack: &OrderedRelayNack,
    is_iq: bool,
) -> (Option<FullJidDeliveryOutcome>, NackChannelAction, bool) {
    match nack.reason {
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Origin,
        }
        | OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Sender,
        } => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner),
            false,
        ),
        OrderedRelayNackReason::NotOwner {
            role: OrderedRelayClaimRole::Target,
        } => {
            let Some(snapshot) = current_claim(services, target_entity).await else {
                return (
                    Some(FullJidDeliveryOutcome::Unavailable),
                    NackChannelAction::Divert(OrderedRelayDiversionReason::Unreachable),
                    false,
                );
            };
            if !snapshot.owner_lease_fresh {
                return (
                    Some(FullJidDeliveryOutcome::Unavailable),
                    NackChannelAction::Divert(OrderedRelayDiversionReason::Unreachable),
                    false,
                );
            }
            let me = services.node_identity.current();
            if snapshot.owner == me {
                return (None, NackChannelAction::Forget, false);
            }
            if snapshot.owner != *previous_owner {
                tracing::debug!(
                    entity_id = %target_entity.id,
                    previous_owner = %previous_owner.node_id,
                    refreshed_owner = %snapshot.owner.node_id,
                    "ordered relay: target-owner changed after retry window; suppressing client fallback"
                );
                return (
                    Some(definite_no_effect_outcome(is_iq)),
                    NackChannelAction::Forget,
                    false,
                );
            }
            (
                Some(definite_no_effect_outcome(is_iq)),
                NackChannelAction::Divert(OrderedRelayDiversionReason::NotOwner),
                false,
            )
        }
        OrderedRelayNackReason::TargetUnavailable => (
            Some(FullJidDeliveryOutcome::Unavailable),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
            false,
        ),
        OrderedRelayNackReason::InFlight => (
            Some(FullJidDeliveryOutcome::Dropped),
            NackChannelAction::Keep,
            true,
        ),
        OrderedRelayNackReason::MaybeCommitted => (
            Some(FullJidDeliveryOutcome::Dropped),
            NackChannelAction::Divert(OrderedRelayDiversionReason::MaybeCommitted),
            true,
        ),
        OrderedRelayNackReason::Diverted(ref diversion)
            if diversion.reason == OrderedRelayDiversionReason::MaybeCommitted =>
        {
            (
                Some(FullJidDeliveryOutcome::Dropped),
                NackChannelAction::Divert(OrderedRelayDiversionReason::MaybeCommitted),
                true,
            )
        }
        OrderedRelayNackReason::Diverted(_) => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
            false,
        ),
        OrderedRelayNackReason::Unreachable => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
            false,
        ),
        OrderedRelayNackReason::Gap { .. }
        | OrderedRelayNackReason::ParseFailure
        | OrderedRelayNackReason::Backpressure => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Divert(diversion_reason_for_nack(nack)),
            false,
        ),
        // #1597: sender-synthesized when the peer does not know the
        // versioned ordered-relay message id. Provably uncommitted, so
        // this one operation fails but the channel must not be
        // poisoned: roll the sequence back and keep the channel.
        OrderedRelayNackReason::UnsupportedEnvelope => (
            Some(definite_no_effect_outcome(is_iq)),
            NackChannelAction::Rollback,
            false,
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NackChannelAction {
    Divert(OrderedRelayDiversionReason),
    Forget,
    Keep,
    /// #1597: the envelope provably never reached the peer's handler
    /// (versioned message id unknown there). Un-consume its sequence
    /// and keep the channel — the opposite of a sticky diversion.
    Rollback,
}

pub(super) fn definite_no_effect_outcome(is_iq: bool) -> FullJidDeliveryOutcome {
    if is_iq {
        FullJidDeliveryOutcome::Unavailable
    } else {
        FullJidDeliveryOutcome::Dropped
    }
}

pub(super) fn replies_for_origin_handoff(
    stanza: &Stanza,
    outcome: FullJidDeliveryOutcome,
    sfu: Option<&dyn waddle_sfu::SfuService>,
) -> Vec<Stanza> {
    match outcome {
        FullJidDeliveryOutcome::Unavailable => {
            crate::server::routes::interpret::bounce_undeliverable_iq(stanza, sfu)
                .into_iter()
                .collect()
        }
        FullJidDeliveryOutcome::Delivered
        | FullJidDeliveryOutcome::QueuedDetached
        | FullJidDeliveryOutcome::Dropped
        | FullJidDeliveryOutcome::MaybeCommitted => Vec::new(),
    }
}

pub(super) fn diversion_reason_for_nack(nack: &OrderedRelayNack) -> OrderedRelayDiversionReason {
    match &nack.reason {
        OrderedRelayNackReason::Gap { .. }
        | OrderedRelayNackReason::ParseFailure
        // #1597: never diverted in practice (outcome_for_nack maps it
        // to Rollback); parse-shaped if it ever is.
        | OrderedRelayNackReason::UnsupportedEnvelope
        | OrderedRelayNackReason::Diverted(_) => OrderedRelayDiversionReason::OrderingGap,
        OrderedRelayNackReason::NotOwner { .. } => OrderedRelayDiversionReason::NotOwner,
        OrderedRelayNackReason::Unreachable | OrderedRelayNackReason::TargetUnavailable => {
            OrderedRelayDiversionReason::Unreachable
        }
        OrderedRelayNackReason::InFlight | OrderedRelayNackReason::Backpressure => {
            OrderedRelayDiversionReason::Backpressure
        }
        OrderedRelayNackReason::MaybeCommitted => OrderedRelayDiversionReason::MaybeCommitted,
    }
}

pub(super) fn channel_diversion_for_ask_error(
    error: &RelayAskError,
) -> Option<OrderedRelayDiversionReason> {
    match error {
        RelayAskError::NotFound { .. } => None,
        RelayAskError::Send {
            failure: RelaySendFailure::MailboxFull,
            ..
        } => Some(OrderedRelayDiversionReason::Backpressure),
        RelayAskError::Send { .. } | RelayAskError::Cancelled => {
            Some(OrderedRelayDiversionReason::Unreachable)
        }
    }
}

pub(super) fn ask_error_allows_target_refresh(error: &RelayAskError) -> bool {
    match error {
        RelayAskError::NotFound { .. } => true,
        RelayAskError::Send {
            effect: RelaySendEffect::NoEffect,
            ..
        } => true,
        RelayAskError::Cancelled
        | RelayAskError::Send {
            effect: RelaySendEffect::MaybeCommitted,
            ..
        } => false,
    }
}

/// Provably-uncommitted proof that the remote reference cannot act on this
/// ask: a definitive no-effect stale-ref reply, or a lookup that resolved
/// nothing at all. Callers whose safety only needs "nothing committed AND a
/// successor supersedes the reference" (replacement retirement, forwarder
/// mirror cleanup) may act on this immediately.
pub(super) fn ask_error_proves_remote_resource_ref_stale(error: &RelayAskError) -> bool {
    matches!(
        error,
        RelayAskError::NotFound { .. }
            | RelayAskError::Send {
                failure: RelaySendFailure::StaleRef,
                effect: RelaySendEffect::NoEffect,
                ..
            }
    )
}

/// Only a definitive no-effect stale-ref reply DEFINITIVELY proves the remote
/// reference is gone. `RelayAskError::NotFound` is NOT definitive:
/// `relay.rs` maps repeated `Ok(None)` lookups and transient lookup errors to
/// it within a finite backoff budget, so a partition or slow Kademlia round
/// produces the same error while the owner process is still alive and still
/// holding mirrors. Callers that would drop a durable cleanup obligation must
/// use this predicate (plus their own persistence policy for lookup misses).
pub(super) fn ask_error_definitively_proves_remote_resource_ref_stale(
    error: &RelayAskError,
) -> bool {
    matches!(
        error,
        RelayAskError::Send {
            failure: RelaySendFailure::StaleRef,
            effect: RelaySendEffect::NoEffect,
            ..
        }
    )
}

/// A lookup that cannot resolve the owner at all. One occurrence is
/// ambiguous (see above); a long consecutive streak across the retry
/// backoff schedule is the practical signal that the owner incarnation is
/// gone for good (node ids are freshly generated per process start, so a
/// restarted owner can never satisfy the old reference).
pub(super) fn ask_error_is_owner_lookup_miss(error: &RelayAskError) -> bool {
    matches!(error, RelayAskError::NotFound { .. })
}

pub(super) fn outcome_for_ask_error(
    error: &RelayAskError,
    is_iq: bool,
) -> Option<FullJidDeliveryOutcome> {
    tracing::warn!(
        ?error,
        "ordered relay: remote ask failed; classifying for client fallback"
    );
    match error {
        RelayAskError::NotFound { .. } => None,
        RelayAskError::Cancelled => Some(FullJidDeliveryOutcome::Dropped),
        RelayAskError::Send { effect, .. } => Some(match effect {
            RelaySendEffect::NoEffect => definite_no_effect_outcome(is_iq),
            RelaySendEffect::MaybeCommitted => FullJidDeliveryOutcome::MaybeCommitted,
        }),
    }
}

pub(super) fn ask_error_maybe_committed(error: &RelayAskError) -> bool {
    matches!(
        error,
        RelayAskError::Send {
            effect: RelaySendEffect::MaybeCommitted,
            ..
        }
    )
}
