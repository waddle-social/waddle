use std::sync::Arc;

use chrono::{DateTime, Utc};
use jid::BareJid;
use kameo::actor::ActorRef;
use tracing::warn;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{InsertOutcome, PendingPayload, PendingRow, PendingRowId};
use waddle_xmpp::registry::{ConnectionRegistry, SendResult, UserRegistryActor};
use waddle_xmpp::Stanza;

use super::{PromotedOutcome, PromotionFailureReason};

/// Bundled delivery handles for pending-storage promotion (ADR-0017 Phase 3
/// Slice 9): the DashMap send surface plus the actor-authoritative registry
/// used for bare-JID resource enumeration. Grouped into one type so
/// `insert_pending` doesn't cross clippy's `too_many_arguments` threshold.
#[derive(Clone, Copy)]
pub(super) struct DeliveryHandles<'a> {
    pub registry: &'a ConnectionRegistry,
    pub user_registry: &'a ActorRef<UserRegistryActor>,
    pub allow_unfenced_effects: bool,
    pub server_domain: &'a str,
}

/// Select the durable authority that must commit with a pending insertion.
#[derive(Clone, Copy)]
pub(super) enum PromotionOrigin<'a> {
    Stream {
        stream_id: &'a str,
        sequence: u32,
        expected_fence: Option<&'a waddle_xmpp::stream_management::persistence::SmClaimFence>,
    },
    IngressCustody(&'a waddle_xmpp::stream_management::persistence::PersistedIngressAppend),
}

pub(super) async fn promote_as_transient(
    message: xmpp_parsers::message::Message,
    recipient_bare: BareJid,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    original_receipt_fallback: DateTime<Utc>,
    delivery: DeliveryHandles<'_>,
    origin: PromotionOrigin<'_>,
) -> PromotedOutcome {
    let payload = PendingPayload::Transient(Box::new(message.clone()));
    insert_pending(
        recipient_bare,
        payload,
        pending_storage,
        original_receipt_fallback,
        &message,
        delivery,
        origin,
    )
    .await
}

/// Insert one Q6-promoted `pending_delivery` row.
///
/// `origin_stream_id` is the SM session whose unacked queue is being
/// promoted (ADR-0017 Phase 3 Slice 5 FIX 3, council-adjudicated):
/// element 9's locked text requires "promotion executes under the
/// row-locked fenced epoch," so this calls
/// [`PendingDeliveryStorage::insert_fenced`], not the bare `insert` — a
/// cluster-fenced storage runs the write inside one transaction carrying
/// the origin session's claim `SELECT ... FOR SHARE` fencing check
/// (aborting with [`PendingStorageError::NotOwner`] before writing if this
/// node no longer holds that claim); every non-fenced implementation
/// (portable/SQLite, or clustering disabled) falls back to the identical
/// unfenced `insert` path via `insert_fenced`'s own default impl.
pub(super) async fn insert_pending(
    recipient: BareJid,
    payload: PendingPayload,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    original_receipt_at: DateTime<Utc>,
    original_message: &xmpp_parsers::message::Message,
    delivery: DeliveryHandles<'_>,
    origin: PromotionOrigin<'_>,
) -> PromotedOutcome {
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at,
        payload,
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let result = match origin {
        PromotionOrigin::Stream {
            stream_id,
            sequence,
            expected_fence,
        } => {
            pending_storage
                .insert_promotion(row, stream_id, sequence, expected_fence)
                .await
        }
        PromotionOrigin::IngressCustody(append) => {
            use waddle_xmpp::pending_delivery::storage::CustodyInsertOutcome;
            match pending_storage.insert_ingress_custody(row, append).await {
                Ok(CustodyInsertOutcome::Inserted) => Ok(InsertOutcome::Inserted),
                Ok(CustodyInsertOutcome::AlreadyCompleted) => {
                    return PromotedOutcome::NotPromotable
                }
                // A quota error sent to an in-memory socket is not a durable
                // replacement for the retained original payload.
                Ok(CustodyInsertOutcome::QuotaExceeded) => {
                    return PromotedOutcome::StorageFailure(
                        PromotionFailureReason::CustodyQuotaExceeded,
                    );
                }
                Err(error) => Err(error),
            }
        }
    };
    match result {
        Ok(InsertOutcome::Inserted) => PromotedOutcome::Queued,
        Ok(InsertOutcome::QuotaExceeded) => {
            if original_message.from.is_none() {
                // No peer return target exists. Retire this nonbounceable
                // account/server frame at admission failure, without a
                // quota-bounce metric or a per-pass warning.
                waddle_xmpp::telemetry::reliability::increment_sm_promotion_not_promotable();
                return PromotedOutcome::NotPromotable;
            }
            // A return error is a replacement custody sink only once a live
            // sender accepted it or its local offline queue committed it.
            // Count the completed rejection once, not every failed retry.
            let outcome = return_quota_bounce(
                original_message,
                pending_storage,
                original_receipt_at,
                delivery,
                origin,
            )
            .await;
            if matches!(outcome, PromotedOutcome::Bounced) {
                waddle_xmpp::telemetry::reliability::increment_pending_delivery_quota_exceeded();
            }
            outcome
        }

        Err(waddle_xmpp::pending_delivery::storage::PendingStorageError::NotOwner { entity }) => {
            // FIX 3: this node's claim on the origin SM session was lost
            // (or never held) by the time the fenced write ran — another
            // node's own janitor/reaper is (or is about to be) the real
            // owner. Never confirm_drained on this outcome: the caller's
            // typed match on `PromotedOutcome` must treat this the same as
            // a storage failure so the durable SM row survives for that
            // node's own promote/confirm pass, never dead-lettered here.
            warn!(
                recipient = %recipient,
                message_id = original_message.id.as_ref().map_or("", |id| id.0.as_str()),
                %entity,
                "Q6 promotion: pending_delivery insert_fenced observed a lost claim \
                 (NotOwner); caller must NOT confirm_drained so the durable SM row \
                 survives for the current owner's own promotion pass"
            );
            PromotedOutcome::StorageFailure(PromotionFailureReason::ClaimLost)
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                message_id = original_message.id.as_ref().map_or("", |id| id.0.as_str()),
                error = %error,
                "Q6 promotion: pending_delivery insert failed; \
                 caller must NOT confirm_drained so durable SM row survives \
                 for restart-time retry"
            );
            PromotedOutcome::StorageFailure(PromotionFailureReason::PendingStorage)
        }
    }
}

