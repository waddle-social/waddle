use std::sync::Arc;

use chrono::{DateTime, Utc};
use jid::BareJid;
use kameo::actor::ActorRef;
use tracing::warn;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
#[cfg(test)]
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::pending_delivery::{InsertOutcome, PendingPayload, PendingRow, PendingRowId};
use waddle_xmpp::registry::{ConnectionRegistry, SendResult, UserRegistryActor};
#[cfg(test)]
use waddle_xmpp::stream_management::persistence::SmClaimFence;
use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, SmSessionPromotionLease};
use waddle_xmpp::Stanza;

use super::PromotedOutcome;

#[derive(Clone, Copy)]
pub(super) enum PendingInsertAuthority<'a> {
    CurrentSm {
        registry: &'a InMemorySmSessionRegistry,
        lease: &'a SmSessionPromotionLease,
    },
    TerminalSm {
        registry: &'a InMemorySmSessionRegistry,
        lease: &'a SmSessionPromotionLease,
    },
    #[cfg(test)]
    TestCurrent {
        session_id: &'a SmSessionId,
        fence: Option<&'a SmClaimFence>,
    },
    ObsoleteGeneration,
}

/// Bundled delivery handles for pending-storage promotion (ADR-0017 Phase 3
/// Slice 9): the DashMap send surface plus the actor-authoritative registry
/// used for bare-JID resource enumeration. Grouped into one type so
/// `insert_pending` doesn't cross clippy's `too_many_arguments` threshold.
#[derive(Clone, Copy)]
pub(super) struct DeliveryHandles<'a> {
    pub registry: &'a ConnectionRegistry,
    pub user_registry: &'a ActorRef<UserRegistryActor>,
}

pub(super) async fn promote_as_transient(
    message: xmpp_parsers::message::Message,
    recipient_bare: BareJid,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    original_receipt_fallback: DateTime<Utc>,
    delivery: DeliveryHandles<'_>,
    authority: PendingInsertAuthority<'_>,
) -> PromotedOutcome {
    let payload = PendingPayload::Transient(Box::new(message.clone()));
    insert_pending(
        recipient_bare,
        payload,
        pending_storage,
        original_receipt_fallback,
        &message,
        delivery,
        authority,
    )
    .await
}

