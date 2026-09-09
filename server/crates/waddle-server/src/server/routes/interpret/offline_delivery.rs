use super::effects::delivery::PreparedOfflineNotification;
use super::*;

pub(super) async fn queue_offline_delivery(
    deps: &Deps<'_>,
    recipient: BareJid,
    payload: waddle_xmpp::pending_delivery::PendingPayload,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
    original_message: Box<Message>,
) {
    let row = waddle_xmpp::pending_delivery::PendingRow {
        id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
        recipient,
        original_receipt_at,
        payload,
        flushed_in_session: None,
        outbound_sequence: None,
    };
    apply_offline_delivery_row(deps, row, original_message).await;
}

/// Capture the frozen row and notification obligations for the ingress arm.
pub(super) async fn apply_offline_delivery_row(
    deps: &Deps<'_>,
    row: waddle_xmpp::pending_delivery::PendingRow,
    original_message: Box<Message>,
) {
    let recipient = row.recipient.clone();
    let payload = &row.payload;
    // XEP-0160 §3 step 2/4 — persist for later delivery.
    // The classifier and OfflineDeliveryHandler have already
    // applied XEP-0160 §4 type rules and the XEP-0334 hint
    // matrix; here we capture the row for transactional execution.
    let Some(_) = deps.pending_delivery_storage else {
        warn!(
            recipient = %recipient,
            "QueueOfflineDelivery emitted but pending_delivery_storage is not wired; \
             dropping (test fixture or unwired deployment)"
        );
        return;
    };
    let notification_archive_stanza_id = match payload {
        waddle_xmpp::pending_delivery::PendingPayload::Archived(stanza_id) => {
            Some(stanza_id.clone())
        }
        waddle_xmpp::pending_delivery::PendingPayload::Transient(_) => None,
    };
    let row_id = row.id.clone();
    let pending_delivery_mutation = match payload {
        waddle_xmpp::pending_delivery::PendingPayload::Archived(archive_stanza_id) => {
            waddle_xmpp::ingress::PendingDeliveryMutation::Archived {
                recipient: recipient.clone(),
                row_id: row_id.clone(),
                archive_stanza_id: archive_stanza_id.clone(),
            }
        }
        waddle_xmpp::pending_delivery::PendingPayload::Transient(_) => {
            waddle_xmpp::ingress::PendingDeliveryMutation::Transient {
                recipient: recipient.clone(),
                row_id: row_id.clone(),
            }
        }
    };
    if deps.effects.is_planning() {
        let prepared_notification = prepare_offline_notification(
            deps,
            &recipient,
            notification_archive_stanza_id.as_ref(),
            &original_message,
        )
        .await;
        if matches!(
            &prepared_notification,
            PreparedOfflineNotification::Prepared(_)
        ) {
            if let Some(archive_stanza_id) = notification_archive_stanza_id.as_ref() {
                deps.capture_intent(IngressEffectIntent::NotificationActivityPreview {
                    owner: recipient.clone(),
                    mutation:
                        waddle_xmpp::ingress::NotificationActivityMutation::NotificationCandidate {
                            conversation: recipient.clone(),
                            archive_stanza_id: archive_stanza_id.clone(),
                            outcome: waddle_xmpp::ingress::NotificationCandidateOutcome::Inserted,
                        },
                });
            }
        }
        deps.capture_intent(IngressEffectIntent::PendingDelivery {
            mutation: pending_delivery_mutation,
        });
        if let Some(archive_stanza_id) = notification_archive_stanza_id.filter(|_| {
            !matches!(
                prepared_notification,
                PreparedOfflineNotification::RetryLater
            )
        }) {
            deps.capture_intent(IngressEffectIntent::NotificationActivityPreview {
                owner: recipient.clone(),
                mutation: waddle_xmpp::ingress::NotificationActivityMutation::OfflineDelivery {
                    conversation: recipient,
                    archive_stanza_id,
                },
            });
        }
        super::effects::delivery::record(
            deps,
            super::effects::delivery::ExternalDeliveryEffect::QueueOfflineDelivery {
                prepared_notification,
                row,
                original_message,
            },
        );
    }
}

