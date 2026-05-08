use std::sync::Arc;

use chrono::{DateTime, Utc};
use jid::BareJid;
use tracing::warn;
use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
use waddle_xmpp::pending_delivery::{InsertOutcome, PendingPayload, PendingRow, PendingRowId};
use waddle_xmpp::registry::{ConnectionRegistry, SendResult};
use waddle_xmpp::Stanza;

use super::PromotedOutcome;

pub(super) async fn promote_as_transient(
    message: xmpp_parsers::message::Message,
    recipient_bare: BareJid,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    original_receipt_fallback: DateTime<Utc>,
    registry: &ConnectionRegistry,
) -> PromotedOutcome {
    let payload = PendingPayload::Transient(Box::new(message.clone()));
    insert_pending(
        recipient_bare,
        payload,
        pending_storage,
        original_receipt_fallback,
        &message,
        registry,
    )
    .await
}

pub(super) async fn insert_pending(
    recipient: BareJid,
    payload: PendingPayload,
    pending_storage: &Arc<dyn PendingDeliveryStorage>,
    original_receipt_at: DateTime<Utc>,
    original_message: &xmpp_parsers::message::Message,
    registry: &ConnectionRegistry,
) -> PromotedOutcome {
    let row = PendingRow {
        id: PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at,
        payload,
        flushed_in_session: None,
        outbound_sequence: None,
    };
    match pending_storage.insert(row).await {
        Ok(InsertOutcome::Inserted) => PromotedOutcome::Queued,
        Ok(InsertOutcome::QuotaExceeded) => {
            // XEP-0160 §3 step 3 + RFC 6120 §8.3 — bounce
            // <service-unavailable/> to the sender. We use the same
            // typed StanzaError builder the routing layer uses for
            // intake-time quota overflow so the wire shape is
            // identical.
            waddle_xmpp::prometheus::increment_pending_delivery_quota_exceeded();
            send_quota_bounce(original_message, &recipient, registry).await;
            PromotedOutcome::Bounced
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
    registry: &ConnectionRegistry,
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
            if matches!(registry.send_to(&full, stanza).await, SendResult::Sent) {
                delivered = true;
            }
        }
        Err(bare) => {
            for full in registry.get_resources_for_user(&bare) {
                if matches!(
                    registry.send_to(&full, stanza.clone()).await,
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