/// Insert one Q6-promoted `pending_delivery` row.
///
/// Current and terminal-durable generations insert under their immutable
/// fences. Terminal authority is generation-qualified in the registry and
/// never borrows successor state. Obsolete generations use an ordinary
/// unlinked insert without borrowing successor authority.
pub(super) async fn insert_pending(
    recipient: BareJid,
    payload: PendingPayload,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    original_receipt_at: DateTime<Utc>,
    original_message: &xmpp_parsers::message::Message,
    delivery: DeliveryHandles<'_>,
    authority: PendingInsertAuthority<'_>,
) -> PromotedOutcome {
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at,
        payload,
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let has_durable_authority = matches!(
        authority,
        PendingInsertAuthority::CurrentSm { .. } | PendingInsertAuthority::TerminalSm { .. }
    );
    #[cfg(test)]
    let has_durable_authority =
        has_durable_authority || matches!(authority, PendingInsertAuthority::TestCurrent { .. });
    let insert_result = match authority {
        PendingInsertAuthority::CurrentSm { registry, lease } => {
            let guard = match registry.lock_current_promotion_mutation(lease).await {
                Ok(guard) => guard,
                Err(waddle_xmpp::stream_management::SmRegistryError::PromotionAuthorityLost) => {
                    return PromotedOutcome::AuthorityLost;
                }
                Err(error) => {
                    warn!(
                        recipient = %recipient,
                        %error,
                        "Q6 promotion: could not validate current-generation authority"
                    );
                    return PromotedOutcome::StorageFailure;
                }
            };
            pending_storage
                .insert_under_sm_fence(row, guard.session_id(), guard.claim_fence())
                .await
        }
        PendingInsertAuthority::TerminalSm { registry, lease } => {
            let guard = match registry.lock_terminal_promotion_mutation(lease).await {
                Ok(guard) => guard,
                Err(waddle_xmpp::stream_management::SmRegistryError::PromotionAuthorityLost) => {
                    return PromotedOutcome::AuthorityLost;
                }
                Err(error) => {
                    warn!(
                        recipient = %recipient,
                        %error,
                        "Q6 promotion: could not validate terminal-generation authority"
                    );
                    return PromotedOutcome::StorageFailure;
                }
            };
            let key = guard.key();
            pending_storage
                .insert_under_sm_fence(row, key.stream_id(), guard.claim_fence())
                .await
        }
        #[cfg(test)]
        PendingInsertAuthority::TestCurrent { session_id, fence } => {
            pending_storage
                .insert_under_sm_fence(row, session_id, fence)
                .await
        }
        PendingInsertAuthority::ObsoleteGeneration => pending_storage.insert(row).await,
    };
    match insert_result {
        Ok(InsertOutcome::Inserted) => PromotedOutcome::Queued,
        Ok(InsertOutcome::QuotaExceeded) => {
            // XEP-0160 §3 step 3 + RFC 6120 §8.3 — bounce
            // <service-unavailable/> to the sender. We use the same
            // typed StanzaError builder the routing layer uses for
            // intake-time quota overflow so the wire shape is
            // identical.
            waddle_xmpp::telemetry::reliability::increment_pending_delivery_quota_exceeded();
            send_quota_bounce(original_message, &recipient, delivery).await;
            PromotedOutcome::Bounced
        }
        Err(waddle_xmpp::pending_delivery::storage::PendingStorageError::NotOwner { entity })
            if has_durable_authority =>
        {
            // FIX 3: this node's claim on the origin SM session was lost
            // (or never held) by the time the fenced write ran — another
            // node's own janitor/reaper is (or is about to be) the real
            // owner. Never confirm_drained on this outcome: the caller's
            // typed match on `PromotedOutcome` must treat this the same as
            // a storage failure so the durable SM row survives for that
            // node's own promote/confirm pass, never dead-lettered here.
            warn!(
                recipient = %recipient,
                %entity,
                "Q6 promotion: exact pending insert observed a lost claim \
                 (NotOwner); caller must NOT confirm_drained so the durable SM row \
                 survives for the current owner's own promotion pass"
            );
            PromotedOutcome::AuthorityLost
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                error = %error,
                "Q6 promotion: pending_delivery insert failed; \
                 caller must NOT confirm_drained so durable SM row survives \
                 for restart-time retry"
            );
            PromotedOutcome::StorageFailure
        }
    }
}

async fn send_quota_bounce(
    original_message: &xmpp_parsers::message::Message,
    recipient: &BareJid,
    delivery: DeliveryHandles<'_>,
) {
    let error = xmpp_parsers::stanza_error::StanzaError::new(
        xmpp_parsers::stanza_error::ErrorType::Cancel,
        xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable,
        "en",
        "Recipient's offline message queue is full",
    );
    let bounce =
        waddle_xmpp::protocol::handlers::errors::message_error_reply(original_message, error);
    let Some(sender_jid) = bounce.to.clone() else {
        warn!(
            recipient = %recipient,
            "Q6 promotion: bounce target JID missing; dropping bounce"
        );
        return;
    };
    let stanza = Stanza::Message(bounce);
    let mut delivered = false;
    match sender_jid.clone().try_into_full() {
        Ok(full) => {
            if matches!(
                delivery.registry.send_to(&full, stanza).await,
                SendResult::Sent
            ) {
                delivered = true;
            }
        }
        Err(bare) => {
            let resources =
                waddle_xmpp::registry::get_resources_for_user(delivery.user_registry, &bare).await;
            for full in resources {
                if matches!(
                    delivery.registry.send_to(&full, stanza.clone()).await,
                    SendResult::Sent
                ) {
                    delivered = true;
                }
            }
        }
    }
    if !delivered {
        warn!(
            recipient = %recipient,
            sender = %sender_jid,
            "Q6 promotion: <service-unavailable/> bounce was not deliverable \
             (remote sender or no bound resource) — XEP-0160 §3 step 3 \
             conformance gap until s2s lands"
        );
    }
}
