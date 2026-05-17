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
    let notification_archive_stanza_id = match &payload {
        waddle_xmpp::pending_delivery::PendingPayload::Archived(stanza_id) => {
            Some(stanza_id.clone())
        }
        waddle_xmpp::pending_delivery::PendingPayload::Transient(_) => None,
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
            enqueue_xep0357_notification_candidate(
                deps,
                &recipient,
                notification_archive_stanza_id.as_ref(),
                &original_message,
            )
            .await;
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

async fn enqueue_xep0357_notification_candidate(
    deps: &Deps<'_>,
    recipient: &BareJid,
    archive_stanza_id: Option<&waddle_xmpp_core::xep0359::StanzaId>,
    original_message: &Message,
) {
    let Some(archive_stanza_id) = archive_stanza_id else {
        debug!(
            recipient = %recipient,
            "Skipping XEP-0357 candidate for transient offline payload"
        );
        return;
    };
    let Some(state) = deps.web_socket_state else {
        return;
    };
    let Some(sender) = original_message.from.as_ref().map(|jid| jid.to_bare()) else {
        return;
    };
    if !should_publish_xep0357_notification(state, recipient, &sender).await {
        return;
    }
    let registrations = match state
        .deps
        .protocol
        .push_store
        .get_for_user(&recipient.to_string())
        .await
    {
        Ok(registrations) => registrations,
        Err(error) => {
            warn!(
                recipient = %recipient,
                error = %error,
                "XEP-0357 push registration lookup failed after pending_delivery insert"
            );
            return;
        }
    };
    let first_party_service = state.deps.service_domains.push.as_str();
    let first_party_service_jid: BareJid = match first_party_service.parse() {
        Ok(jid) => jid,
        Err(error) => {
            warn!(
                recipient = %recipient,
                push_service = first_party_service,
                error = %error,
                "first-party Push Service JID is invalid; skipping notification candidate"
            );
            return;
        }
    };
    let mut targets = Vec::new();
    for registration in registrations {
        if registration.service_jid != first_party_service {
            debug!(
                recipient = %recipient,
                service = %registration.service_jid,
                "XEP-0357 external Push Service publish is not wired in this first-party boundary"
            );
            continue;
        }
        match crate::notification_outbox::target_from_subscription(&registration) {
            Ok(Some(target)) if target.push_service_jid() == &first_party_service_jid => {
                targets.push(target);
            }
            Ok(Some(target)) => {
                warn!(
                    recipient = %recipient,
                    registration_service = %registration.service_jid,
                    target_service = %target.push_service_jid(),
                    "first-party XEP-0357 registration target did not parse back to the configured service"
                );
            }
            Ok(None) => {
                warn!(
                    recipient = %recipient,
                    service = %registration.service_jid,
                    "first-party XEP-0357 registration missing node; skipping notification candidate target"
                );
            }
            Err(error) => {
                warn!(
                    recipient = %recipient,
                    error = %error,
                    "first-party XEP-0357 registration could not be converted into an outbox target"
                );
            }
        }
    }
    if targets.is_empty() {
        return;
    }

    let candidate = match crate::notification_outbox::NotificationCandidate::direct_message(
        recipient.clone(),
        sender.clone(),
        archive_stanza_id.clone(),
    ) {
        Ok(candidate) => candidate,
        Err(error) => {
            warn!(
                recipient = %recipient,
                sender = %sender,
                error = %error,
                "XEP-0357 notification candidate rejected"
            );
            return;
        }
    };
    match state
        .deps
        .protocol
        .notification_outbox
        .insert_candidate_and_enqueue(&candidate, &targets)
        .await
    {
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Inserted {
            enqueued_jobs,
        }) => {
            debug!(
                recipient = %recipient,
                sender = %sender,
                enqueued_jobs,
                "XEP-0357 notification candidate inserted into durable outbox"
            );
        }
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Duplicate) => {
            debug!(
                recipient = %recipient,
                sender = %sender,
                "Duplicate XEP-0357 notification candidate ignored"
            );
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                sender = %sender,
                error = %error,
                "XEP-0357 notification candidate insert failed"
            );
        }
    }
}

async fn should_publish_xep0357_notification(
    state: &WebSocketState,
    recipient: &BareJid,
    sender: &BareJid,
) -> bool {
    if sender == recipient {
        return false;
    }
    let setting = state
        .deps
        .protocol
        .notification_settings_projection
        .effective_setting(
            recipient,
            sender,
            crate::notification_settings_projection::ConversationKind::Direct,
        )
        .await;
    match setting {
        Ok(setting) => setting.should_notify(false),
        Err(error) => {
            warn!(
                recipient = %recipient,
                conversation = %sender,
                error = %error,
                "XEP-0492 notification setting lookup failed; suppressing push notification"
            );
            false
        }
    }
}
