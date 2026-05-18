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
    let row_id = waddle_xmpp::pending_delivery::PendingRowId::fresh();
    let row = waddle_xmpp::pending_delivery::PendingRow {
        id: row_id.clone(),
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
            let outcome = enqueue_xep0357_notification_candidate(
                deps,
                &recipient,
                notification_archive_stanza_id.as_ref(),
            )
            .await;
            if notification_archive_stanza_id.is_some()
                && outcome == NotificationCandidateQueueOutcome::Completed
            {
                mark_pending_notification_outboxed(storage.as_ref(), &row_id, &recipient).await;
            }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotificationCandidateQueueOutcome {
    Completed,
    RetryLater,
}

async fn enqueue_xep0357_notification_candidate(
    deps: &Deps<'_>,
    recipient: &BareJid,
    archive_stanza_id: Option<&waddle_xmpp_core::xep0359::StanzaId>,
) -> NotificationCandidateQueueOutcome {
    let Some(archive_stanza_id) = archive_stanza_id else {
        debug!(
            recipient = %recipient,
            "Skipping XEP-0357 candidate for transient offline payload because no committed archive state exists"
        );
        return NotificationCandidateQueueOutcome::Completed;
    };
    let Some(state) = deps.web_socket_state else {
        return NotificationCandidateQueueOutcome::RetryLater;
    };
    enqueue_xep0357_notification_candidate_from_committed_archive(
        state,
        recipient,
        archive_stanza_id,
    )
    .await
}

async fn enqueue_xep0357_notification_candidate_from_committed_archive(
    state: &WebSocketState,
    recipient: &BareJid,
    archive_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
) -> NotificationCandidateQueueOutcome {
    let archive_bare = archive_stanza_id.by.to_bare();
    let archived = match state
        .deps
        .protocol
        .mam_storage
        .get_message_by_archive_or_stanza_id(&archive_bare, archive_stanza_id.as_str())
        .await
    {
        Ok(Some(archived)) => archived,
        Ok(None) => {
            warn!(
                recipient = %recipient,
                stanza_id = %archive_stanza_id,
                "XEP-0357 notification candidate skipped because committed MAM row is missing"
            );
            return NotificationCandidateQueueOutcome::Completed;
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                stanza_id = %archive_stanza_id,
                error = %error,
                "XEP-0357 notification candidate could not load committed MAM row"
            );
            return NotificationCandidateQueueOutcome::RetryLater;
        }
    };
    let parsed_original_message =
        super::archive_lookup::parse_archived_message_xml(archived.stanza_xml.as_deref());
    let Some(sender_jid) = notification_sender_jid(&archived, parsed_original_message.as_ref())
    else {
        warn!(
            recipient = %recipient,
            stanza_id = %archive_stanza_id,
            archive_sender = %archived.from,
            "XEP-0357 notification candidate skipped because exact sender resource provenance is unavailable"
        );
        return NotificationCandidateQueueOutcome::Completed;
    };
    let sender = sender_jid.to_bare();
    let original_message = parsed_original_message
        .unwrap_or_else(|| super::archive_lookup::fallback_archived_message(&archived));
    enqueue_xep0357_notification_candidate_for_message(
        state,
        recipient,
        &sender,
        &sender_jid,
        archive_stanza_id,
        &original_message,
    )
    .await
}

fn notification_sender_jid(
    archived: &MamArchivedMessage,
    original_message: Option<&Message>,
) -> Option<Jid> {
    if let Some(from) = original_message.and_then(|message| message.from.clone()) {
        if let Some(_resource) = from.resource() {
            if archived.from.resource().is_some() {
                if from == archived.from {
                    return Some(from);
                }
            } else if from.to_bare() == archived.from.to_bare() {
                return Some(from);
            }
        }
        warn!(
            archive_sender = %archived.from,
            stanza_sender = %from,
            "Archived stanza XML sender conflicted with MAM row sender; skipping push candidate"
        );
        return None;
    }

    if archived.from.resource().is_some() {
        Some(archived.from.clone())
    } else {
        None
    }
}