async fn return_quota_bounce(
    original_message: &xmpp_parsers::message::Message,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    original_receipt_at: DateTime<Utc>,
    delivery: DeliveryHandles<'_>,
    origin: PromotionOrigin<'_>,
) -> PromotedOutcome {
    let error = xmpp_parsers::stanza_error::StanzaError::new(
        xmpp_parsers::stanza_error::ErrorType::Cancel,
        xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable,
        "en",
        "Recipient's offline message queue is full",
    );
    let bounce =
        waddle_xmpp::protocol::handlers::errors::message_error_reply(original_message, error);
    let Some(sender_jid) = bounce.to.clone() else {
        return PromotedOutcome::StorageFailure(PromotionFailureReason::QuotaBounceUnavailable);
    };
    if delivery.allow_unfenced_effects {
        let stanza = Stanza::Message(bounce.clone());
        let targets = match sender_jid.clone().try_into_full() {
            Ok(full) => vec![full],
            Err(bare) => {
                waddle_xmpp::registry::get_resources_for_user(delivery.user_registry, &bare).await
            }
        };
        let mut delivered = false;
        for full in targets {
            if matches!(
                delivery.registry.send_to(&full, stanza.clone()).await,
                SendResult::Sent
            ) {
                delivered = true;
            }
        }
        if delivered {
            return PromotedOutcome::Bounced;
        }
    }

    // Remote error routing awaits S2S. A local sender can reconnect to any
    // resource: pending delivery is addressed by bare JID, retaining the
    // typed error's original `to` while flush routes to the reconnecting resource.
    // Do not recurse through DM classification: errors are normally not
    // stored offline (XEP-0160 §4); this retained rejection is deliberate
    // custody for the original message's XEP-0198 terminal outcome.
    if sender_jid.node().is_none() || sender_jid.domain().as_str() != delivery.server_domain {
        return PromotedOutcome::StorageFailure(PromotionFailureReason::QuotaBounceUnavailable);
    }
    let PromotionOrigin::Stream {
        stream_id,
        sequence,
        expected_fence,
    } = origin
    else {
        // Ingress transfer is bound to its original recipient; it cannot
        // atomically transfer custody into the sender's queue.
        return PromotedOutcome::StorageFailure(PromotionFailureReason::CustodyQuotaExceeded);
    };
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: sender_jid.to_bare(),
        original_receipt_at,
        payload: PendingPayload::Transient(Box::new(bounce)),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    match pending_storage
        .insert_promotion(row, stream_id, sequence, expected_fence)
        .await
    {
        Ok(InsertOutcome::Inserted) => PromotedOutcome::Bounced,
        Ok(InsertOutcome::QuotaExceeded) => {
            PromotedOutcome::StorageFailure(if delivery.allow_unfenced_effects {
                PromotionFailureReason::QuotaBounceUnavailable
            } else {
                PromotionFailureReason::ClusteredQuotaExceeded
            })
        }
        Err(waddle_xmpp::pending_delivery::storage::PendingStorageError::NotOwner { .. }) => {
            PromotedOutcome::StorageFailure(PromotionFailureReason::ClaimLost)
        }
        Err(_) => PromotedOutcome::StorageFailure(PromotionFailureReason::PendingStorage),
    }
}