/// Emit the XEP-0160 queue-full error after the settlement transaction ends.
pub(crate) async fn bounce_offline_quota(
    deps: &Deps<'_>,
    recipient: &BareJid,
    original_message: &Message,
) {
    waddle_xmpp::telemetry::reliability::increment_pending_delivery_quota_exceeded();
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
    let bounce =
        waddle_xmpp::protocol::handlers::errors::message_error_reply(original_message, error);
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
            let resources = match deps.user_registry {
                Some(user_registry) => {
                    waddle_xmpp::registry::get_resources_for_user(user_registry, &bare).await
                }
                None => Vec::new(),
            };
            for full in resources {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotificationCandidateQueueOutcome {
    Completed,
    Inserted,
    Duplicate,
    RetryLater,
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
    let prepared = prepare_notification_candidate_for_message(
        state,
        recipient,
        sender,
        sender_jid,
        archive_stanza_id,
        original_message,
    )
    .await;
    insert_prepared_notification(Some(state), prepared).await
}

async fn prepare_notification_candidate_for_message(
    state: &WebSocketState,
    recipient: &BareJid,
    sender: &BareJid,
    sender_jid: &Jid,
    archive_stanza_id: &waddle_xmpp_core::xep0359::StanzaId,
    original_message: &Message,
) -> PreparedOfflineNotification {
    let candidate = match crate::notification_outbox::direct_candidate_from_envelope(
        original_message,
        recipient,
        sender_jid,
        archive_stanza_id,
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
            return PreparedOfflineNotification::Suppressed;
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                sender = %sender,
                error = %error,
                "XEP-0357 notification candidate rejected"
            );
            return PreparedOfflineNotification::Suppressed;
        }
    };
    // T0 push-gate evaluation — compliance: suppressed
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
    // T0 emission does NOT consult the activity reader — XEP-0513
    // `<active/>` is a T1-only gate (current activity is a T1 read
    // per the recipient-state contract). The `NoopActivityReader` is
    // wired only to satisfy the typed signature; it would never be
    // dispatched into at T0Emit even if the candidate class were
    // `ActiveChannelMention`.
    let activity_reader = crate::notification_activity::NoopActivityReader;
    let mut activity_cache = std::collections::BTreeMap::<
        (BareJid, BareJid),
        Option<crate::notification_activity::NotificationActivity>,
    >::new();
    let eval_deps = crate::notification_outbox::PushEvalDeps {
        settings_projection: state
            .deps
            .protocol
            .notification_settings_projection
            .as_ref(),
        room_policy: &room_policy,
        dnd_reader: &dnd_reader,
        activity_reader: &activity_reader,
        // T0 emission deliberately skips the XEP-0513 `<active/>`
        // filter (current activity is a T1 read), so the TTL is never
        // consulted here. Avoid the per-call env-var read by passing
        // the default-in-ms as a typed placeholder; T1 (which DOES
        // consult the TTL) reads the env-driven value at the drain
        // site (Copilot review on PR #731).
        active_mention_ttl_ms: (crate::notification_outbox::DEFAULT_ACTIVE_MENTION_TTL_SECONDS
            as i64)
            * 1_000,
    };
    let mut eval_caches = crate::notification_outbox::PushEvalCaches {
        room_policy: &mut room_policy_cache,
        dnd: &mut dnd_cache,
        activity: &mut activity_cache,
    };
    let outcome = match crate::notification_outbox::evaluate_push_gate_at_dispatch(
        crate::notification_outbox::PushEvalStage::T0Emit,
        eval_deps,
        &candidate,
        &mut eval_caches,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            warn!(
                recipient = %recipient,
                sender = %sender,
                error = ?error,
                "push gate evaluation failed at T0; deferring offline-delivery DM candidate"
            );
            return PreparedOfflineNotification::RetryLater;
        }
    };
    match outcome {
        crate::notification_outbox::T1PushDispatchOutcome::Deliver { .. } => {}
        crate::notification_outbox::T1PushDispatchOutcome::Suppressed { reason } => {
            info!(
                recipient = %candidate.recipient_bare_jid(),
                conversation = %candidate.conversation_jid(),
                sender = %sender,
                notification_class = candidate.class().as_db_value(),
                push_stage = "suppressed",
                suppression_reason = reason.as_db_value(),
                "T0 push gate suppressed XEP-0357 DM candidate; no candidate row persisted"
            );
            waddle_xmpp::telemetry::reliability::increment_push_suppressed(
                reason.telemetry_reason(),
            );
            return PreparedOfflineNotification::Suppressed;
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
            return PreparedOfflineNotification::RetryLater;
        }
    }
    PreparedOfflineNotification::Prepared(Box::new(candidate))
}