async fn enqueue_xep0357_notification_candidate_for_message(
    state: &WebSocketState,
    recipient: &BareJid,
    sender: &BareJid,
    sender_jid: &Jid,
    archive_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    original_message: &Message,
) -> NotificationCandidateQueueOutcome {
    // XEP-0492 gate: consult the recipient's per-conversation notification
    // level (defaulting to XEP-0492 conversation-kind defaults via the
    // projection store) and the XEP-0513 mention bit. The decision is a
    // typed `PushDispatchDecision` — never a stringly-typed diagnostic —
    // so the suppression reason flows through to the typed log line.
    let decision =
        match evaluate_xep0492_push_dispatch_decision(state, recipient, sender, original_message)
            .await
        {
            Ok(decision) => decision,
            Err(()) => return NotificationCandidateQueueOutcome::RetryLater,
        };
    match decision {
        crate::notification_settings_projection::PushDispatchDecision::Deliver => {}
        crate::notification_settings_projection::PushDispatchDecision::Suppressed { reason } => {
            info!(
                recipient = %recipient,
                sender = %sender,
                reason = %reason,
                "XEP-0492 push gate suppressed XEP-0357 notification candidate"
            );
            return NotificationCandidateQueueOutcome::Completed;
        }
    }
    let candidate = match crate::notification_outbox::NotificationCandidate::direct_message(
        recipient.clone(),
        sender_jid.clone(),
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
            return NotificationCandidateQueueOutcome::Completed;
        }
    };
    match state
        .deps
        .protocol
        .notification_outbox
        .insert_candidate(&candidate)
        .await
    {
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Inserted) => {
            debug!(
                recipient = %recipient,
                sender = %sender,
                "XEP-0357 notification candidate inserted for durable outbox worker"
            );
            NotificationCandidateQueueOutcome::Completed
        }
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Duplicate) => {
            debug!(
                recipient = %recipient,
                sender = %sender,
                "Duplicate XEP-0357 notification candidate ignored"
            );
            NotificationCandidateQueueOutcome::Completed
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                sender = %sender,
                error = %error,
                "XEP-0357 notification candidate insert failed"
            );
            NotificationCandidateQueueOutcome::RetryLater
        }
    }
}

/// Resolve the XEP-0492 push-dispatch gate for a single inbound DM that
/// is about to be projected into the recipient's offline queue.
///
/// The gate combines the recipient's typed
/// [`waddle_xmpp::xep::NotificationLevel`] (resolved by the
/// `NotificationSettingsProjectionStore`, falling back to the XEP-0492
/// conversation-kind defaults) with the XEP-0513 mention bit derived
/// directly from the inbound `<message>` payloads. Both inputs flow as
/// typed values; there are no string-typed payloads on the gate boundary.
///
/// `QueueOfflineDelivery` only fires for DM intake
/// ([`waddle_xmpp::protocol::handlers::offline_delivery::OfflineDeliveryHandler`]
/// is gated on `Locality::Recipient` + headless pass for `<message
/// type='chat'>`), so the conversation kind on this path is always
/// `ConversationKind::Direct`. The shared pure reducer
/// [`crate::notification_settings_projection::PushDispatchDecision::evaluate`]
/// is the single decision point — when MUC push fan-out lands it will
/// reuse the same reducer rather than re-implementing the level matrix.
async fn evaluate_xep0492_push_dispatch_decision(
    state: &WebSocketState,
    recipient: &BareJid,
    sender: &BareJid,
    original_message: &Message,
) -> Result<crate::notification_settings_projection::PushDispatchDecision, ()> {
    if sender == recipient {
        // Self-DM: never push to your own offline queue. Per XEP-0492
        // semantics this is a hard suppression independent of the
        // configured level; surface it as `Never` to keep the typed log
        // path uniform.
        return Ok(
            crate::notification_settings_projection::PushDispatchDecision::Suppressed {
                reason: waddle_xmpp::xep::NotificationLevel::Never,
            },
        );
    }
    let level = match state
        .deps
        .protocol
        .notification_settings_projection
        .effective_setting(
            recipient,
            sender,
            crate::notification_settings_projection::ConversationKind::Direct,
        )
        .await
    {
        Ok(level) => level,
        Err(error) => {
            warn!(
                recipient = %recipient,
                conversation = %sender,
                error = %error,
                "XEP-0492 notification setting lookup failed; retrying notification candidate later"
            );
            return Err(());
        }
    };
    let is_mention = crate::notification_mentions::direct_message_mentions_recipient(
        original_message,
        recipient,
    );
    Ok(crate::notification_settings_projection::PushDispatchDecision::evaluate(level, is_mention))
}

async fn mark_pending_notification_outboxed(
    storage: &dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage,
    row_id: &waddle_xmpp::pending_delivery::PendingRowId,
    recipient: &BareJid,
) {
    if let Err(error) = storage.mark_notification_outboxed(row_id).await {
        warn!(
            recipient = %recipient,
            row_id = %row_id,
            error = %error,
            "pending_delivery notification outbox marker write failed; janitor will retry"
        );
    }
}

pub(crate) async fn reconcile_xep0357_notification_candidates(
    state: &WebSocketState,
    batch_size: usize,
) -> usize {
    let batch_size = batch_size.clamp(1, 1_000);
    let pending_storage = state.deps.protocol.pending_delivery_storage.as_ref();
    let rows = match pending_storage.list_unoutboxed_archived(batch_size).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(
                error = %error,
                "XEP-0357 notification candidate recovery could not read pending_delivery rows"
            );
            return 0;
        }
    };
    let mut completed = 0usize;
    for row in rows {
        let waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id) =
            &row.payload
        else {
            continue;
        };
        let outcome = enqueue_xep0357_notification_candidate_from_committed_archive(
            state,
            &row.recipient,
            archive_stanza_id,
        )
        .await;
        if outcome == NotificationCandidateQueueOutcome::Completed {
            mark_pending_notification_outboxed(pending_storage, &row.id, &row.recipient).await;
            completed += 1;
        }
    }
    completed
}
