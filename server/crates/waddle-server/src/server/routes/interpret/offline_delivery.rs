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
    // XEP-0513 mention bit is message-intrinsic and frozen at T0; T1
    // reads it back from the candidate row when running the XEP-0492
    // dispatch gate.
    //
    // Self-directed candidates (sender bare JID == recipient bare JID)
    // are rejected at the `NotificationCandidate::direct_message`
    // constructor as `SelfDirectedNotificationCandidate` — no row is
    // persisted, satisfying the compliance requirement that
    // self-notifications produce no candidate/outbox entry. This is
    // input validation, not recipient-state suppression, so it lives
    // at the typed constructor boundary alongside the existing
    // full-sender-JID and archive-id owner checks.
    let is_mention = message_is_mention_for_recipient(original_message, recipient);
    let hints = crate::notification_outbox::NotificationMessageHints::none()
        .with_noping(message_carries_recipient_noping(
            original_message,
            recipient,
        ))
        .with_xep0334(
            waddle_xmpp::xep::xep0334::has_hint(
                original_message,
                waddle_xmpp::xep::xep0334::Hint::NoStore,
            ),
            waddle_xmpp::xep::xep0334::has_hint(
                original_message,
                waddle_xmpp::xep::xep0334::Hint::NoPermanentStore,
            ),
        );
    let candidate = match crate::notification_outbox::NotificationCandidate::direct_message_with_hints(
        recipient.clone(),
        sender_jid.clone(),
        archive_stanza_id.clone(),
        is_mention,
        hints,
    ) {
        Ok(candidate) => candidate,
        Err(
            crate::notification_outbox::NotificationOutboxError::SelfDirectedNotificationCandidate(
                _,
            ),
        ) => {
            debug!(
                recipient = %recipient,
                sender = %sender,
                "XEP-0357 notification candidate skipped: self-directed (sender bare JID == recipient bare JID)"
            );
            return NotificationCandidateQueueOutcome::Completed;
        }
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
    // T0 XEP-0492 push-dispatch gate — compliance: suppressed
    // outcomes leave no row in `notification_candidates`. The same
    // typed evaluator runs again at T1 inside
    // `drain_pending_candidates_into_outbox` as a race-window guard.
    // DM evaluation never consults `room_policy`, so the no-op
    // adapter is sufficient here; the per-call cache is a fresh empty
    // map (one-shot eval).
    let room_policy = crate::notification_outbox::NoopRoomPolicy;
    let mut room_policy_cache = std::collections::BTreeMap::<
        BareJid,
        crate::notification_outbox::RoomPolicyCacheEntry,
    >::new();
    let dnd_reader = crate::notification_outbox::NoopDndReader;
    let mut dnd_cache =
        std::collections::BTreeMap::<BareJid, crate::notification_outbox::DndState>::new();
    let outcome = match crate::notification_outbox::evaluate_xep0492_at_dispatch(
        state
            .deps
            .protocol
            .notification_settings_projection
            .as_ref(),
        &room_policy,
        &dnd_reader,
        &candidate,
        &mut room_policy_cache,
        &mut dnd_cache,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            warn!(
                recipient = %recipient,
                sender = %sender,
                error = %error,
                "XEP-0492 notification setting lookup failed at T0; deferring DM candidate"
            );
            return NotificationCandidateQueueOutcome::RetryLater;
        }
    };
    match outcome {
        crate::notification_outbox::T1PushDispatchOutcome::Deliver => {}
        crate::notification_outbox::T1PushDispatchOutcome::Suppressed { reason } => {
            info!(
                recipient = %recipient,
                sender = %sender,
                is_mention,
                %reason,
                "T0 push gate suppressed XEP-0357 DM candidate; no candidate row persisted"
            );
            waddle_xmpp::prometheus::increment_push_suppressed(reason.as_db_value());
            return NotificationCandidateQueueOutcome::Completed;
        }
        crate::notification_outbox::T1PushDispatchOutcome::DeferUnknownRoomPolicy => {
            // DM evaluation does not consult room_policy, so this is
            // a structural invariant violation. Fail-loud and retry.
            warn!(
                recipient = %recipient,
                sender = %sender,
                "XEP-0492 evaluator returned DeferUnknownRoomPolicy for a DM candidate; \
                 this is structurally impossible — retrying"
            );
            return NotificationCandidateQueueOutcome::RetryLater;
        }
    }
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
                is_mention,
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

/// Returns `true` when the inbound XEP-0513 explicit mention naming
/// `recipient` also carries the `<noping/>` child element — the sender
/// explicitly opted the recipient out of being pinged for this mention.
///
/// Message-frozen at T0 onto the candidate row; the T1 evaluator reads
/// it back and suppresses with `SuppressedReason::Xep0513Noping`.
fn message_carries_recipient_noping(message: &Message, recipient: &BareJid) -> bool {
    waddle_xmpp::xep::extract_explicit_mentions(message).is_some_and(|mentions| {
        mentions.mentions.iter().any(|mention| {
            mention.noping
                && mention
                    .jid
                    .as_ref()
                    .is_some_and(|mentioned| mentioned == recipient)
        })
    })
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
