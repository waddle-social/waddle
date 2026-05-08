use super::*;

pub(super) async fn queue_offline_delivery(
    deps: &Deps<'_>,
    recipient: BareJid,
    payload: waddle_xmpp::pending_delivery::PendingPayload,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
    original_message: Box<Message>,
) {
    // XEP-0160 §3 step 2/4 — persist for later delivery.
    // The classifier and OfflineDeliveryHandler have already
    // applied XEP-0160 §4 type rules and the XEP-0334 hint
    // matrix; here we just write the row.
    let Some(storage) = deps.pending_delivery_storage else {
        warn!(
            recipient = %recipient,
            "QueueOfflineDelivery emitted but pending_delivery_storage is not wired; \
             dropping (test fixture or unwired deployment)"
        );
        return;
    };
    let row = waddle_xmpp::pending_delivery::PendingRow {
        id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at,
        payload,
        flushed_in_session: None,
        outbound_sequence: None,
    };
    match storage.insert(row).await {
        Ok(waddle_xmpp::pending_delivery::InsertOutcome::Inserted) => {
            debug!(
                recipient = %recipient,
                "pending_delivery row inserted"
            );
        }
        Ok(waddle_xmpp::pending_delivery::InsertOutcome::QuotaExceeded) => {
            waddle_xmpp::prometheus::increment_pending_delivery_quota_exceeded();
            // XEP-0160 §3 step 3 + RFC 6120 §8.3 — return a
            // typed `<service-unavailable/>` bounce that
            // echoes the original payload (RFC 6120 §8.3.4
            // convention).
            //
            // **Known partial inconsistency**: ArchiveHandler
            // runs earlier in the chain than
            // OfflineDeliveryHandler, so by the time we get
            // here the message is already in MAM. Sender
            // sees `<service-unavailable/>` while the
            // recipient can still pull the message from MAM
            // catch-up on next reconnect — i.e. the bounce
            // is for the *live-delivery* obligation, not
            // for archival visibility.
            //
            // This matches every existing reference XMPP
            // server (Prosody, ejabberd) and is consistent
            // with XEP-0160 §3 step 3's narrow scope
            // ("offline message queue is full"). The
            // alternative — un-archiving on quota — would
            // race with concurrent MAM queries and break
            // XEP-0313's monotonic-archive invariant.
            let error = xmpp_parsers::stanza_error::StanzaError::new(
                xmpp_parsers::stanza_error::ErrorType::Cancel,
                xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable,
                "en",
                "Recipient's offline message queue is full",
            );
            let bounce = waddle_xmpp::protocol::handlers::errors::message_error_reply(
                &original_message,
                error,
            );
            let sender_jid = match bounce.to.clone() {
                Some(j) => j,
                None => {
                    warn!(
                        recipient = %recipient,
                        "bounce target JID missing; dropping bounce"
                    );
                    return;
                }
            };
            let bounce_stanza = waddle_xmpp::Stanza::Message(bounce);
            let mut delivered = false;
            match sender_jid.clone().try_into_full() {
                Ok(full) => {
                    if matches!(
                        deps.connection_registry.send_to(&full, bounce_stanza).await,
                        waddle_xmpp::registry::SendResult::Sent
                    ) {
                        delivered = true;
                    }
                }
                Err(bare) => {
                    for full in deps.connection_registry.get_resources_for_user(&bare) {
                        if matches!(
                            deps.connection_registry
                                .send_to(&full, bounce_stanza.clone())
                                .await,
                            waddle_xmpp::registry::SendResult::Sent
                        ) {
                            delivered = true;
                        }
                    }
                }
            }
            if delivered {
                warn!(
                    recipient = %recipient,
                    sender = %sender_jid,
                    "pending_delivery quota exceeded — bounced \
                     <service-unavailable/> to sender per XEP-0160 §3 step 3"
                );
            } else {
                // Sender is remote (cross-domain) or has no
                // resources currently bound. S2S routing of
                // the bounce is out of scope today; surface
                // the conformance gap loudly so it shows up
                // in deployment logs.
                warn!(
                    recipient = %recipient,
                    sender = %sender_jid,
                    "pending_delivery quota exceeded but \
                     <service-unavailable/> bounce was not \
                     deliverable (remote sender or no bound \
                     resource) — XEP-0160 §3 step 3 \
                     conformance gap until s2s lands"
                );
            }
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                error = %error,
                "pending_delivery insert failed"
            );
        }
    }
}
