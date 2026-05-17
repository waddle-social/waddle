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
    let sender = archived.from.to_bare();
    let original_message =
        super::archive_lookup::parse_archived_message_xml(archived.stanza_xml.as_deref())
            .unwrap_or_else(|| super::archive_lookup::fallback_archived_message(&archived));
    enqueue_xep0357_notification_candidate_for_message(
        state,
        recipient,
        &sender,
        archive_stanza_id,
        &original_message,
    )
    .await
}

async fn enqueue_xep0357_notification_candidate_for_message(
    state: &WebSocketState,
    recipient: &BareJid,
    sender: &BareJid,
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
            return NotificationCandidateQueueOutcome::RetryLater;
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
            return NotificationCandidateQueueOutcome::RetryLater;
        }
    };
    let mut has_first_party_target = false;
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
                has_first_party_target = true;
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
    if !has_first_party_target {
        return NotificationCandidateQueueOutcome::Completed;
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
    let is_mention = message_is_mention_for_recipient(original_message, recipient);
    Ok(crate::notification_settings_projection::PushDispatchDecision::evaluate(level, is_mention))
}

/// Returns `true` when the inbound XEP-0513 explicit-mention payloads
/// name `recipient` as a mentioned `<mention jid='…'/>`.
///
/// The recipient JID is the bare JID that owns the offline queue; that
/// is the canonical identity referenced by `<mention jid='…'/>` per
/// XEP-0513 §3. Channel-wide `<mention mentions='urn:xmpp:mentions:0#channel'/>`
/// is intentionally NOT treated as an individual mention here — the
/// XEP-0492 `<on-mention/>` semantics target explicit user mentions; the
/// channel-mention surface is for MUC reflector announcements, which do
/// not flow through the DM `QueueOfflineDelivery` arm.
fn message_is_mention_for_recipient(message: &Message, recipient: &BareJid) -> bool {
    waddle_xmpp::xep::extract_explicit_mentions(message)
        .is_some_and(|mentions| mentions.mentions_jid(recipient))
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