async fn insert_prepared_notification(
    state: Option<&WebSocketState>,
    prepared: PreparedOfflineNotification,
) -> NotificationCandidateQueueOutcome {
    let candidate = match prepared {
        PreparedOfflineNotification::Prepared(candidate) => candidate,
        PreparedOfflineNotification::Suppressed => {
            return NotificationCandidateQueueOutcome::Completed;
        }
        PreparedOfflineNotification::RetryLater => {
            return NotificationCandidateQueueOutcome::RetryLater;
        }
    };
    let Some(state) = state else {
        return NotificationCandidateQueueOutcome::RetryLater;
    };
    match state
        .deps
        .protocol
        .notification_outbox
        .insert_candidate(&candidate)
        .await
    {
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Inserted) => {
            NotificationCandidateQueueOutcome::Inserted
        }
        Ok(crate::notification_outbox::NotificationCandidateInsertOutcome::Duplicate) => {
            NotificationCandidateQueueOutcome::Duplicate
        }
        Err(error) => {
            warn!(recipient = %candidate.recipient_bare_jid(), %error,
                "XEP-0357 notification candidate insert failed");
            NotificationCandidateQueueOutcome::RetryLater
        }
    }
}

async fn prepare_offline_notification(
    deps: &Deps<'_>,
    recipient: &BareJid,
    archive_stanza_id: Option<&waddle_xmpp_core::xep0359::StanzaId>,
    message: &Message,
) -> PreparedOfflineNotification {
    let Some(archive_stanza_id) = archive_stanza_id else {
        return PreparedOfflineNotification::Suppressed;
    };
    let Some(state) = deps.web_socket_state else {
        return PreparedOfflineNotification::RetryLater;
    };
    let Some(sender_jid) = message
        .from
        .as_ref()
        .filter(|sender| sender.resource().is_some())
    else {
        return PreparedOfflineNotification::Suppressed;
    };
    prepare_notification_candidate_for_message(
        state,
        recipient,
        &sender_jid.to_bare(),
        sender_jid,
        archive_stanza_id,
        message,
    )
    .await
}

async fn mark_pending_notification_outboxed(
    storage: &dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage,
    row_id: &waddle_xmpp::pending_delivery::PendingRowId,
    recipient: &BareJid,
) -> bool {
    match storage.mark_notification_outboxed(row_id).await {
        Ok(_) => true,
        Err(error) => {
            warn!(
                recipient = %recipient,
                row_id = %row_id,
                error = %error,
                "pending_delivery notification outbox marker write failed; janitor will retry"
            );
            false
        }
    }
}

#[cfg(test)]
pub(crate) async fn reconcile_xep0357_notification_candidates(
    state: &WebSocketState,
    batch_size: usize,
) -> usize {
    reconcile_xep0357_notification_candidates_for_sweep(state, batch_size)
        .await
        .completed
}

pub(crate) async fn reconcile_xep0357_notification_candidates_for_sweep(
    state: &WebSocketState,
    batch_size: usize,
) -> super::NotificationRecoverySweepOutcome {
    let batch_size = batch_size.clamp(1, 1_000);
    let pending_storage = state.deps.protocol.pending_delivery_storage.as_ref();
    let rows = match pending_storage.list_unoutboxed_archived(batch_size).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(
                error = %error,
                "XEP-0357 notification candidate recovery could not read pending_delivery rows"
            );
            return super::NotificationRecoverySweepOutcome {
                completed: 0,
                had_failure: true,
            };
        }
    };
    let mut completed = 0usize;
    let mut had_failure = false;
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
        match outcome {
            NotificationCandidateQueueOutcome::Completed
            | NotificationCandidateQueueOutcome::Inserted
            | NotificationCandidateQueueOutcome::Duplicate => {
                if !mark_pending_notification_outboxed(pending_storage, &row.id, &row.recipient)
                    .await
                {
                    had_failure = true;
                }
                completed += 1;
            }
            NotificationCandidateQueueOutcome::RetryLater => had_failure = true,
        }
    }
    super::NotificationRecoverySweepOutcome {
        completed,
        had_failure,
    }
}
